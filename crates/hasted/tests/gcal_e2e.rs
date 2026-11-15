//! Daemon + Google Calendar connector against a fake Google server: engaged on first install,
//! sync, approval-only writes with `sendUpdates=none`, stale updates and ambiguous outcomes.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haste_connector_gcal::{ClientCredentials, CycleAudit, GcalConfig, GcalConnector};
use haste_core::connector::{CallError, Connector};
use haste_core::policy::PrincipalKind;
use haste_core::proposal::{EventChanges, Payload, ProposalState};
use haste_google_oauth::keychain::{MemoryStore, TokenStore};
use haste_google_oauth::testing::{spawn, FakeServer};
use haste_proto::{encode_frame, parse_header, ErrorCode, Request, RequestBody, Response};
use hasted::notifier::RecordingNotifier;
use hasted::scheduler::sync_once;
use hasted::{start, DaemonConfig, DaemonHandle};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const NOW: i64 = 1_794_218_400; // 2026-11-09T10:00:00Z

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    Normal,
    ServerError,
    Rejected,
}

struct Remote {
    title: Mutex<String>,
    mode: Mutex<WriteMode>,
}

struct Rig {
    _dir: tempfile::TempDir,
    daemon: DaemonHandle,
    server: FakeServer,
    remote: Arc<Remote>,
    connector: Arc<dyn Connector>,
    human: String,
}

fn event_json(id: &str, title: &str) -> serde_json::Value {
    json!({"id":id,"status":"confirmed","summary":title,"updated":"2026-11-08T00:00:00Z",
           "start":{"dateTime":"2026-11-10T09:00:00Z"},"end":{"dateTime":"2026-11-10T10:00:00Z"},
           "description":"SECRET DESCRIPTION","location":"SECRET PLACE",
           "attendees":[{"email":"secret.person@example.org"}]})
}

async fn boot() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut config = DaemonConfig::new(
        &dir.path().join("haste"),
        Arc::new(RecordingNotifier::default()),
    );
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();
    let remote = Arc::new(Remote {
        title: Mutex::new("Standup".into()),
        mode: Mutex::new(WriteMode::Normal),
    });
    let r = Arc::clone(&remote);
    let server = spawn(move |req| {
        if req.path == "/token" {
            return (
                200,
                json!({"access_token":"at","expires_in":3600}).to_string(),
            );
        }
        let mode = *r.mode.lock().unwrap();
        match req.method.as_str() {
            "GET" => (
                200,
                json!({"items":[event_json("e1", &r.title.lock().unwrap())],"nextSyncToken":"T1"})
                    .to_string(),
            ),
            "POST" | "PATCH" if mode == WriteMode::ServerError => (503, "{}".into()),
            "POST" | "PATCH" if mode == WriteMode::Rejected => (400, "{}".into()),
            "POST" => (200, event_json("new1", "Planning").to_string()),
            "PATCH" => {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap_or_default();
                let title = body["summary"].as_str().unwrap_or("Standup").to_owned();
                (200, event_json("e1", &title).to_string())
            }
            _ => (405, "{}".into()),
        }
    })
    .await;
    let store = Arc::new(MemoryStore::default());
    store.set("me@example.org", "refresh").unwrap();
    let state = Arc::clone(&daemon.state);
    let audit: CycleAudit = Arc::new(move |o, d| {
        state.audit("connector", "gcal.sync", o, Some("gcal"), Some(d));
    });
    let connector: Arc<dyn Connector> = Arc::new(
        GcalConnector::new(
            &GcalConfig {
                account: "me@example.org".into(),
                calendars: vec!["primary".into()],
            },
            &ClientCredentials {
                client_id: "cid".into(),
                client_secret: "s".into(),
            },
            &format!("{}/calendar/v3", server.base),
            &format!("{}/token", server.base),
            store,
            audit,
        )
        .with_clock(Arc::new(|| NOW)),
    );
    daemon.state.register_connector(Arc::clone(&connector));
    daemon
        .state
        .store
        .replace_machine_labels(
            "m1",
            &std::collections::BTreeSet::from(["haste".to_owned()]),
        )
        .unwrap();
    Rig {
        _dir: dir,
        daemon,
        server,
        remote,
        connector,
        human,
    }
}

