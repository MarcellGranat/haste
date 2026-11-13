//! Daemon + the three Beeper connectors against a loopback fake of Beeper Desktop's API. No live
//! calls, no real Keychain. The shapes the fake serves are the provisional ones in `wire.rs`.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haste_connector_beeper::client::BeeperClient;
use haste_connector_beeper::config::{AccountSelection, ChatSelection};
use haste_connector_beeper::consts::TOKEN_ACCOUNT;
use haste_connector_beeper::{BeeperConfig, BeeperConnector, BeeperSource, CycleAudit};
use haste_core::connector::{CallError, Connector};
use haste_core::model::Source;
use haste_core::policy::PrincipalKind;
use haste_core::proposal::{Payload, ProposalState};
use haste_core::send::CHAT_MAX_SENDS_PER_HOUR;
use haste_google_oauth::keychain::{MemoryStore, TokenStore};
use haste_google_oauth::testing::{spawn, FakeServer, Recorded};
use haste_proto::{encode_frame, parse_header, ErrorCode, Request, RequestBody, Response};
use hasted::notifier::RecordingNotifier;
use hasted::scheduler::sync_once;
use hasted::{start, DaemonConfig, DaemonHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const TOKEN: &str = "SECRET-BEEPER-TOKEN";
const NAME_MARKER: &str = "PRIVATE-CHAT-NAME";
const TEXT_MARKER: &str = "PRIVATE-MESSAGE-TEXT";
const REPLY_MARKER: &str = "PRIVATE-REPLY-TEXT";
/// 2026-11-09T09:59:00Z, a minute before the clock the daemon reads (any recent time works).
const MSG_TIME: &str = "2026-11-09T09:59:00Z";

type SendReply = Arc<Mutex<(u16, String)>>;

struct Harness {
    _dir: tempfile::TempDir,
    daemon: DaemonHandle,
    beeper: FakeServer,
    whatsapp: Arc<dyn Connector>,
    messenger: Arc<dyn Connector>,
    human: String,
    send_reply: SendReply,
}

async fn boot() -> Harness {
    let send_reply: SendReply = Arc::new(Mutex::new((200, r#"{"id":"beeper-msg-1"}"#.to_owned())));
    let reply = Arc::clone(&send_reply);
    let beeper = spawn(move |r: &Recorded| {
        let segs: Vec<&str> = r.path.trim_start_matches('/').split('/').collect();
        match (r.method.as_str(), segs.as_slice()) {
            ("GET", ["v1", "info"]) => (200, r#"{"version":"x"}"#.into()),
            ("GET", ["v1", "accounts"]) => (
                200,
                serde_json::json!({ "items": [
                    { "id": "acc-wa", "network": "whatsapp", "connectionType": "on-device" },
                    { "id": "acc-ms", "network": "facebook", "connectionType": "on-device" }
                ]})
                .to_string(),
            ),
            ("GET", ["v1", "chats"]) => (
                200,
                serde_json::json!({ "items": [
                    { "id": "chat-a", "accountID": "acc-wa", "title": NAME_MARKER },
                    { "id": "chat-m", "accountID": "acc-ms", "title": "Messenger friend" }
                ]})
                .to_string(),
            ),
            ("GET", ["v1", "chats", id]) => (
                200,
                serde_json::json!({ "id": id, "accountID": if *id == "chat-a" { "acc-wa" } else { "acc-ms" }, "title": NAME_MARKER })
                    .to_string(),
            ),
            ("GET", ["v1", "chats", _, "messages"]) => (
                200,
                serde_json::json!({ "items": [
                    { "id": "m1", "senderID": "user-1", "text": TEXT_MARKER, "timestamp": MSG_TIME, "sortKey": "000001" }
                ], "hasMore": false })
                .to_string(),
            ),
            ("POST", ["v1", "chats", _, "messages"]) => reply.lock().unwrap().clone(),
            _ => (404, "{}".into()),
        }
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let mut config = DaemonConfig::new(
        &dir.path().join("haste"),
        Arc::new(RecordingNotifier::default()),
    );
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();

    let tokens = Arc::new(MemoryStore::default());
    tokens.set(TOKEN_ACCOUNT, TOKEN).unwrap();
    let client = Arc::new(BeeperClient::new(&beeper.base, tokens).unwrap());
    let cfg = BeeperConfig {
        accounts: vec![
            AccountSelection {
                id: "acc-wa".into(),
                on_device_attested: false,
                allow_cloud: false,
            },
            AccountSelection {
                id: "acc-ms".into(),
                on_device_attested: false,
                allow_cloud: false,
            },
        ],
        chats: ChatSelection {
            allow_all: false,
            allow: vec!["chat-a".into(), "chat-m".into()],
            deny: vec![],
        },
        history_days: 30,
        base_url: None,
    };
    let make = |which: BeeperSource| -> Arc<dyn Connector> {
        let state = Arc::clone(&daemon.state);
        let name = which.name();
        let audit: CycleAudit = Arc::new(move |outcome, detail| {
            state.audit(
                "connector",
                &format!("{name}.sync"),
                outcome,
                Some(name),
                Some(detail),
            );
        });
        Arc::new(BeeperConnector::new(
            which,
            Arc::clone(&client),
            &cfg,
            audit,
        ))
    };
    let whatsapp = make(BeeperSource::Whatsapp);
    let messenger = make(BeeperSource::Messenger);
    daemon.state.register_connector(Arc::clone(&whatsapp));
    daemon.state.register_connector(Arc::clone(&messenger));
    Harness {
        _dir: dir,
        daemon,
        beeper,
        whatsapp,
        messenger,
        human,
        send_reply,
    }
}

async fn call_with(h: &Harness, token: &str, body: RequestBody) -> Response {
    let mut stream = UnixStream::connect(&h.daemon.socket_path).await.unwrap();
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

async fn human(h: &Harness, body: RequestBody) -> Response {
    call_with(h, &h.human, body).await
}

async fn release(h: &Harness, source: &str) {
    assert!(matches!(
        human(
            h,
            RequestBody::ConnectorRelease {
                source: source.into()
            }
        )
        .await,
        Response::Done
    ));
}

async fn register(h: &Harness, id: &str, kind: PrincipalKind) -> String {
    match human(
        h,
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

fn audit(h: &Harness) -> String {
    fs::read_to_string(h.daemon.state.config.audit_path()).unwrap()
}

fn posts(h: &Harness) -> usize {
    h.beeper
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count()
}

/// Release both switches and sync both networks; return the thread id of the given source.
async fn synced(h: &Harness, source: Source) -> String {
    release(h, "whatsapp").await;
    release(h, "messenger").await;
    sync_once(&h.daemon.state, &h.whatsapp).await.unwrap();
    sync_once(&h.daemon.state, &h.messenger).await.unwrap();
    h.daemon
        .state
        .store
        .list_threads(&Default::default())
        .unwrap()
        .into_iter()
        .find(|(t, _)| t.source == source)
        .expect("thread for source")
        .0
        .id
}

async fn approved_draft(h: &Harness, agent: &str, thread: &str, key: &str) -> String {
    let proposed = call_with(
        h,
        agent,
        RequestBody::Propose {
            payload: Payload::Draft {
                attachments: Vec::new(),
                to: None,
                body: REPLY_MARKER.into(),
            },
            thread_id: Some(thread.to_owned()),
            idempotency_key: key.into(),
            hints: None,
        },
    )
    .await;
    let Response::Proposal { proposal } = proposed else {
        panic!("{proposed:?}")
    };
    let approved = human(
        h,
        RequestBody::Approve {
            proposal_id: proposal.id.clone(),
            acknowledge_stale: true,
        },
    )
    .await;
    let Response::Proposal { proposal: p } = approved else {
        panic!("{approved:?}")
    };
    assert_eq!(p.state, ProposalState::Approved);
    p.id
}

async fn preview_digest(h: &Harness, id: &str) -> String {
    match human(
        h,
        RequestBody::SendPreview {
            proposal_id: id.to_owned(),
        },
    )
    .await
    {
        Response::SendPreview { preview } => preview.recipients_digest,
        other => panic!("{other:?}"),
    }
}

fn send_body(id: &str, digest: &str) -> RequestBody {
    RequestBody::Send {
        proposal_id: id.to_owned(),
        acknowledge_stale: true,
        recipients_digest: digest.to_owned(),
    }
}

#[tokio::test]
async fn a_fresh_install_is_engaged_and_makes_no_request_until_released() {
    let h = boot().await;
    for connector in [&h.whatsapp, &h.messenger] {
        assert_eq!(
            sync_once(&h.daemon.state, connector).await,
            Err(CallError::KillSwitchEngaged)
        );
    }
    assert!(h.beeper.requests().is_empty(), "nothing reached Beeper");
    release(&h, "whatsapp").await;
    let report = sync_once(&h.daemon.state, &h.whatsapp).await.unwrap();
    assert_eq!((report.threads, report.messages), (1, 1));
    let (thread, _) = &h
        .daemon
        .state
        .store
        .list_threads(&Default::default())
        .unwrap()[0];
    assert_eq!(thread.source, Source::Whatsapp);
    assert_eq!(thread.external_id, "chat-a");
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn approving_a_chat_draft_makes_no_request_and_only_the_human_sends_once() {
    let h = boot().await;
    let thread = synced(&h, Source::Whatsapp).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let id = approved_draft(&h, &agent, &thread, "k1").await;
    assert_eq!(posts(&h), 0, "approval creates nothing at Beeper");

    // an agent can never send
    let forged = call_with(&h, &agent, send_body(&id, "x")).await;
    assert_eq!(code(&forged), Some(ErrorCode::Denied));
    assert_eq!(posts(&h), 0);

    let Response::SendPreview { preview } = human(
        &h,
        RequestBody::SendPreview {
            proposal_id: id.clone(),
        },
    )
    .await
    else {
        panic!("no preview")
    };
    assert_eq!(preview.source, "whatsapp");
    assert_eq!(preview.recipients, vec![NAME_MARKER.to_owned()]);
    assert!(preview.subject.is_empty());
    assert_eq!(posts(&h), 0, "a preview sends nothing");

    // a digest that is not the preview's is refused before anything is sent
    let wrong = human(&h, send_body(&id, "not-the-digest")).await;
    assert_eq!(code(&wrong), Some(ErrorCode::Validation));
    assert_eq!(posts(&h), 0);

    let sent = human(&h, send_body(&id, &preview.recipients_digest)).await;
    let Response::Proposal { proposal } = sent else {
        panic!("{sent:?}")
    };
    assert_eq!(proposal.state, ProposalState::Sent);
    assert_eq!(posts(&h), 1, "exactly one POST");

    // counts and fixed labels only: no chat name, text, reply or token
    let log = audit(&h);
    assert!(log.contains(r#""action":"proposal.send","outcome":"sent""#));
    for secret in [
        NAME_MARKER,
        TEXT_MARKER,
        REPLY_MARKER,
        TOKEN,
        "beeper-msg-1",
    ] {
        assert!(!log.contains(secret), "audit leaked {secret}");
    }
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn an_ambiguous_chat_send_stays_executing_and_is_never_repeated() {
    let h = boot().await;
    let thread = synced(&h, Source::Whatsapp).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let id = approved_draft(&h, &agent, &thread, "k1").await;
    let digest = preview_digest(&h, &id).await;
    *h.send_reply.lock().unwrap() = (500, "{}".to_owned());
    let r = human(&h, send_body(&id, &digest)).await;
    assert!(code(&r).is_some(), "{r:?}");
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Executing
    );
    let again = human(&h, send_body(&id, &digest)).await;
    assert_eq!(code(&again), Some(ErrorCode::IllegalTransition));
    assert_eq!(posts(&h), 1, "never submitted twice");
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn an_engaged_kill_switch_refuses_preview_and_send() {
    let h = boot().await;
    let thread = synced(&h, Source::Whatsapp).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let id = approved_draft(&h, &agent, &thread, "k1").await;
    let digest = preview_digest(&h, &id).await;
    assert!(matches!(
        human(
            &h,
            RequestBody::ConnectorEngage {
                source: "whatsapp".into()
            }
        )
        .await,
        Response::Done
    ));
    let before = h.beeper.requests().len();
    let preview = human(
        &h,
        RequestBody::SendPreview {
            proposal_id: id.clone(),
        },
    )
    .await;
    assert_eq!(code(&preview), Some(ErrorCode::Denied));
    let sent = human(&h, send_body(&id, &digest)).await;
    assert_eq!(code(&sent), Some(ErrorCode::Denied));
    assert_eq!(h.beeper.requests().len(), before, "nothing reached Beeper");
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Approved
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn chat_sends_are_capped_per_hour_per_account_and_the_other_network_is_unaffected() {
    let h = boot().await;
    let wa = synced(&h, Source::Whatsapp).await;
    let ms = synced(&h, Source::Messenger).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    for i in 0..CHAT_MAX_SENDS_PER_HOUR {
        let id = approved_draft(&h, &agent, &wa, &format!("k{i}")).await;
        let digest = preview_digest(&h, &id).await;
        let r = human(&h, send_body(&id, &digest)).await;
        assert!(
            matches!(&r, Response::Proposal { proposal } if proposal.state == ProposalState::Sent),
            "{i}: {r:?}"
        );
    }
    let id = approved_draft(&h, &agent, &wa, "over").await;
    let digest = preview_digest(&h, &id).await;
    let r = human(&h, send_body(&id, &digest)).await;
    assert_eq!(code(&r), Some(ErrorCode::Validation));
    assert_eq!(posts(&h) as u32, CHAT_MAX_SENDS_PER_HOUR);
    // Messenger's account has its own hour
    let other = approved_draft(&h, &agent, &ms, "other").await;
    let other_digest = preview_digest(&h, &other).await;
    let r = human(&h, send_body(&other, &other_digest)).await;
    assert!(
        matches!(&r, Response::Proposal { proposal } if proposal.state == ProposalState::Sent),
        "{r:?}"
    );
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Approved
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn read_only_and_machine_principals_cannot_send_chat_drafts() {
    let h = boot().await;
    let thread = synced(&h, Source::Whatsapp).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let machine = register(&h, "machine-a", PrincipalKind::Machine).await;
    let id = approved_draft(&h, &agent, &thread, "k1").await;
    for token in [&agent, &machine] {
        let r = call_with(&h, token, send_body(&id, "x")).await;
        assert_eq!(code(&r), Some(ErrorCode::Denied));
        let p = call_with(
            &h,
            token,
            RequestBody::SendPreview {
                proposal_id: id.clone(),
            },
        )
        .await;
        assert_eq!(code(&p), Some(ErrorCode::Denied));
    }
    assert_eq!(posts(&h), 0);
    h.daemon.shutdown().await;
}
