//! The daemon observing herdr: a fake unix-socket server stands in for the local herdr and a fake
//! `herdr` shell script stands in for the hub's CLI (remote machines). No real herdr, no SSH.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haste_core::herdr::MachineState;
use haste_core::policy::PrincipalKind;
use haste_core::proposal::{Payload, ProposalState};
use haste_herdr::command::Runner;
use haste_proto::{
    encode_frame, parse_header, ErrorCode, Event, Hints, Request, RequestBody, Response,
};
use hasted::herdr::HerdrConfig;
use hasted::notifier::RecordingNotifier;
use hasted::scheduler::run_delivery;
use hasted::{start, DaemonConfig, DaemonHandle};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;

const SCRIPT: &str = r#"#!/bin/sh
D="$(dirname "$0")"
echo "$*" >> "$D/args.log"
case "$1" in
  machine) cat "$D/machines.json" ;;
  --machine)
    M="$2"
    if [ "$3 $4" = "api snapshot" ]; then
      [ -f "$D/fail-$M" ] && exit 1
      cat "$D/snapshot-$M.json"
    fi ;;
  agent) exit 0 ;;
esac
"#;

/// A snapshot with one workspace and one agent (and a second pane that has no agent).
fn snapshot_json(protocol: u32, label: &str, status: &str) -> String {
    json!({"id":"x","result":{"type":"session_snapshot","snapshot":{
        "protocol": protocol, "version": "0.9.5",
        "workspaces":[{"workspace_id":"w1","label":label}],
        "panes":[
            {"pane_id":"w1:p1","workspace_id":"w1","cwd":"/SECRET/path"},
            {"pane_id":"w1:p9","workspace_id":"w1","cwd":"/SECRET/other"}],
        "agents":[{"pane_id":"w1:p1","workspace_id":"w1","agent_status":status,"agent":"claude",
                   "terminal_title":"SECRET TITLE","cwd":"/SECRET/path"}]}}})
    .to_string()
}

struct FakeSocket {
    snapshot: Mutex<String>,
    push: Mutex<Vec<String>>,
    push_now: Notify,
}

fn serve_socket(path: PathBuf, fake: Arc<FakeSocket>) {
    let listener = UnixListener::bind(&path).expect("bind");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let fake = Arc::clone(&fake);
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let Ok(Some(first)) = lines.next_line().await else {
                    return;
                };
                let request: serde_json::Value = serde_json::from_str(&first).unwrap();
                match request["method"].as_str().unwrap() {
                    "session.snapshot" => {
                        // like real herdr, answer with the id of the request
                        let mut reply: serde_json::Value =
                            serde_json::from_str(&fake.snapshot.lock().unwrap()).unwrap();
                        reply["id"] = request["id"].clone();
                        let _ = write.write_all((reply.to_string() + "\n").as_bytes()).await;
                    }
                    "events.subscribe" => {
                        // like real herdr: a status subscription is per pane and needs a pane_id
                        let subs = request["params"]["subscriptions"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        if subs.iter().any(|s| {
                            s["type"] == "pane.agent_status_changed" && s.get("pane_id").is_none()
                        }) {
                            let err = json!({"id":request["id"],"error":{"code":"invalid_request","message":"missing field `pane_id`"}}).to_string() + "\n";
                            let _ = write.write_all(err.as_bytes()).await;
                            return;
                        }
                        let ack =
                            json!({"id":request["id"],"result":{"type":"subscription_started"}})
                                .to_string()
                                + "\n";
                        let _ = write.write_all(ack.as_bytes()).await;
                        loop {
                            fake.push_now.notified().await;
                            let pending: Vec<String> =
                                fake.push.lock().unwrap().drain(..).collect();
                            for l in pending {
                                let _ = write.write_all((l + "\n").as_bytes()).await;
                            }
                        }
                    }
                    _ => {}
                }
            });
        }
    });
}

struct Rig {
    dir: tempfile::TempDir,
    cli: PathBuf,
    daemon: DaemonHandle,
    notifier: Arc<RecordingNotifier>,
    socket: Arc<FakeSocket>,
    human: String,
    notices: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn write(&self, name: &str, content: &str) {
        fs::write(self.cli.join(name), content).unwrap();
    }

    fn remove(&self, name: &str) {
        let _ = fs::remove_file(self.cli.join(name));
    }