async fn call(r: &Rig, token: &str, body: RequestBody) -> Response {
    let mut stream = UnixStream::connect(&r.daemon.socket_path).await.unwrap();
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

async fn human(r: &Rig, body: RequestBody) -> Response {
    call(r, &r.human, body).await
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { error } => Some(error.code),
        _ => None,
    }
}

async fn agent(r: &Rig) -> String {
    let Response::Registered { token, .. } = human(
        r,
        RequestBody::RegisterPrincipal {
            principal_id: "agent-a".into(),
            kind: PrincipalKind::Agent,
            allowed_actions: vec![
                "read".into(),
                "calendar.create".into(),
                "calendar.update".into(),
            ],
        },
    )
    .await
    else {
        panic!("register failed")
    };
    token
}

async fn release_and_sync(r: &Rig) {
    human(
        r,
        RequestBody::ConnectorRelease {
            source: "gcal".into(),
        },
    )
    .await;
    sync_once(&r.daemon.state, &r.connector).await.unwrap();
}

async fn propose(r: &Rig, token: &str, payload: Payload, key: &str) -> Response {
    call(
        r,
        token,
        RequestBody::Propose {
            payload,
            thread_id: None,
            idempotency_key: key.into(),
            hints: None,
        },
    )
    .await
}

fn proposal_of(r: Response) -> haste_core::Proposal {
    match r {
        Response::Proposal { proposal } => proposal,
        other => panic!("{other:?}"),
    }
}

fn create_payload(project: Option<&str>) -> Payload {
    Payload::CalendarCreate {
        calendar: "primary".into(),
        title: "Planning".into(),
        start: NOW + 86_400,
        end: NOW + 86_400 + 3600,
        all_day: false,
        attendees: vec!["guest@example.org".into()],
        description: None,
        project: project.map(str::to_owned),
    }
}

fn writes(r: &Rig) -> Vec<haste_google_oauth::testing::Recorded> {
    r.server
        .requests()
        .into_iter()
        .filter(|q| q.method == "POST" || q.method == "PATCH" || q.method == "DELETE")
        .filter(|q| q.path != "/token")
        .collect()
}

async fn status(r: &Rig, id: &str) -> haste_core::Proposal {
    proposal_of(
        human(
            r,
            RequestBody::ProposalStatus {
                proposal_id: id.into(),
            },
        )
        .await,
    )
}

async fn approve_and_execute(r: &Rig, id: &str, ack: bool) -> Response {
    human(
        r,
        RequestBody::Approve {
            proposal_id: id.into(),
            acknowledge_stale: ack,
        },
    )
    .await;
    human(
        r,
        RequestBody::Execute {
            proposal_id: id.into(),
            acknowledge_stale: ack,
        },
    )
    .await
}

#[tokio::test]
async fn a_fresh_install_is_engaged_and_reads_nothing_until_released() {
    let rig = boot().await;
    assert_eq!(
        sync_once(&rig.daemon.state, &rig.connector)
            .await
            .unwrap_err(),
        CallError::KillSwitchEngaged
    );
    assert!(rig.server.requests().is_empty(), "no request while engaged");
    release_and_sync(&rig).await;
    let Response::Events { events } = human(&rig, RequestBody::EventsList { from: 0 }).await else {
        panic!()
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.external_id, "primary/e1");
    // a released connector stays released across a re-registration (restart)
    rig.daemon
        .state
        .register_connector(Arc::clone(&rig.connector));
    assert!(sync_once(&rig.daemon.state, &rig.connector).await.is_ok());
    rig.daemon.shutdown().await;
}

