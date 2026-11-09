//! Daemon + iCloud connector against a loopback fake IMAP server. No network, no Keychain.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use haste_connector_icloud::config::IcloudConfig;
use haste_connector_icloud::secrets::{MemoryStore, SecretStore};
use haste_connector_icloud::session::PlainOpener;
use haste_connector_icloud::testing::{spawn, FakeFolder, FakeImap, FakeState, TestMessage};
use haste_connector_icloud::{CycleAudit, IcloudConnector};
use haste_core::connector::{CallError, Connector};
use haste_core::model::Source;
use haste_core::policy::PrincipalKind;
use haste_proto::{encode_frame, parse_header, ErrorCode, Request, RequestBody, Response};
use hasted::notifier::RecordingNotifier;
use hasted::scheduler::sync_once;
use hasted::{start, DaemonConfig, DaemonHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const PASSWORD: &str = "hunter2-app-password";

struct Harness {
    _dir: tempfile::TempDir,
    daemon: DaemonHandle,
    imap: FakeImap,
    connector: Arc<dyn Connector>,
    human: String,
}

fn message(uid: u32, subject: &str, from: &str, headers: &str, body: &str) -> TestMessage {
    let mut m = TestMessage::simple(uid, subject, headers, body);
    m.headers = m.headers.replace("ana@example.org", from);
    m
}

async fn boot(password_on_server: &str) -> Harness {
    let mut folders = std::collections::BTreeMap::new();
    folders.insert(
        "INBOX".to_owned(),
        FakeFolder {
            uid_validity: 42,
            messages: vec![message(
                1,
                "Quarterly plan",
                "ana@example.org",
                "",
                "SECRET BODY ONE",
            )],
        },
    );
    let imap = spawn(FakeState {
        folders,
        password: password_on_server.to_owned(),
        ..FakeState::default()
    });

    let dir = tempfile::tempdir().unwrap();
    let mut config = DaemonConfig::new(
        &dir.path().join("haste"),
        Arc::new(RecordingNotifier::default()),
    );
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();

    let mut cfg = IcloudConfig::new("me@icloud.com");
    cfg.host = "127.0.0.1".into();
    cfg.port = imap.port;
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("me@icloud.com", PASSWORD).unwrap();
    let state = Arc::clone(&daemon.state);
    let audit: CycleAudit = Arc::new(move |outcome, detail| {
        state.audit(
            "connector",
            "icloud.sync",
            outcome,
            Some("icloud"),
            Some(detail),
        );
    });
    let connector: Arc<dyn Connector> = Arc::new(IcloudConnector::new(
        cfg,
        secrets,
        Arc::new(PlainOpener),
        audit,
    ));
    daemon.state.register_connector(Arc::clone(&connector));
    Harness {
        _dir: dir,
        daemon,
        imap,
        connector,
        human,
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

fn audit(h: &Harness) -> String {
    fs::read_to_string(h.daemon.state.config.audit_path()).unwrap()
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { error } => Some(error.code),
        _ => None,
    }
}

#[tokio::test]
async fn sync_stores_icloud_threads_and_a_delta_keeps_the_threads_subject_and_participants() {
    let h = boot(PASSWORD).await;
    let report = sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert_eq!((report.threads, report.messages), (1, 1));
    let store = &h.daemon.state.store;
    let (thread, _) = store.list_threads(&Default::default()).unwrap().remove(0);
    assert_eq!(thread.source, Source::Icloud);
    assert_eq!(thread.subject, "Quarterly plan");
    assert!(store
        .connector_cursor(&Source::Icloud)
        .unwrap()
        .unwrap()
        .contains("\"last_uid\":1"));

    // A reply from someone new arrives; the delta batch contains only that reply.
    h.imap
        .state
        .lock()
        .unwrap()
        .folders
        .get_mut("INBOX")
        .unwrap()
        .messages
        .push(message(
            2,
            "Re: Quarterly plan",
            "bela@example.org",
            "In-Reply-To: <m1@example.org>\r\nReferences: <m1@example.org>\r\n",
            "SECRET BODY TWO",
        ));
    let second = sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert_eq!(
        (second.threads, second.messages),
        (1, 1),
        "only the new message was fetched"
    );

    let threads = store.list_threads(&Default::default()).unwrap();
    assert_eq!(threads.len(), 1, "the reply joined the existing thread");
    let thread = &threads[0].0;
    assert_eq!(thread.subject, "Quarterly plan", "not renamed to 'Re: ...'");
    assert!(
        thread.participants.contains(&"ana@example.org".to_owned()),
        "{:?}",
        thread.participants
    );
    assert!(
        thread.participants.contains(&"bela@example.org".to_owned()),
        "{:?}",
        thread.participants
    );
    assert_eq!(store.messages_for_thread(&thread.id).unwrap().len(), 2);
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn connector_admin_works_for_icloud_without_icloud_specific_code() {
    let h = boot(PASSWORD).await;
    match human(&h, RequestBody::ConnectorStatus).await {
        Response::Connectors { connectors } => {
            assert_eq!(connectors.len(), 1);
            assert_eq!(connectors[0].source, "icloud");
            assert!(connectors[0].registered && !connectors[0].engaged);
        }
        other => panic!("{other:?}"),
    }
    sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert!(matches!(
        human(
            &h,
            RequestBody::ConnectorEngage {
                source: "icloud".into()
            }
        )
        .await,
        Response::Done
    ));
    let before = h.imap.state.lock().unwrap().commands.len();
    assert_eq!(
        sync_once(&h.daemon.state, &h.connector).await,
        Err(CallError::KillSwitchEngaged)
    );
    assert_eq!(
        h.imap.state.lock().unwrap().commands.len(),
        before,
        "no IMAP traffic while engaged"
    );
    assert!(
        h.daemon
            .state
            .store
            .connector_cursor(&Source::Icloud)
            .unwrap()
            .is_some(),
        "cursor kept"
    );
    assert!(matches!(
        human(
            &h,
            RequestBody::ConnectorRelease {
                source: "icloud".into()
            }
        )
        .await,
        Response::Done
    ));
    sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert!(audit(&h)
        .contains(r#""action":"connector.release","outcome":"released","object":"icloud""#));

    // Agents are still locked out.
    let agent = match human(
        &h,
        RequestBody::RegisterPrincipal {
            principal_id: "agent-a".into(),
            kind: PrincipalKind::Agent,
            allowed_actions: vec!["read".into()],
        },
    )
    .await
    {
        Response::Registered { token, .. } => token,
        other => panic!("{other:?}"),
    };
    let denied = call_with(
        &h,
        &agent,
        RequestBody::ConnectorRelease {
            source: "icloud".into(),
        },
    )
    .await;
    assert_eq!(code(&denied), Some(ErrorCode::Denied));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn a_revoked_app_password_engages_the_kill_switch_notifies_and_is_not_retried() {
    let h = boot("the-server-now-wants-another-password").await;
    let mut events = h.daemon.state.events.subscribe();
    assert!(matches!(
        sync_once(&h.daemon.state, &h.connector).await,
        Err(CallError::BreakerOpened(_))
    ));
    assert!(h
        .daemon
        .state
        .store
        .kill_switch_engaged(&Source::Icloud)
        .unwrap());
    assert_eq!(
        h.imap.state.lock().unwrap().login_attempts,
        1,
        "exactly one LOGIN, no retry"
    );
    let notice = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(notice, haste_proto::Event::Notice { message } if message.contains("icloud") && message.contains("circuit breaker"))
    );
    assert!(audit(&h).contains(r#""action":"fetch","outcome":"breaker_opened""#));

    // Still failing: nothing even tries to log in until the human releases it.
    assert_eq!(
        sync_once(&h.daemon.state, &h.connector).await,
        Err(CallError::KillSwitchEngaged)
    );
    assert_eq!(h.imap.state.lock().unwrap().login_attempts, 1);

    // The human fixes the password, releases, and the next cycle syncs.
    h.imap.state.lock().unwrap().password = PASSWORD.to_owned();
    human(
        &h,
        RequestBody::ConnectorRelease {
            source: "icloud".into(),
        },
    )
    .await;
    assert_eq!(
        sync_once(&h.daemon.state, &h.connector)
            .await
            .unwrap()
            .messages,
        1
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn audit_log_has_counts_only_and_never_the_password_or_message_text() {
    let h = boot(PASSWORD).await;
    sync_once(&h.daemon.state, &h.connector).await.unwrap();
    let log = audit(&h);
    assert!(log.contains(r#""action":"icloud.sync","outcome":"ok","object":"icloud","detail":"INBOX:new=1,reset=false,skipped=false""#), "{log}");
    for secret in [
        PASSWORD,
        "SECRET BODY ONE",
        "Quarterly plan",
        "ana@example.org",
    ] {
        assert!(!log.contains(secret), "audit log leaked {secret}");
    }
    let db = fs::read(h.daemon.state.config.db_path()).unwrap();
    assert!(
        !db.windows(PASSWORD.len()).any(|w| w == PASSWORD.as_bytes()),
        "database holds the password"
    );
    h.daemon.shutdown().await;
}