    fn cli_calls(&self) -> Vec<String> {
        fs::read_to_string(self.cli.join("args.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn audit(&self) -> String {
        fs::read_to_string(self.daemon.state.config.audit_path()).unwrap_or_default()
    }

    fn notices(&self) -> Vec<String> {
        self.notices.lock().unwrap().clone()
    }

    fn machine(&self, name: &str) -> Option<haste_proto::MachineView> {
        self.daemon
            .state
            .herdr
            .views()
            .into_iter()
            .find(|m| m.machine == name)
    }
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `machines`: JSON list of saved machines. Snapshots for `m1` (haste, `m1_status`) and `mbp`
/// (dotfiles, idle) are written before the daemon starts.
async fn boot(m1_status: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("fake-cli");
    fs::create_dir_all(&cli).unwrap();
    let bin = cli.join("herdr");
    fs::write(&bin, SCRIPT).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        cli.join("machines.json"),
        r#"[{"id":"1","label":"m1","enabled":true},{"id":"2","label":"m2","enabled":false},{"id":"3","label":"mbp","enabled":true}]"#,
    )
    .unwrap();
    fs::write(
        cli.join("snapshot-m1.json"),
        snapshot_json(22, "[2] haste", m1_status),
    )
    .unwrap();
    fs::write(
        cli.join("snapshot-mbp.json"),
        snapshot_json(22, "[1] dotfiles", "idle"),
    )
    .unwrap();

    let socket_path = dir.path().join("herdr.sock");
    let socket = Arc::new(FakeSocket {
        snapshot: Mutex::new(snapshot_json(22, "[4] haste", "working")),
        push: Mutex::new(vec![]),
        push_now: Notify::new(),
    });
    serve_socket(socket_path.clone(), Arc::clone(&socket));

    let notifier = Arc::new(RecordingNotifier::default());
    let mut config = DaemonConfig::new(&dir.path().join("haste"), notifier.clone());
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    config.delivery_backoff = Duration::ZERO;
    config.herdr = Some(HerdrConfig {
        socket: socket_path,
        runner: Runner::new(&bin),
        observe_local: true,
        poll_base: Duration::from_millis(40),
        list_refresh: Duration::from_millis(300),
        local_up: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();

    let notices = Arc::new(Mutex::new(Vec::new()));
    let mut events = daemon.state.events.subscribe();
    let sink = Arc::clone(&notices);
    tokio::spawn(async move {
        while let Ok(e) = events.recv().await {
            if let Event::Notice { message } = e {
                sink.lock().unwrap().push(message);
            }
        }
    });
    let rig = Rig {
        dir,
        cli,
        daemon,
        notifier,
        socket,
        human,
        notices,
    };
    // Wait until the local machine and both enabled remotes are connected.
    wait_for("all machines connected", || {
        ["local", "m1", "mbp"].iter().all(|m| {
            rig.machine(m)
                .is_some_and(|v| v.state == MachineState::Connected)
        })
    })
    .await;
    rig
}

async fn call(rig: &Rig, token: &str, body: RequestBody) -> Response {
    let mut stream = UnixStream::connect(&rig.daemon.socket_path).await.unwrap();
    stream
        .write_all(&encode_frame(&Request::new(token, body)).unwrap())
        .await
        .unwrap();
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let mut buf = vec![0_u8; parse_header(header).unwrap()];
    stream.read_exact(&mut buf).await.unwrap();
    serde_json::from_slice(&buf).unwrap()
}

async fn human(rig: &Rig, body: RequestBody) -> Response {
    call(rig, &rig.human, body).await
}

async fn register(rig: &Rig, id: &str, kind: PrincipalKind) -> String {
    match human(
        rig,
        RequestBody::RegisterPrincipal {
            principal_id: id.into(),
            kind,
            allowed_actions: vec!["read".into(), "draft".into()],
        },
    )
    .await
    {
        Response::Registered { token, .. } => token,
        other => panic!("{other:?}"),
    }
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { error } => Some(error.code),
        _ => None,
    }
}

fn hints(machine: &str, pane: &str) -> Option<Hints> {
    Some(Hints {
        machine: Some(machine.into()),
        pane: Some(pane.into()),
    })
}

fn draft(thread: &str, key: &str, hints: Option<Hints>) -> RequestBody {
    RequestBody::Propose {
        payload: Payload::Draft {
            attachments: Vec::new(),
            to: None,
            body: "hello".into(),
        },
        thread_id: Some(thread.to_owned()),
        idempotency_key: key.into(),
        hints,
    }
}

fn seed_threads(rig: &Rig) -> (String, String, String) {
    let s = &rig.daemon.state.store;
    let mk = |ext: &str| {
        s.upsert_thread(&haste_core::model::Source::Gmail, ext, ext, &[], 1)
            .unwrap()
            .id
    };
    (mk("t-haste"), mk("t-dotfiles"), mk("t-untagged"))
}

// ------------------------------------------------------------------ observation

#[tokio::test]
async fn local_and_enabled_remote_machines_are_observed_and_scoped_per_principal() {
    let rig = boot("working").await;
    // m2 is disabled in herdr, so it was never contacted.
    assert!(
        !rig.cli_calls().iter().any(|c| c.contains("m2")),
        "{:?}",
        rig.cli_calls()
    );
    let Response::Agents { machines } = human(&rig, RequestBody::AgentList).await else {
        panic!()
    };
    let names: Vec<&str> = machines.iter().map(|m| m.machine.as_str()).collect();
    assert_eq!(names, ["local", "m1", "mbp"]);
    let m1 = machines.iter().find(|m| m.machine == "m1").unwrap();
    assert_eq!(m1.agents[0].workspace_label, "haste");
    assert_eq!(m1.agents[0].agent.as_deref(), Some("claude"));

    // an agent sees no one's status; a machine sees only its own machine
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let Response::Agents { machines } = call(&rig, &agent, RequestBody::AgentList).await else {
        panic!()
    };
    assert!(machines.is_empty());
    let m1_token = register(&rig, "m1", PrincipalKind::Machine).await;
    let Response::Agents { machines } = call(&rig, &m1_token, RequestBody::AgentList).await else {
        panic!()
    };
    assert_eq!(
        machines
            .iter()
            .map(|m| m.machine.as_str())
            .collect::<Vec<_>>(),
        ["m1"]
    );
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn the_first_snapshot_is_silent_then_a_transition_to_blocked_is_announced_and_audited() {
    let rig = boot("blocked").await; // m1's agent is already blocked when haste starts
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !rig.audit().contains("herdr.blocked"),
        "an already-blocked agent is not announced at startup"
    );
    assert!(
        !rig.notices().iter().any(|n| n.contains("is blocked")),
        "{:?}",
        rig.notices()
    );
    assert_eq!(
        rig.daemon.state.herdr.blocked().len(),
        1,
        "but it is in the model"
    );

    rig.write(
        "snapshot-m1.json",
        &snapshot_json(22, "[4] haste", "working"),
    );
    wait_for("agent working", || {
        rig.machine("m1")
            .is_some_and(|m| m.agents[0].status == haste_core::herdr::AgentStatus::Working)
    })
    .await;
    rig.write(
        "snapshot-m1.json",
        &snapshot_json(22, "[2] haste", "blocked"),
    );
    wait_for("blocked notice", || {
        rig.notices()
            .iter()
            .any(|n| n.contains("[herdr:m1]") && n.contains("is blocked"))
    })
    .await;
    let audit = rig.audit();
    assert!(
        audit.contains(r#""action":"herdr.blocked","outcome":"blocked","object":"m1""#),
        "{audit}"
    );
    assert!(audit.contains("workspace=w1 pane=w1:p1 blocked_now="));
    // counts and identifiers only
    for secret in ["SECRET", "/SECRET/path"] {
        assert!(
            !audit.contains(secret) && !rig.notices().iter().any(|n| n.contains(secret)),
            "{secret} leaked"
        );
    }
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_local_status_event_updates_the_model_and_raises_one_notice() {
    let rig = boot("idle").await;
    rig.socket.push.lock().unwrap().push(
        json!({"event":"pane.agent_status_changed","data":{"pane_id":"w1:p1","workspace_id":"w1","agent_status":"blocked","agent":"claude","title":"SECRET TITLE"}}).to_string(),
    );
    rig.socket.push_now.notify_one();
    wait_for("local blocked", || {
        rig.machine("local").is_some_and(|m| {
            m.agents
                .iter()
                .any(|a| a.status == haste_core::herdr::AgentStatus::Blocked)
        })
    })
    .await;
    wait_for("notice", || {
        rig.notices()
            .iter()
            .any(|n| n.contains("[herdr:local]") && n.contains("is blocked"))
    })
    .await;
    assert!(!rig.audit().contains("SECRET"));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn an_unreachable_machine_shows_no_rows_is_reported_once_and_recovers_with_a_silent_baseline()
{
    let rig = boot("working").await;
    rig.write("fail-m1", "");
    wait_for("m1 unreachable", || {
        rig.machine("m1")
            .is_some_and(|m| m.state == MachineState::Unreachable)
    })
    .await;
    assert!(
        rig.machine("m1").unwrap().agents.is_empty(),
        "no stale rows are shown as current"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        rig.audit()
            .matches(r#""action":"herdr.link","outcome":"unreachable","object":"m1""#)
            .count(),
        1,
        "reported once, not per attempt"
    );

    rig.remove("fail-m1");
    rig.write(
        "snapshot-m1.json",
        &snapshot_json(22, "[2] haste", "blocked"),
    );
    wait_for("m1 connected again", || {
        rig.machine("m1")
            .is_some_and(|m| m.state == MachineState::Connected)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !rig.notices()
            .iter()
            .any(|n| n.contains("[herdr:m1]") && n.contains("is blocked")),
        "recovery is a baseline: nothing is announced"
    );
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_protocol_mismatch_observes_nothing_and_raises_one_notice_naming_both_numbers() {
    let rig = boot("working").await;
    rig.write(
        "snapshot-m1.json",
        &snapshot_json(23, "[2] haste", "working"),
    );
    wait_for("m1 mismatch", || {
        rig.machine("m1")
            .is_some_and(|m| matches!(m.state, MachineState::Mismatch { .. }))
    })
    .await;
    assert_eq!(
        rig.machine("m1").unwrap().state,
        MachineState::Mismatch {
            expected: 22,
            observed: 23
        }
    );
    assert!(rig.machine("m1").unwrap().agents.is_empty());
    let hits: Vec<String> = rig
        .notices()
        .into_iter()
        .filter(|n| n.contains("protocol mismatch"))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("expected 22") && hits[0].contains("observed 23"));
    assert!(rig.audit().contains("expected=22 observed=23"));
    rig.daemon.shutdown().await;
}

// ------------------------------------------------------------------ projects

#[tokio::test]
async fn a_proposal_carries_the_workspace_of_the_agents_pane_as_its_project() {
    let rig = boot("working").await;
    let (t, _, _) = seed_threads(&rig);
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;

    // an agent in the local hub's workspace "haste"
    let Response::Proposal { proposal } =
        call(&rig, &agent, draft(&t, "k1", hints("local", "w1:p1"))).await
    else {
        panic!()
    };
    assert_eq!(
        (
            proposal.project.as_deref(),
            proposal.origin_machine.as_deref()
        ),
        (Some("haste"), Some("local"))
    );
    // a pane with no detected agent still resolves to its workspace
    let Response::Proposal { proposal } =
        call(&rig, &agent, draft(&t, "k2", hints("local", "w1:p9"))).await
    else {
        panic!()
    };
    assert_eq!(proposal.project.as_deref(), Some("haste"));
    // an agent on another machine: that machine's workspace
    let Response::Proposal { proposal } =
        call(&rig, &agent, draft(&t, "k3", hints("mbp", "w1:p1"))).await
    else {
        panic!()
    };
    assert_eq!(
        (
            proposal.project.as_deref(),
            proposal.origin_machine.as_deref()
        ),
        (Some("dotfiles"), Some("mbp"))
    );
    // unknown pane, unknown machine, no hints: created normally, with no project
    for (key, h) in [
        ("k4", hints("local", "w9:p9")),
        ("k5", hints("nowhere", "w1:p1")),
        ("k6", None),
    ] {
        let Response::Proposal { proposal } = call(&rig, &agent, draft(&t, key, h)).await else {
            panic!("{key}")
        };
        assert_eq!(
            (proposal.project, proposal.origin_machine),
            (None, None),
            "{key}"
        );
        assert_eq!(proposal.state, ProposalState::Pending);
    }
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn hints_never_change_who_the_caller_is_or_what_it_may_do() {
    let rig = boot("working").await;
    let (t, _, _) = seed_threads(&rig);
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    // claims to be in a workspace that exists, on another machine
    let Response::Proposal { proposal } =
        call(&rig, &agent, draft(&t, "k", hints("m1", "w1:p1"))).await
    else {
        panic!()
    };
    assert_eq!(
        proposal.principal_id, "agent-a",
        "identity comes from the token"
    );
    // hints grant nothing: approving is still human-only
    let r = call(
        &rig,
        &agent,
        RequestBody::Approve {
            proposal_id: proposal.id.clone(),
            acknowledge_stale: true,
        },
    )
    .await;
    assert_eq!(code(&r), Some(ErrorCode::Denied));
    rig.daemon.shutdown().await;
}

// ------------------------------------------------------------------ tags and machine scope

#[tokio::test]
async fn tags_are_workspace_labels_set_only_by_the_human_and_scope_a_machines_reads() {
    let rig = boot("working").await;
    let (t_haste, t_dot, t_none) = seed_threads(&rig);

    // known tags: the labels of every machine (haste on local and m1, dotfiles on mbp)
    let Response::Tags { tags } = human(&rig, RequestBody::TagList).await else {
        panic!()
    };
    assert_eq!(tags, ["dotfiles", "haste"]);
    assert!(matches!(
        human(
            &rig,
            RequestBody::TagAdd {
                thread_id: t_haste.clone(),
                tag: "haste".into()
            }
        )
        .await,
        Response::Done
    ));
    assert!(matches!(
        human(
            &rig,
            RequestBody::TagAdd {
                thread_id: t_dot.clone(),
                tag: "dotfiles".into()
            }
        )
        .await,
        Response::Done
    ));
    let typo = human(
        &rig,
        RequestBody::TagAdd {
            thread_id: t_haste.clone(),
            tag: "bad\u{7}".into(),
        },
    )
    .await;
    assert_eq!(
        code(&typo),
        Some(ErrorCode::Validation),
        "an invalid label is refused"
    );
    assert!(
        rig.audit()
            .contains(r#""action":"tag.change","outcome":"refused""#)
            && rig.audit().contains("invalid_tag")
    );

    // agents and machines cannot change tags, and the denial names the request
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let machine = register(&rig, "m1", PrincipalKind::Machine).await;
    for token in [&agent, &machine] {
        let r = call(
            &rig,
            token,
            RequestBody::TagAdd {
                thread_id: t_none.clone(),
                tag: "haste".into(),
            },
        )
        .await;
        assert_eq!(code(&r), Some(ErrorCode::Denied));
    }
    assert!(rig.audit().contains("human_only_action:tag_add"));
    assert!(rig
        .daemon
        .state
        .store
        .thread_tags(&t_none)
        .unwrap()
        .is_empty());

    let listed = |token: String| {
        let rig = &rig;
        async move {
            let Response::Threads { threads } = call(
                rig,
                &token,
                RequestBody::ThreadList {
                    sources: vec![],
                    include_archived: false,
                    include_snoozed: false,
                },
            )
            .await
            else {
                panic!()
            };
            let mut ids: Vec<String> = threads.into_iter().map(|v| v.thread.external_id).collect();
            ids.sort();
            ids
        }
    };
    // the human and agents see all three, as before
    assert_eq!(
        listed(rig.human.clone()).await,
        ["t-dotfiles", "t-haste", "t-untagged"]
    );
    assert_eq!(
        listed(agent.clone()).await,
        ["t-dotfiles", "t-haste", "t-untagged"]
    );
    // machine m1 has the workspace "haste": it sees that project's thread only
    assert_eq!(listed(machine.clone()).await, ["t-haste"]);
    // a hidden thread looks like a missing one
    let r = call(
        &rig,
        &machine,
        RequestBody::ThreadShow {
            thread_id: t_dot.clone(),
        },
    )
    .await;
    assert_eq!(code(&r), Some(ErrorCode::NotFound));
    // a machine principal that matches no herdr machine sees nothing, and it is not an error
    let ghost = register(&rig, "ghost", PrincipalKind::Machine).await;
    assert!(listed(ghost).await.is_empty());
    // tag changes are audited with thread id and tag
    assert!(rig.audit().contains(&format!(
        r#""action":"tag.change","outcome":"added","object":"{t_haste}","detail":"tag=haste""#
    )));

    // removing the tag hides the thread from m1 again
    assert!(matches!(
        human(
            &rig,
            RequestBody::TagRemove {
                thread_id: t_haste,
                tag: "haste".into()
            }
        )
        .await,
        Response::Done
    ));
    assert!(listed(machine).await.is_empty());
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_machines_scope_follows_its_last_known_workspaces_even_while_it_is_unreachable() {
    let rig = boot("working").await;
    let (t_haste, _, _) = seed_threads(&rig);
    human(
        &rig,
        RequestBody::TagAdd {
            thread_id: t_haste,
            tag: "haste".into(),
        },
    )
    .await;
    let machine = register(&rig, "m1", PrincipalKind::Machine).await;
    rig.write("fail-m1", "");
    wait_for("m1 unreachable", || {
        rig.machine("m1")
            .is_some_and(|m| m.state == MachineState::Unreachable)
    })
    .await;
    let Response::Threads { threads } = call(
        &rig,
        &machine,
        RequestBody::ThreadList {
            sources: vec![],
            include_archived: false,
            include_snoozed: false,
        },
    )
    .await
    else {
        panic!()
    };
    assert_eq!(
        threads.len(),
        1,
        "the labels last seen on m1 still define its scope"
    );
    rig.daemon.shutdown().await;
}

// ------------------------------------------------------------------ result delivery

#[tokio::test]
async fn results_go_to_the_machine_the_agent_runs_on() {
    let rig = boot("working").await;
    let (t, t2, _) = seed_threads(&rig); // different threads, or the second proposal supersedes the first
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let local_id = match call(&rig, &agent, draft(&t, "k1", hints("local", "w1:p1"))).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    let remote_id = match call(&rig, &agent, draft(&t2, "k2", hints("m1", "w1:p1"))).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    for id in [&local_id, &remote_id] {
        human(
            &rig,
            RequestBody::Reject {
                proposal_id: id.clone(),
                reason: Some("no".into()),
            },
        )
        .await;
    }
    let mut last = std::collections::HashMap::new();
    run_delivery(&rig.daemon.state, &mut last).await;
    let local = rig.notifier.sent.lock().unwrap().clone();
    let remote = rig.notifier.remote_sent.lock().unwrap().clone();
    assert_eq!(local.len(), 1, "{local:?}");
    assert!(local[0].1.contains(&local_id));
    assert_eq!(remote.len(), 1, "{remote:?}");
    assert_eq!(
        (remote[0].0.as_str(), remote[0].1.as_str()),
        ("m1", "agent-a")
    );
    assert!(remote[0].2.contains(&remote_id));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_principal_that_is_not_a_valid_herdr_name_gets_no_prompt_and_no_retries() {
    let rig = boot("working").await;
    let (t, _, _) = seed_threads(&rig);
    let agent = register(&rig, "Agent X", PrincipalKind::Agent).await; // not [a-z][a-z0-9_-]{0,31}
    let id = match call(&rig, &agent, draft(&t, "k", None)).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    human(
        &rig,
        RequestBody::Reject {
            proposal_id: id.clone(),
            reason: None,
        },
    )
    .await;
    let mut last = std::collections::HashMap::new();
    for _ in 0..3 {
        run_delivery(&rig.daemon.state, &mut last).await;
    }
    assert!(
        rig.notifier.sent.lock().unwrap().is_empty()
            && rig.notifier.remote_sent.lock().unwrap().is_empty()
    );
    assert_eq!(
        *rig.notifier.attempts.lock().unwrap(),
        0,
        "nothing was attempted"
    );
    assert_eq!(
        rig.audit()
            .matches(r#""action":"herdr.prompt","outcome":"refused_invalid_name""#)
            .count(),
        1,
        "refused once, not retried"
    );
    // the result is still there
    let status = call(
        &rig,
        &agent,
        RequestBody::ProposalStatus { proposal_id: id },
    )
    .await;
    assert!(
        matches!(&status, Response::Proposal { proposal } if proposal.state == ProposalState::Rejected)
    );
    rig.daemon.shutdown().await;
    let _: &Path = rig.dir.path();
}

// ------------------------------------------------------------------ scope guard rails

/// How a request relates to the scope filter. This `match` has no wildcard on purpose: adding a
/// request type to the protocol stops this test from compiling until someone decides which kind it
/// is, so a new read cannot slip past the filter unnoticed.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    /// Returns threads, proposals or agent status: must be scoped per principal.
    ScopedRead,
    /// Changes state or is human-only administration: covered by the policy gate tests.
    NotARead,
}

fn kind(body: &RequestBody) -> Kind {
    match body {
        RequestBody::ThreadList { .. }
        | RequestBody::ThreadShow { .. }
        | RequestBody::ProposalStatus { .. }
        | RequestBody::ProposalList { .. }
        | RequestBody::AgentList
        | RequestBody::RemindersList { .. }
        | RequestBody::EventsList { .. } => Kind::ScopedRead,
        RequestBody::Propose { .. }
        | RequestBody::NoteAppend { .. }
        | RequestBody::NotePerson { .. }
        | RequestBody::NoteReference(_)
        | RequestBody::NoteMode { .. }
        | RequestBody::SendPreview { .. }
        | RequestBody::NoteShow { .. }
        | RequestBody::NoteLog { .. }
        | RequestBody::Approve { .. }
        | RequestBody::Reject { .. }
        | RequestBody::Send { .. }
        | RequestBody::RegisterPrincipal { .. }
        | RequestBody::RevokeToken { .. }
        | RequestBody::AuditRecent { .. }
        | RequestBody::ConnectorStatus
        | RequestBody::ConnectorRelease { .. }
        | RequestBody::ConnectorEngage { .. }
        | RequestBody::ConnectorResync { .. }
        | RequestBody::RemindersAuth
        | RequestBody::AttachmentSave { .. }
        | RequestBody::ReactionAdd { .. }
        | RequestBody::ConnectorAuthorized { .. }
        | RequestBody::TagAdd { .. }
        | RequestBody::TagRemove { .. }
        | RequestBody::TagList
        | RequestBody::ThreadRefresh { .. }
        | RequestBody::ReminderNew { .. }
        | RequestBody::ReminderEdit { .. }
        | RequestBody::ReminderTagAdd { .. }
        | RequestBody::ReminderTagRemove { .. }
        | RequestBody::EventTagAdd { .. }
        | RequestBody::EventTagRemove { .. }
        | RequestBody::RemindersStatus
        | RequestBody::TriageSet { .. }
        | RequestBody::FilterList
        | RequestBody::IgnoreList
        | RequestBody::IgnoreMute { .. }
        | RequestBody::ReadSet { .. }
        | RequestBody::SenderRules
        | RequestBody::EventShift { .. }
        | RequestBody::EventCreateLocal { .. }
        | RequestBody::EventDeleteLocal { .. }
        | RequestBody::ReminderDuration { .. }
        | RequestBody::ReminderLater { .. }
        | RequestBody::ScheduleSend { .. }
        | RequestBody::ScheduledList
        | RequestBody::ReminderPriority { .. }
        | RequestBody::ReminderEarly { .. }
        | RequestBody::MutedDelete { .. }
        | RequestBody::ThreadDelete { .. }
        | RequestBody::RuleChange { .. }
        | RequestBody::Execute { .. }
        | RequestBody::ResolveExecuting { .. }
        | RequestBody::Subscribe { .. } => Kind::NotARead,
    }
}

#[tokio::test]
async fn every_scoped_read_returns_only_what_each_kind_of_principal_may_see() {
    let rig = boot("working").await;
    let (t_haste, t_dot, _) = seed_threads(&rig);
    human(
        &rig,
        RequestBody::TagAdd {
            thread_id: t_haste.clone(),
            tag: "haste".into(),
        },
    )
    .await;
    human(
        &rig,
        RequestBody::TagAdd {
            thread_id: t_dot.clone(),
            tag: "dotfiles".into(),
        },
    )
    .await;
    // three reminders: tagged haste, tagged dotfiles, untagged
    let wire = |ext: &str| haste_core::model::Reminder {
        id: String::new(),
        external_id: ext.into(),
        title: format!("title {ext}"),
        notes_snippet: String::new(),
        due_at: None,
        completed: false,
        list_name: "Inbox".into(),
        remote_modified_at: 1,
        missing_since: None,
        version: 0,
    };
    rig.daemon
        .state
        .store
        .upsert_reminders(&[wire("r1"), wire("r2"), wire("r3")], None)
        .unwrap();
    let mut by_ext = std::collections::HashMap::new();
    for r in rig.daemon.state.store.list_reminders(true).unwrap() {
        by_ext.insert(r.external_id.clone(), r.id);
    }
    for (ext, tag) in [("r1", "haste"), ("r2", "dotfiles")] {
        human(
            &rig,
            RequestBody::ReminderTagAdd {
                reminder_id: by_ext[ext].clone(),
                tag: tag.into(),
            },
        )
        .await;
    }
    // three events: tagged haste, tagged dotfiles, untagged
    let event = |ext: &str| haste_core::model::Event {
        id: String::new(),
        external_id: format!("primary/{ext}"),
        calendar: "primary".into(),
        title: format!("event {ext}"),
        start_at: 4_000_000_000,
        end_at: 4_000_003_600,
        all_day: false,
        status: "confirmed".into(),
        attendees: vec![],
        location: String::new(),
        description_snippet: String::new(),
        updated_at: 1,
        missing_since: None,
        version: 0,
    };
    rig.daemon
        .state
        .store
        .upsert_events(&[event("e1"), event("e2"), event("e3")], &[])
        .unwrap();
    let mut event_by_ext = std::collections::HashMap::new();
    for e in rig.daemon.state.store.list_events(0).unwrap() {
        event_by_ext.insert(e.external_id.clone(), e.id);
    }
    for (ext, tag) in [("primary/e1", "haste"), ("primary/e2", "dotfiles")] {
        human(
            &rig,
            RequestBody::EventTagAdd {
                event_id: event_by_ext[ext].clone(),
                tag: tag.into(),
            },
        )
        .await;
    }
    let owner = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let other = register(&rig, "agent-b", PrincipalKind::Agent).await;
    let machine = register(&rig, "m1", PrincipalKind::Machine).await;
    let ghost = register(&rig, "ghost", PrincipalKind::Machine).await;
    let Response::Proposal { proposal } =
        call(&rig, &owner, draft(&t_haste, "k", hints("local", "w1:p1"))).await
    else {
        panic!()
    };

    let reads = [
        RequestBody::ThreadList {
            sources: vec![],
            include_archived: false,
            include_snoozed: false,
        },
        RequestBody::ThreadShow {
            thread_id: t_dot.clone(),
        },
        RequestBody::ProposalStatus {
            proposal_id: proposal.id.clone(),
        },
        RequestBody::ProposalList { states: vec![] },
        RequestBody::AgentList,
        RequestBody::RemindersList {
            include_completed: true,
        },
        RequestBody::EventsList { from: 0 },
    ];
    assert!(
        reads.iter().all(|r| kind(r) == Kind::ScopedRead),
        "the list below must be exactly the scoped reads"
    );

    // (human, owner agent, other agent, machine m1, unknown machine) -> visible item counts
    let count = |r: &Response| -> usize {
        match r {
            Response::Threads { threads } => threads.len(),
            Response::Thread { .. } => 1,
            Response::Proposal { .. } => 1,
            Response::Proposals { proposals } => proposals.len(),
            Response::Agents { machines } => machines.iter().map(|m| m.agents.len()).sum(),
            Response::Reminders { reminders } => reminders.len(),
            Response::Events { events } => events.len(),
            Response::Error { .. } => 0,
            other => panic!("{other:?}"),
        }
    };
    let tokens = [rig.human.clone(), owner, other, machine, ghost];
    let expected: [[usize; 5]; 7] = [
        //        human owner other m1 ghost
        /* ThreadList     */ [3, 3, 3, 1, 0],
        /* ThreadShow dot */ [1, 1, 1, 0, 0],
        /* ProposalStatus */ [1, 1, 0, 0, 0],
        /* ProposalList   */ [1, 1, 0, 0, 0],
        /* AgentList      */ [3, 0, 0, 1, 0],
        /* RemindersList  */ [3, 3, 3, 1, 0],
        /* EventsList      */ [3, 3, 3, 1, 0],
    ];
    for (row, read) in reads.iter().enumerate() {
        for (col, token) in tokens.iter().enumerate() {
            let got = count(&call(&rig, token, read.clone()).await);
            assert_eq!(got, expected[row][col], "{read:?} as principal #{col}");
        }
    }
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_matching_tag_does_not_let_a_machine_do_anything_it_could_not_before() {
    let rig = boot("working").await;
    let (t_haste, _, _) = seed_threads(&rig);
    human(
        &rig,
        RequestBody::TagAdd {
            thread_id: t_haste.clone(),
            tag: "haste".into(),
        },
    )
    .await;
    let machine = register(&rig, "m1", PrincipalKind::Machine).await;
    // it can see the tagged thread ...
    let shown = call(
        &rig,
        &machine,
        RequestBody::ThreadShow {
            thread_id: t_haste.clone(),
        },
    )
    .await;
    assert!(matches!(shown, Response::Thread { .. }));
    // ... but drafting on it is still refused by the gate, and so is every human-only action
    assert_eq!(
        code(&call(&rig, &machine, draft(&t_haste, "k", None)).await),
        Some(ErrorCode::Denied)
    );
    for body in [
        RequestBody::TagAdd {
            thread_id: t_haste.clone(),
            tag: "haste".into(),
        },
        RequestBody::Approve {
            proposal_id: "x".into(),
            acknowledge_stale: true,
        },
        RequestBody::Send {
            proposal_id: "x".into(),
            acknowledge_stale: true,
            recipients_digest: "x".into(),
        },
        RequestBody::SendPreview {
            proposal_id: "x".into(),
        },
    ] {
        assert_eq!(
            code(&call(&rig, &machine, body).await),
            Some(ErrorCode::Denied)
        );
    }
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn only_the_human_can_tag_a_reminder_and_unknown_tags_are_refused() {
    let rig = boot("working").await;
    rig.daemon
        .state
        .store
        .upsert_reminders(
            &[haste_core::model::Reminder {
                id: String::new(),
                external_id: "r1".into(),
                title: "t".into(),
                notes_snippet: String::new(),
                due_at: None,
                completed: false,
                list_name: String::new(),
                remote_modified_at: 1,
                missing_since: None,
                version: 0,
            }],
            None,
        )
        .unwrap();
    let id = rig.daemon.state.store.list_reminders(true).unwrap()[0]
        .id
        .clone();
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let machine = register(&rig, "m1", PrincipalKind::Machine).await;
    for token in [&agent, &machine] {
        for body in [
            RequestBody::ReminderTagAdd {
                reminder_id: id.clone(),
                tag: "haste".into(),
            },
            RequestBody::ReminderTagRemove {
                reminder_id: id.clone(),
                tag: "haste".into(),
            },
        ] {
            assert_eq!(
                code(&call(&rig, token, body).await),
                Some(ErrorCode::Denied)
            );
        }
    }
    let unknown = human(
        &rig,
        RequestBody::ReminderTagAdd {
            reminder_id: id.clone(),
            tag: "nope".into(),
        },
    )
    .await;
    assert_eq!(code(&unknown), Some(ErrorCode::Validation));
    assert!(rig
        .daemon
        .state
        .store
        .reminder_tags(&id)
        .unwrap()
        .is_empty());
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn triage_writes_are_human_only_local_and_leave_content_alone() {
    let rig = boot("working").await;
    let (t_haste, _, _) = seed_threads(&rig);
    let list = |all: bool| RequestBody::ThreadList {
        sources: vec![],
        include_archived: all,
        include_snoozed: all,
    };
    let Response::Threads { threads: before } = human(&rig, list(true)).await else {
        panic!()
    };
    let subject = before
        .iter()
        .find(|t| t.thread.id == t_haste)
        .unwrap()
        .thread
        .subject
        .clone();

    let agent = register(&rig, "agent-t", PrincipalKind::Agent).await;
    let denied = call(
        &rig,
        &agent,
        RequestBody::TriageSet {
            thread_id: t_haste.clone(),
            priority: None,
            set_aside: None,
            snoozed_until: None,
            archived: Some(true),
        },
    )
    .await;
    assert_eq!(code(&denied), Some(ErrorCode::Denied));
    assert!(rig.audit().contains("human_only_action:triage_set"));

    let done = human(
        &rig,
        RequestBody::TriageSet {
            thread_id: t_haste.clone(),
            priority: Some(2),
            set_aside: Some(true),
            snoozed_until: None,
            archived: Some(true),
        },
    )
    .await;
    assert!(matches!(done, Response::Done));
    let Response::Threads { threads: visible } = human(&rig, list(false)).await else {
        panic!()
    };
    assert!(
        visible.iter().all(|t| t.thread.id != t_haste),
        "archived is hidden"
    );
    let Response::Threads { threads: all } = human(&rig, list(true)).await else {
        panic!()
    };
    let t = all.iter().find(|t| t.thread.id == t_haste).unwrap();
    assert_eq!(t.thread.subject, subject, "content is untouched");
    assert_eq!((t.triage.priority, t.triage.set_aside), (2, true));
    assert!(rig.audit().contains("fields=priority,set_aside,archive"));

    let bad = human(
        &rig,
        RequestBody::TriageSet {
            thread_id: t_haste,
            priority: Some(99),
            set_aside: None,
            snoozed_until: None,
            archived: None,
        },
    )
    .await;
    assert_eq!(code(&bad), Some(ErrorCode::Validation));
    let Response::Filters { filters, error } = human(&rig, RequestBody::FilterList).await else {
        panic!()
    };
    assert_eq!((filters.len(), error), (1, None));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn mail_senders_stay_blocked_until_accepted_and_reading_does_not_accept() {
    let rig = boot("working").await;
    let store = &rig.daemon.state.store;
    let mail = store
        .upsert_thread(
            &haste_core::model::Source::Gmail,
            "g-1",
            "Offer",
            &["shop@y.org".into()],
            10,
        )
        .unwrap();
    let chat = store
        .upsert_thread(
            &haste_core::model::Source::Whatsapp,
            "c-1",
            "Friends",
            &[],
            11,
        )
        .unwrap();
    store
        .upsert_message(&haste_core::model::Message {
            id: String::new(),
            thread_id: mail.id.clone(),
            source: haste_core::model::Source::Gmail,
            external_id: "m-1".into(),
            sender_person_id: None,
            sender_identifier: Some("Shop <shop@y.org>".into()),
            body_text: "hi".into(),
            sent_at: 10,
            is_own: false,
        })
        .unwrap();
    store.set_unread(&[(mail.id.clone(), true)], 1).unwrap();
    let list = || RequestBody::ThreadList {
        sources: vec![],
        include_archived: false,
        include_snoozed: true,
    };
    let view = |resp: Response, id: &str| -> (bool, bool, Option<String>) {
        let Response::Threads { threads } = resp else {
            panic!()
        };
        let v = threads.iter().find(|v| v.thread.id == id).unwrap();
        (v.unread, v.sender_known, v.sender.clone())
    };
    // mail is not accepted; a chat never needs it; the sender is the one the rules match
    let (unread, known, sender) = view(human(&rig, list()).await, &mail.id);
    assert!(unread && !known);
    assert_eq!(sender.as_deref(), Some("Shop <shop@y.org>"));
    assert!(view(human(&rig, list()).await, &chat.id).1);
    // reading it, and marking it unread again, accepts nobody
    for read in [true, false] {
        human(
            &rig,
            RequestBody::ReadSet {
                thread_id: mail.id.clone(),
                read,
            },
        )
        .await;
        assert!(!view(human(&rig, list()).await, &mail.id).1);
    }
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn sender_rules_are_local_files_changed_only_by_the_human() {
    use haste_proto::RuleKind;
    let rig = boot("working").await;
    let chat = rig
        .daemon
        .state
        .store
        .upsert_thread(
            &haste_core::model::Source::Whatsapp,
            "chat-2",
            "Team (ops)",
            &[],
            1,
        )
        .unwrap()
        .id;
    let change = |add: bool, rule: RuleKind| RequestBody::RuleChange {
        thread_id: chat.clone(),
        rule,
        add,
        domain: false,
    };
    let agent = register(&rig, "agent-r", PrincipalKind::Agent).await;
    let denied = call(&rig, &agent, change(true, RuleKind::Priority)).await;
    assert_eq!(code(&denied), Some(ErrorCode::Denied));
    let denied = call(
        &rig,
        &agent,
        RequestBody::ReadSet {
            thread_id: chat.clone(),
            read: true,
        },
    )
    .await;
    assert_eq!(code(&denied), Some(ErrorCode::Denied));

    assert!(matches!(
        human(&rig, change(true, RuleKind::Priority)).await,
        Response::Done
    ));
    human(&rig, change(true, RuleKind::Priority)).await; // no duplicate line
    human(&rig, change(true, RuleKind::Allowed)).await;
    let Response::Rules {
        mute,
        priority,
        allowed,
        errors,
    } = human(&rig, RequestBody::SenderRules).await
    else {
        panic!()
    };
    assert_eq!(priority, vec!["^Team \\(ops\\)$".to_owned()]);
    assert_eq!(allowed, priority);
    assert!(mute.is_empty() && errors.is_empty());
    // taking it off removes every line that matches this sender, and nothing else
    human(&rig, change(false, RuleKind::Priority)).await;
    let Response::Rules {
        priority, allowed, ..
    } = human(&rig, RequestBody::SenderRules).await
    else {
        panic!()
    };
    assert!(priority.is_empty());
    assert_eq!(allowed.len(), 1);
    // a chat has no read mark
    let refused = human(
        &rig,
        RequestBody::ReadSet {
            thread_id: chat,
            read: true,
        },
    )
    .await;
    assert_eq!(code(&refused), Some(ErrorCode::Validation));
    assert!(rig.audit().contains("rule.add"));
    assert!(!rig.audit().contains("Team"));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn muting_is_human_only_local_and_never_mutes_the_human() {
    use std::os::unix::fs::PermissionsExt;
    let rig = boot("working").await;
    // a chat is muted by its name
    let chat = rig
        .daemon
        .state
        .store
        .upsert_thread(
            &haste_core::model::Source::Whatsapp,
            "chat-1",
            "Noisy Group (v2)",
            &[],
            1,
        )
        .unwrap()
        .id;
    // mail: the daemon does not know the human's own address here, so it refuses to guess
    let (mail, _, _) = seed_threads(&rig);

    let agent = register(&rig, "agent-m", PrincipalKind::Agent).await;
    let denied = call(
        &rig,
        &agent,
        RequestBody::IgnoreMute {
            thread_id: chat.clone(),
        },
    )
    .await;
    assert_eq!(code(&denied), Some(ErrorCode::Denied));
    assert!(rig.audit().contains("human_only_action:ignore_mute"));

    let refused = human(&rig, RequestBody::IgnoreMute { thread_id: mail }).await;
    assert_eq!(code(&refused), Some(ErrorCode::Validation));

    let Response::Muted { pattern, .. } = human(
        &rig,
        RequestBody::IgnoreMute {
            thread_id: chat.clone(),
        },
    )
    .await
    else {
        panic!()
    };
    assert_eq!(pattern, "^Noisy Group \\(v2\\)$", "escaped, exact");
    // again: no duplicate line
    human(&rig, RequestBody::IgnoreMute { thread_id: chat }).await;
    let Response::Ignore { patterns, errors } = human(&rig, RequestBody::IgnoreList).await else {
        panic!()
    };
    assert_eq!((patterns, errors), (vec![pattern], vec![]));
    let file = rig.daemon.state.config.data_dir.join(".haste_ignore");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // the audit says that it happened, never who or what was muted
    assert!(rig.audit().contains("ignore.add"));
    assert!(!rig.audit().contains("Noisy"));
    rig.daemon.shutdown().await;
}