async fn events_of(rig: &Rig) -> Vec<haste_proto::EventView> {
    let Response::Events { events } = human(rig, RequestBody::EventsList { from: 0 }).await else {
        panic!()
    };
    events
}

#[tokio::test]
async fn an_event_can_be_moved_here_only_by_the_human_and_put_back() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let before = events_of(&rig).await;
    let event = before[0].clone();
    assert!(!event.shifted);
    let (start, end) = (
        event.event.start_at + 7_200,
        event.event.start_at + 7_200 + 1_800,
    );
    let shift = |times| RequestBody::EventShift {
        event_id: event.event.id.clone(),
        times,
    };
    // an agent cannot
    let token = agent(&rig).await;
    assert_eq!(
        code(&call(&rig, &token, shift(Some((start, end)))).await),
        Some(ErrorCode::Denied)
    );
    // nonsense is refused
    assert_eq!(
        code(&human(&rig, shift(Some((end, start)))).await),
        Some(ErrorCode::Validation)
    );
    // the human moves it: the list shows the new times, flagged
    assert!(matches!(
        human(&rig, shift(Some((start, end)))).await,
        Response::Done
    ));
    let moved = events_of(&rig).await;
    assert!(moved[0].shifted);
    assert_eq!(
        (moved[0].event.start_at, moved[0].event.end_at),
        (start, end)
    );
    // a new read of the calendar leaves it where it was put
    sync_once(&rig.daemon.state, &rig.connector).await.unwrap();
    assert_eq!(events_of(&rig).await[0].event.start_at, start);
    // Google was only read, never written
    assert!(writes(&rig).is_empty(), "nothing was sent to Google");
    // and it can be put back
    assert!(matches!(human(&rig, shift(None)).await, Response::Done));
    let back = events_of(&rig).await;
    assert!(!back[0].shifted);
    assert_eq!(back[0].event.start_at, event.event.start_at);
    let audit = std::fs::read_to_string(rig.daemon.state.config.data_dir.join("audit.jsonl"))
        .unwrap_or_default();
    assert!(audit.contains("event.shift"));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn an_all_day_event_given_a_time_is_a_timed_one_here_until_put_back() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let store = &rig.daemon.state.store;
    let day = 1_795_000_000 - 1_795_000_000 % 86_400;
    store
        .upsert_events(
            &[haste_core::model::Event {
                id: String::new(),
                external_id: "primary/all-day".into(),
                calendar: "primary".into(),
                title: "Conference".into(),
                start_at: day,
                end_at: day + 86_400,
                all_day: true,
                status: "confirmed".into(),
                attendees: vec![],
                location: String::new(),
                description_snippet: String::new(),
                updated_at: 1,
                missing_since: None,
                version: 0,
            }],
            &[],
        )
        .unwrap();
    let find = |events: Vec<haste_proto::EventView>| {
        events
            .into_iter()
            .find(|e| e.event.external_id == "primary/all-day")
            .unwrap()
    };
    let before = find(events_of(&rig).await);
    assert!(before.event.all_day);
    let (start, end) = (day + 10 * 3_600, day + 12 * 3_600);
    assert!(matches!(
        human(
            &rig,
            RequestBody::EventShift {
                event_id: before.event.id.clone(),
                times: Some((start, end)),
            }
        )
        .await,
        Response::Done
    ));
    let timed = find(events_of(&rig).await);
    assert!(!timed.event.all_day && timed.shifted);
    assert_eq!((timed.event.start_at, timed.event.end_at), (start, end));
    human(
        &rig,
        RequestBody::EventShift {
            event_id: before.event.id.clone(),
            times: None,
        },
    )
    .await;
    assert!(find(events_of(&rig).await).event.all_day, "back to all day");
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn invalid_calendar_proposals_are_refused_at_creation_and_pending_ones_write_nothing() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let token = agent(&rig).await;
    let event_id = rig.daemon.state.store.list_events(0).unwrap()[0].id.clone();

    // unknown project, attendee change, cancel, empty change, impossible time, unknown event
    let unknown = propose(&rig, &token, create_payload(Some("nope")), "k0").await;
    assert_eq!(code(&unknown), Some(ErrorCode::Validation));
    let update = |changes: EventChanges, id: &str| Payload::CalendarUpdate {
        event_id: id.into(),
        changes,
        project: None,
        event_version: None,
    };
    for (n, changes) in [
        EventChanges {
            title: Some("x".into()),
            attendees: Some(vec!["a@b.org".into()]),
            ..EventChanges::default()
        },
        EventChanges {
            status: Some("cancelled".into()),
            ..EventChanges::default()
        },
        EventChanges::default(),
        EventChanges {
            start: Some(10),
            end: Some(5),
            ..EventChanges::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let r = propose(&rig, &token, update(changes, &event_id), &format!("v{n}")).await;
        assert_eq!(code(&r), Some(ErrorCode::Validation), "case {n}");
    }
    let missing = propose(
        &rig,
        &token,
        update(
            EventChanges {
                title: Some("x".into()),
                ..EventChanges::default()
            },
            "no-such-event",
        ),
        "m",
    )
    .await;
    assert_eq!(code(&missing), Some(ErrorCode::NotFound));
    assert!(rig
        .daemon
        .state
        .store
        .list_proposals(&[ProposalState::Pending])
        .unwrap()
        .is_empty());

    // a valid proposal is pending and, pending or rejected, nothing is written
    let p = proposal_of(propose(&rig, &token, create_payload(Some("haste")), "k1").await);
    assert_eq!(p.state, ProposalState::Pending);
    assert!(writes(&rig).is_empty());
    human(
        &rig,
        RequestBody::Reject {
            proposal_id: p.id,
            reason: None,
        },
    )
    .await;
    assert!(writes(&rig).is_empty());
    // an agent can neither approve nor run anything
    for body in [
        RequestBody::Approve {
            proposal_id: "x".into(),
            acknowledge_stale: true,
        },
        RequestBody::Execute {
            proposal_id: "x".into(),
            acknowledge_stale: true,
        },
    ] {
        assert_eq!(
            code(&call(&rig, &token, body).await),
            Some(ErrorCode::Denied)
        );
    }
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn an_approved_create_writes_once_without_notifying_attendees_and_audits_ids_only() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let token = agent(&rig).await;
    let p = proposal_of(propose(&rig, &token, create_payload(Some("haste")), "k1").await);
    let done = proposal_of(approve_and_execute(&rig, &p.id, false).await);
    assert_eq!(done.state, ProposalState::Sent);
    assert_eq!(done.result.as_deref(), Some("event:primary/new1"));
    let w = writes(&rig);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].method, "POST");
    assert!(w[0].query.contains("sendUpdates=none"), "{}", w[0].query);
    assert!(!w[0].query.contains("sendUpdates=all"));
    // an executed proposal cannot run twice
    let again = human(
        &rig,
        RequestBody::Execute {
            proposal_id: p.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    assert_eq!(code(&again), Some(ErrorCode::IllegalTransition));
    assert_eq!(writes(&rig).len(), 1);
    // the new event is stored and tagged with the proposal's project
    let store = &rig.daemon.state.store;
    let created = store
        .list_events(0)
        .unwrap()
        .into_iter()
        .find(|e| e.external_id == "primary/new1")
        .unwrap();
    assert_eq!(store.event_tags(&created.id).unwrap(), ["haste"]);
    let audit = fs::read_to_string(rig.daemon.state.config.audit_path()).unwrap();
    assert!(audit.contains("event:primary/new1"));
    for secret in ["SECRET", "Planning", "guest@example.org", "secret.person"] {
        assert!(!audit.contains(secret), "audit leaked {secret}");
    }
    assert!(rig.server.requests().iter().all(|r| r.method != "DELETE"));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_stale_update_needs_acknowledgement_and_patches_only_the_allowed_field() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let token = agent(&rig).await;
    let event_id = rig.daemon.state.store.list_events(0).unwrap()[0].id.clone();
    let p = proposal_of(
        propose(
            &rig,
            &token,
            Payload::CalendarUpdate {
                event_id: event_id.clone(),
                changes: EventChanges {
                    title: Some("Standup v2".into()),
                    ..EventChanges::default()
                },
                project: None,
                event_version: None,
            },
            "u1",
        )
        .await,
    );
    human(
        &rig,
        RequestBody::Approve {
            proposal_id: p.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    // the event changes upstream after the proposal was made
    *rig.remote.title.lock().unwrap() = "Moved by a colleague".into();
    sync_once(&rig.daemon.state, &rig.connector).await.unwrap();
    let exec = |ack| RequestBody::Execute {
        proposal_id: p.id.clone(),
        acknowledge_stale: ack,
    };
    assert_eq!(
        code(&human(&rig, exec(false)).await),
        Some(ErrorCode::StaleNeedsAcknowledgement)
    );
    assert!(
        writes(&rig).is_empty(),
        "nothing sent before the confirmation"
    );
    let done = proposal_of(human(&rig, exec(true)).await);
    assert_eq!(done.state, ProposalState::Sent);
    let w = writes(&rig);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].method, "PATCH");
    assert!(w[0].query.contains("sendUpdates=none"));
    let body: serde_json::Value = serde_json::from_str(&w[0].body).unwrap();
    assert_eq!(
        body,
        json!({"summary":"Standup v2"}),
        "no attendees, no status"
    );
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn an_unknown_outcome_stays_executing_a_rejection_returns_to_approved_and_engaging_refuses() {
    let rig = boot().await;
    release_and_sync(&rig).await;
    let token = agent(&rig).await;
    let p = proposal_of(propose(&rig, &token, create_payload(None), "k1").await);
    human(
        &rig,
        RequestBody::Approve {
            proposal_id: p.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    let exec = RequestBody::Execute {
        proposal_id: p.id.clone(),
        acknowledge_stale: false,
    };

    // Google answers 400: certainly nothing was written, so the proposal can be run again
    *rig.remote.mode.lock().unwrap() = WriteMode::Rejected;
    assert_eq!(
        code(&human(&rig, exec.clone()).await),
        Some(ErrorCode::Denied)
    );
    assert_eq!(status(&rig, &p.id).await.state, ProposalState::Approved);

    // the kill switch refuses approved writes too
    human(
        &rig,
        RequestBody::ConnectorEngage {
            source: "gcal".into(),
        },
    )
    .await;
    assert_eq!(
        code(&human(&rig, exec.clone()).await),
        Some(ErrorCode::Denied)
    );
    assert_eq!(
        status(&rig, &p.id).await.result.as_deref(),
        Some("kill switch engaged")
    );
    human(
        &rig,
        RequestBody::ConnectorRelease {
            source: "gcal".into(),
        },
    )
    .await;

    // a 503: the write may have happened, so it is NOT retried automatically
    *rig.remote.mode.lock().unwrap() = WriteMode::ServerError;
    assert_eq!(
        code(&human(&rig, exec.clone()).await),
        Some(ErrorCode::Internal)
    );
    assert_eq!(status(&rig, &p.id).await.state, ProposalState::Executing);
    assert_eq!(
        code(&human(&rig, exec).await),
        Some(ErrorCode::IllegalTransition),
        "an executing proposal is never re-run"
    );
    let back = proposal_of(
        human(
            &rig,
            RequestBody::ResolveExecuting {
                proposal_id: p.id.clone(),
                done: false,
            },
        )
        .await,
    );
    assert_eq!(back.state, ProposalState::Approved);
    rig.daemon.shutdown().await;
}
