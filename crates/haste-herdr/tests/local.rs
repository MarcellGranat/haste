//! The local feed against a fake herdr unix socket server. No real herdr needed.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haste_core::herdr::AgentStatus;
use haste_herdr::backoff::Backoff;
use haste_herdr::local::{run, LocalMsg, LocalState};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, Notify};
use tokio::time::timeout;

fn snapshot_line(protocol: u32, status: &str, label: &str) -> String {
    json!({"id":"snap","result":{"type":"session_snapshot","snapshot":{
        "protocol": protocol, "version":"0.9.5", "tabs":[],
        "panes":[{"pane_id":"w1:p1","workspace_id":"w1"},{"pane_id":"w1:p2","workspace_id":"w1"}],
        "workspaces":[{"workspace_id":"w1","label":label}],
        "agents":[{"pane_id":"w1:p1","workspace_id":"w1","agent_status":status,"agent":"claude",
                   "terminal_title":"SECRET"}]}}})
    .to_string()
}

fn snapshot_with_panes(panes: &[&str]) -> String {
    let panes: Vec<Value> = panes
        .iter()
        .map(|p| json!({"pane_id": p, "workspace_id": "w1"}))
        .collect();
    json!({"id":"snap","result":{"type":"session_snapshot","snapshot":{
        "protocol": 22, "version":"0.9.5", "tabs":[], "panes": panes,
        "workspaces":[{"workspace_id":"w1","label":"[4] haste"}], "agents":[]}}})
    .to_string()
}

fn event_line(kind: &str, status: &str) -> String {
    json!({"event":kind,"data":{"pane_id":"w1:p1","workspace_id":"w1","agent_status":status,"title":"SECRET","state_labels":{"a":"SECRET"}}}).to_string()
}

#[derive(Default)]
struct Script {
    /// Responses to successive `session.snapshot` requests (the last one repeats).
    snapshots: Mutex<VecDeque<String>>,
    /// Lines written on a subscription connection right after `subscription_started`.
    after_subscribe: Mutex<Vec<String>>,
    /// Methods received, in order.
    methods: Mutex<Vec<String>>,
    /// The subscription lists received on each `events.subscribe`.
    subscriptions: Mutex<Vec<Vec<Value>>>,
    /// Signalled to drop the open subscription connection.
    drop_now: Notify,
    /// Lines to push on the open subscription connection on demand.
    push: Mutex<Vec<String>>,
    push_now: Notify,
}

impl Script {
    fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }
}

fn serve(path: PathBuf, script: Arc<Script>) {
    let listener = UnixListener::bind(&path).expect("bind");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let script = Arc::clone(&script);
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let Ok(Some(first)) = lines.next_line().await else {
                    return;
                };
                let request: Value = serde_json::from_str(&first).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                script.methods.lock().unwrap().push(method.clone());
                match method.as_str() {
                    "session.snapshot" => {
                        let line = {
                            let mut q = script.snapshots.lock().unwrap();
                            if q.len() > 1 {
                                q.pop_front().unwrap()
                            } else {
                                q.front().cloned().unwrap()
                            }
                        };
                        let _ = write.write_all((line + "\n").as_bytes()).await;
                    }
                    "events.subscribe" => {
                        // like real herdr: the status subscription is per pane and needs a pane_id
                        let subs = request["params"]["subscriptions"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        script.subscriptions.lock().unwrap().push(subs.clone());
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
                        let queued: Vec<String> =
                            script.after_subscribe.lock().unwrap().drain(..).collect();
                        for l in queued {
                            let _ = write.write_all((l + "\n").as_bytes()).await;
                        }
                        loop {
                            tokio::select! {
                                () = script.drop_now.notified() => return,
                                () = script.push_now.notified() => {
                                    let pending: Vec<String> = script.push.lock().unwrap().drain(..).collect();
                                    for l in pending { let _ = write.write_all((l + "\n").as_bytes()).await; }
                                }
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
    rx: mpsc::Receiver<LocalMsg>,
    script: Arc<Script>,
    _dir: tempfile::TempDir,
}

fn start(snapshots: Vec<String>, after_subscribe: Vec<String>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let script = Arc::new(Script::default());
    *script.snapshots.lock().unwrap() = snapshots.into();
    *script.after_subscribe.lock().unwrap() = after_subscribe;
    serve(path.clone(), Arc::clone(&script));
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run(
        path,
        tx,
        Backoff::new(Duration::from_millis(10), Duration::from_millis(40)),
        Duration::from_secs(300),
    ));
    Rig {
        rx,
        script,
        _dir: dir,
    }
}

async fn next(rig: &mut Rig) -> LocalMsg {
    timeout(Duration::from_secs(5), rig.rx.recv())
        .await
        .expect("timed out waiting for a message")
        .expect("channel open")
}

#[tokio::test]
async fn snapshot_comes_first_then_events_are_applied_in_order() {
    let mut rig = start(
        vec![snapshot_line(22, "working", "haste")],
        // an event is already waiting on the subscription before the snapshot is read
        vec![event_line("pane.agent_status_changed", "blocked")],
    );
    let LocalMsg::Snapshot(s) = next(&mut rig).await else {
        panic!("the snapshot must come first")
    };
    assert_eq!(s.workspaces[0].label, "haste");
    assert_eq!(s.agents[0].agent_status, AgentStatus::Working);
    let LocalMsg::Event(e) = next(&mut rig).await else {
        panic!("then the buffered event")
    };
    assert_eq!(e.agent_status, AgentStatus::Blocked);
    // protocol checked first, then subscribe, then the second snapshot
    assert_eq!(
        rig.script.methods(),
        ["session.snapshot", "events.subscribe", "session.snapshot"]
    );
}

#[tokio::test]
async fn the_subscription_never_asks_for_pane_output() {
    let mut rig = start(vec![snapshot_line(22, "idle", "haste")], vec![]);
    let _ = next(&mut rig).await;
    assert!(!rig
        .script
        .methods()
        .iter()
        .any(|m| m.contains("read") || m.contains("send") || m.contains("output")));
}

#[tokio::test]
async fn after_a_disconnect_a_fresh_snapshot_replaces_what_was_stale() {
    let mut rig = start(
        vec![
            snapshot_line(22, "blocked", "haste"), // protocol check
            snapshot_line(22, "blocked", "haste"), // first session
            snapshot_line(22, "working", "haste"), // after reconnect: protocol check
            snapshot_line(22, "working", "haste"), // after reconnect: session snapshot
        ],
        vec![],
    );
    let LocalMsg::Snapshot(first) = next(&mut rig).await else {
        panic!()
    };
    assert_eq!(first.agents[0].agent_status, AgentStatus::Blocked);
    rig.script
        .after_subscribe
        .lock()
        .unwrap()
        .push(event_line("pane.agent_status_changed", "idle"));
    rig.script.drop_now.notify_one();
    assert_eq!(
        next(&mut rig).await,
        LocalMsg::State(LocalState::Unreachable)
    );
    let LocalMsg::Snapshot(second) = next(&mut rig).await else {
        panic!("a snapshot follows the reconnect")
    };
    assert_eq!(
        second.agents[0].agent_status,
        AgentStatus::Working,
        "the stale blocked state is gone"
    );
}

#[tokio::test]
async fn a_protocol_mismatch_subscribes_to_nothing_and_stops() {
    let mut rig = start(vec![snapshot_line(23, "idle", "haste")], vec![]);
    assert_eq!(
        next(&mut rig).await,
        LocalMsg::State(LocalState::Mismatch { observed: 23 })
    );
    assert_eq!(
        rig.script.methods(),
        ["session.snapshot"],
        "no subscription was opened"
    );
    assert!(
        timeout(Duration::from_millis(300), rig.rx.recv())
            .await
            .map_or(true, |m| m.is_none()),
        "and it does not retry"
    );
}

#[tokio::test]
async fn a_malformed_event_is_dropped_and_the_next_one_still_applies() {
    let bad = json!({"event":"pane.agent_status_changed","data":{"pane_id":"w1:p1"}}).to_string();
    let mut rig = start(
        vec![snapshot_line(22, "working", "haste")],
        vec![bad, event_line("pane.agent_status_changed", "blocked")],
    );
    assert!(matches!(next(&mut rig).await, LocalMsg::Snapshot(_)));
    assert_eq!(next(&mut rig).await, LocalMsg::Dropped);
    assert!(
        matches!(next(&mut rig).await, LocalMsg::Event(e) if e.agent_status == AgentStatus::Blocked)
    );
}

#[tokio::test]
async fn a_lifecycle_event_triggers_a_fresh_snapshot_picking_up_a_renamed_workspace() {
    let mut rig = start(
        vec![
            snapshot_line(22, "idle", "old-name"),
            snapshot_line(22, "idle", "old-name"),
            snapshot_line(22, "idle", "new-name"),
        ],
        vec![],
    );
    let LocalMsg::Snapshot(first) = next(&mut rig).await else {
        panic!()
    };
    assert_eq!(first.workspaces[0].label, "old-name");
    rig.script
        .push
        .lock()
        .unwrap()
        .push(json!({"event":"workspace.renamed","data":{"workspace_id":"w1"}}).to_string());
    rig.script.push_now.notify_one();
    let LocalMsg::Snapshot(second) = next(&mut rig).await else {
        panic!("a rename re-reads the snapshot")
    };
    assert_eq!(second.workspaces[0].label, "new-name");
}

#[tokio::test]
async fn a_missing_socket_is_reported_unreachable_and_retried_with_bounded_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(64);
    let started = std::time::Instant::now();
    tokio::spawn(run(
        dir.path().join("nothing-here.sock"),
        tx,
        Backoff::new(Duration::from_millis(10), Duration::from_millis(40)),
        Duration::from_secs(300),
    ));
    for _ in 0..4 {
        let msg = timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg, LocalMsg::State(LocalState::Unreachable));
    }
    // four failures with a 40 ms cap cannot take anywhere near a second of backoff
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn agent_status_is_subscribed_per_pane_with_a_pane_id_each() {
    let mut rig = start(
        vec![snapshot_with_panes(&["w1:p1", "w1:p2", "w1:p3"])],
        vec![],
    );
    assert!(matches!(next(&mut rig).await, LocalMsg::Snapshot(_)));
    let subs = rig.script.subscriptions.lock().unwrap()[0].clone();
    let status: Vec<&Value> = subs
        .iter()
        .filter(|s| s["type"] == "pane.agent_status_changed")
        .collect();
    assert_eq!(status.len(), 3, "{subs:?}");
    let mut ids: Vec<&str> = status
        .iter()
        .map(|s| s["pane_id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["w1:p1", "w1:p2", "w1:p3"]);
    // plus the pane-less ones that mean "the layout changed"
    for kind in [
        "pane.created",
        "pane.closed",
        "pane.agent_detected",
        "workspace.renamed",
    ] {
        assert!(
            subs.iter()
                .any(|s| s["type"] == kind && s.get("pane_id").is_none()),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn a_new_pane_renews_the_subscriptions_so_its_status_is_watched_too() {
    let mut rig = start(
        vec![
            snapshot_with_panes(&["w1:p1"]),          // protocol check
            snapshot_with_panes(&["w1:p1"]),          // first emitted snapshot
            snapshot_with_panes(&["w1:p1", "w1:p2"]), // after the layout event
            snapshot_with_panes(&["w1:p1", "w1:p2"]), // protocol check of the renewed session
            snapshot_with_panes(&["w1:p1", "w1:p2"]), // its emitted snapshot
        ],
        vec![],
    );
    assert!(matches!(next(&mut rig).await, LocalMsg::Snapshot(s) if s.panes.len() == 1));
    rig.script
        .push
        .lock()
        .unwrap()
        .push(json!({"event":"pane.created","data":{"pane_id":"w1:p2"}}).to_string());
    rig.script.push_now.notify_one();
    // the changed layout is announced, then the feed resubscribes with both panes
    assert!(matches!(next(&mut rig).await, LocalMsg::Snapshot(s) if s.panes.len() == 2));
    assert!(matches!(next(&mut rig).await, LocalMsg::Snapshot(s) if s.panes.len() == 2));
    let all = rig.script.subscriptions.lock().unwrap().clone();
    assert_eq!(all.len(), 2, "subscribed twice");
    let watched = |subs: &Vec<Value>| {
        subs.iter()
            .filter(|s| s["type"] == "pane.agent_status_changed")
            .count()
    };
    assert_eq!((watched(&all[0]), watched(&all[1])), (1, 2));
}

#[tokio::test]
async fn a_server_that_refuses_the_subscription_ends_the_session_and_is_retried_not_hung_on() {
    // A status subscription without pane ids is what the old code sent; the fake refuses it like
    // real herdr. Here we prove the client does not wait out its timeout on an error reply.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let script = Arc::new(Script::default());
    *script.snapshots.lock().unwrap() = vec![snapshot_line(22, "idle", "haste")].into();
    serve(path.clone(), Arc::clone(&script));
    // make the fake think a pane-less status subscription arrived by deleting pane ids from the reply
    // path: simplest is to prove the error branch directly with a server that always refuses.
    let refuse = dir.path().join("refuse.sock");
    let listener = UnixListener::bind(&refuse).unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                if let Ok(Some(first)) = lines.next_line().await {
                    let request: Value = serde_json::from_str(&first).unwrap();
                    let reply = if request["method"] == "session.snapshot" {
                        let mut v: Value =
                            serde_json::from_str(&snapshot_line(22, "idle", "haste")).unwrap();
                        v["id"] = request["id"].clone();
                        v.to_string()
                    } else {
                        json!({"id":request["id"],"error":{"code":"invalid_request","message":"no"}}).to_string()
                    };
                    let _ = write.write_all((reply + "\n").as_bytes()).await;
                }
            });
        }
    });
    let (tx, mut rx) = mpsc::channel(8);
    let started = std::time::Instant::now();
    tokio::spawn(run(
        refuse,
        tx,
        Backoff::new(Duration::from_millis(10), Duration::from_millis(40)),
        Duration::from_secs(300),
    ));
    let msg = timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("did not wait for the 10 s request timeout")
        .unwrap();
    assert_eq!(msg, LocalMsg::State(LocalState::Unreachable));
    assert!(started.elapsed() < Duration::from_secs(3));
}
