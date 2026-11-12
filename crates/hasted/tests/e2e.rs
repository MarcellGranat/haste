//! End-to-end tests: a real daemon on a temporary socket directory.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use haste_core::connector::{
    Batch, BoxFuture, Capabilities, Connector, ConnectorError, Cursor, DraftToSend, SendReceipt,
};
use haste_core::model::Source;
use haste_core::proposal::{Payload, ProposalState};
use haste_proto::{
    encode_frame, parse_header, ErrorCode, Event, Request, RequestBody, Response, Topic,
    MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
use hasted::notifier::RecordingNotifier;
use hasted::scheduler::{run_delivery, run_expiry};
use hasted::{start, DaemonConfig, DaemonHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

struct Harness {
    _dir: tempfile::TempDir,
    daemon: DaemonHandle,
    notifier: Arc<RecordingNotifier>,
    human: String,
}

async fn boot() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let notifier = Arc::new(RecordingNotifier::default());
    let mut config = DaemonConfig::new(&dir.path().join("haste"), notifier.clone());
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    config.delivery_backoff = Duration::ZERO;
    let daemon = start(config).await.unwrap();
    // the daemon never creates the notes folder: the user (here, the test) chooses and makes it
    fs::create_dir_all(&daemon.state.config.notes_root).unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();
    Harness {
        _dir: dir,
        daemon,
        notifier,
        human,
    }
}

async fn read_response(stream: &mut UnixStream) -> Response {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await.unwrap();
    let len = parse_header(header).unwrap();
    let mut body = vec![0_u8; len];
    stream.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn call_request(socket: &Path, request: &Request) -> Response {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    stream
        .write_all(&encode_frame(request).unwrap())
        .await
        .unwrap();
    read_response(&mut stream).await
}

impl Harness {
    async fn call(&self, token: &str, body: RequestBody) -> Response {
        call_request(&self.daemon.socket_path, &Request::new(token, body)).await
    }

    async fn register(&self, id: &str, actions: &[&str]) -> String {
        let r = self
            .call(
                &self.human,
                RequestBody::RegisterPrincipal {
                    principal_id: id.into(),
                    kind: haste_core::policy::PrincipalKind::Agent,
                    allowed_actions: actions.iter().map(|s| (*s).to_owned()).collect(),
                },
            )
            .await;
        match r {
            Response::Registered { token, .. } => token,
            other => panic!("registration failed: {other:?}"),
        }
    }

    fn seed_thread(&self) -> String {
        self.daemon
            .state
            .store
            .upsert_thread(&Source::Gmail, "ext-1", "Hello", &["ana@x.org".into()], 10)
            .unwrap()
            .id
    }

    /// The digest the daemon's preview gives for a send, as the TUI would pass it back.
    async fn preview_digest(&self, id: &str) -> String {
        match self
            .call(
                &self.human,
                RequestBody::SendPreview {
                    proposal_id: id.into(),
                },
            )
            .await
        {
            Response::SendPreview { preview } => preview.recipients_digest,
            other => panic!("{other:?}"),
        }
    }

    fn audit_text(&self) -> String {
        fs::read_to_string(self.daemon.state.config.audit_path()).unwrap_or_default()
    }
}

const AGENT_ACTIONS: [&str; 6] = [
    "read",
    "draft",
    "archive",
    "snooze",
    "reminder.create",
    "note.append",
];

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { error } => Some(error.code),
        _ => None,
    }
}

fn draft(thread: &str, key: &str) -> RequestBody {
    RequestBody::Propose {
        payload: Payload::Draft {
            to: Some("ana@x.org".into()),
            body: "hi".into(),
        },
        thread_id: Some(thread.to_owned()),
        idempotency_key: key.into(),
        hints: None,
    }
}

struct TestConnector {
    send: bool,
    calls: Arc<std::sync::atomic::AtomicU32>,
}

impl Connector for TestConnector {
    fn source(&self) -> Source {
        Source::Gmail
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            draft: true,
            send: self.send,
            archive: true,
            ..Default::default()
        }
    }
    fn fetch(&self, _c: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        Box::pin(async { Ok(Batch::default()) })
    }
    fn send<'a>(
        &'a self,
        _d: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Ok(SendReceipt {
                external_id: "gm-1".into(),
            })
        })
    }
}

#[tokio::test]
async fn read_draft_and_note_append_end_to_end() {
    let h = boot().await;
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;

    let listed = h
        .call(
            &agent,
            RequestBody::ThreadList {
                sources: vec!["gmail".into()],
                include_archived: false,
                include_snoozed: false,
            },
        )
        .await;
    assert!(matches!(&listed, Response::Threads { threads } if threads.len() == 1));

    let proposal = match h.call(&agent, draft(&thread, "k1")).await {
        Response::Proposal { proposal } => proposal,
        other => panic!("{other:?}"),
    };
    assert_eq!(proposal.state, ProposalState::Pending);
    // Retried creation returns the same proposal.
    let again = h.call(&agent, draft(&thread, "k1")).await;
    assert!(matches!(&again, Response::Proposal { proposal: p } if p.id == proposal.id));

    let appended = h
        .call(
            &agent,
            RequestBody::NoteAppend {
                path: "people/ana.md".into(),
                text: "likes tea".into(),
                idempotency_key: "n1".into(),
            },
        )
        .await;
    assert!(matches!(appended, Response::Appended { .. }));
    let note = fs::read_to_string(h.daemon.state.config.notes_root.join("people/ana.md")).unwrap();
    assert!(note.contains("<!-- haste: appended by agent-a at "));
    assert!(note.contains("likes tea"));

    let outside = h
        .call(
            &agent,
            RequestBody::NoteAppend {
                path: "private/keys.md".into(),
                text: "x".into(),
                idempotency_key: "n2".into(),
            },
        )
        .await;
    assert_eq!(code(&outside), Some(ErrorCode::Denied));
    assert!(!h.daemon.state.config.notes_root.join("private").exists());

    // Audit records carry the path, never the text.
    assert!(!h.audit_text().contains("likes tea"));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn forged_send_is_denied_and_audited() {
    let h = boot().await;
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let r = h
        .call(
            &agent,
            RequestBody::Send {
                proposal_id: "x".into(),
                acknowledge_stale: true,
                recipients_digest: "x".into(),
            },
        )
        .await;
    assert_eq!(code(&r), Some(ErrorCode::Denied));
    assert!(h
        .audit_text()
        .contains(r#""action":"send","outcome":"denied""#));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn agent_cannot_approve_reject_or_register() {
    let h = boot().await;
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let id = match h.call(&agent, draft(&thread, "k")).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    for body in [
        RequestBody::Approve {
            proposal_id: id.clone(),
            acknowledge_stale: true,
        },
        RequestBody::Reject {
            proposal_id: id.clone(),
            reason: None,
        },
        RequestBody::RevokeToken {
            principal_id: "agent-a".into(),
        },
        RequestBody::AuditRecent { limit: 5 },
        RequestBody::Subscribe {
            topics: vec![Topic::Proposals],
        },
    ] {
        assert_eq!(code(&h.call(&agent, body).await), Some(ErrorCode::Denied));
    }
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Pending
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn token_failures_are_uniform_and_leak_nothing() {
    let h = boot().await;
    let agent = h.register("agent-a", &["read"]).await;
    let list = || RequestBody::ThreadList {
        sources: vec![],
        include_archived: false,
        include_snoozed: false,
    };

    let missing = h.call("", list()).await;
    assert_eq!(code(&missing), Some(ErrorCode::Unauthenticated));
    let forged = "forged-token-material";
    let bad = h.call(forged, list()).await;
    assert_eq!(code(&bad), Some(ErrorCode::Unauthenticated));
    assert!(!format!("{bad:?}").contains(forged));
    assert!(!h.audit_text().contains(forged));

    // Token bound to `read` only cannot draft.
    let thread = h.seed_thread();
    assert_eq!(
        code(&h.call(&agent, draft(&thread, "k")).await),
        Some(ErrorCode::Unauthorized)
    );

    // Revocation is immediate and permanent.
    assert!(matches!(
        h.call(
            &h.human,
            RequestBody::RevokeToken {
                principal_id: "agent-a".into()
            }
        )
        .await,
        Response::Done
    ));
    assert_eq!(
        code(&h.call(&agent, list()).await),
        Some(ErrorCode::Unauthenticated)
    );

    // The real token never reaches the audit file.
    assert!(!h.audit_text().contains(agent.split_once('.').unwrap().1));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn human_can_read_recent_audit_records() {
    let h = boot().await;
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    h.call(
        &agent,
        RequestBody::ThreadList {
            sources: vec![],
            include_archived: false,
            include_snoozed: false,
        },
    )
    .await;
    match h
        .call(&h.human, RequestBody::AuditRecent { limit: 100 })
        .await
    {
        Response::Audit { records } => {
            assert!(records
                .iter()
                .any(|r| r.principal == "agent-a" && r.action == "read"));
            assert!(records.windows(2).all(|w| w[1].seq == w[0].seq + 1));
        }
        other => panic!("{other:?}"),
    }
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn unsupported_version_is_rejected_without_action() {
    let h = boot().await;
    let mut req = Request::new(&h.human, RequestBody::AuditRecent { limit: 1 });
    req.version = PROTOCOL_VERSION + 1;
    let r = call_request(&h.daemon.socket_path, &req).await;
    assert_eq!(code(&r), Some(ErrorCode::UnsupportedVersion));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn oversized_frame_closes_connection_and_is_audited() {
    let h = boot().await;
    let mut stream = UnixStream::connect(&h.daemon.socket_path).await.unwrap();
    let len = u32::try_from(MAX_FRAME_BYTES + 1).unwrap();
    stream.write_all(&len.to_be_bytes()).await.unwrap();
    let mut buf = [0_u8; 1];
    assert_eq!(stream.read(&mut buf).await.unwrap_or(0), 0);
    assert!(h
        .audit_text()
        .contains(r#""action":"frame","outcome":"oversized""#));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn proposal_status_is_scoped_to_owner() {
    let h = boot().await;
    let thread = h.seed_thread();
    let a = h.register("agent-a", &AGENT_ACTIONS).await;
    let b = h.register("agent-b", &AGENT_ACTIONS).await;
    let id = match h.call(&a, draft(&thread, "k")).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    assert!(matches!(
        h.call(
            &a,
            RequestBody::ProposalStatus {
                proposal_id: id.clone()
            }
        )
        .await,
        Response::Proposal { .. }
    ));
    assert_eq!(
        code(
            &h.call(&b, RequestBody::ProposalStatus { proposal_id: id })
                .await
        ),
        Some(ErrorCode::NotFound)
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn approve_then_send_and_result_delivery() {
    let h = boot().await;
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    h.daemon.state.register_connector(Arc::new(TestConnector {
        send: true,
        calls: calls.clone(),
    }));
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let id = match h.call(&agent, draft(&thread, "k")).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    let digest = haste_core::send::recipients_digest(&["ana@x.org".to_owned()]);
    let send = |digest: String| RequestBody::Send {
        proposal_id: id.clone(),
        acknowledge_stale: false,
        recipients_digest: digest,
    };
    // Preview and send before approval are refused, and nothing is called.
    let early_preview = h
        .call(
            &h.human,
            RequestBody::SendPreview {
                proposal_id: id.clone(),
            },
        )
        .await;
    assert_eq!(code(&early_preview), Some(ErrorCode::IllegalTransition));
    let early = h.call(&h.human, send(digest.clone())).await;
    assert_eq!(code(&early), Some(ErrorCode::IllegalTransition));
    // Approval does not send.
    let approved = h
        .call(
            &h.human,
            RequestBody::Approve {
                proposal_id: id.clone(),
                acknowledge_stale: false,
            },
        )
        .await;
    assert!(
        matches!(&approved, Response::Proposal { proposal } if proposal.state == ProposalState::Approved)
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    // A digest that does not match what the daemon resolves is refused: nothing is called.
    let wrong = h.call(&h.human, send("not-the-digest".into())).await;
    assert_eq!(code(&wrong), Some(ErrorCode::Validation));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Approved
    );
    // The preview carries the digest the human then confirms.
    let shown = h.preview_digest(&id).await;
    assert_eq!(shown, digest);
    let sent = h.call(&h.human, send(shown.clone())).await;
    assert!(
        matches!(&sent, Response::Proposal { proposal } if proposal.state == ProposalState::Sent
            && proposal.result.as_deref() == Some("sent:gm-1")),
        "{sent:?}"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Never twice.
    let again = h.call(&h.human, send(shown)).await;
    assert_eq!(code(&again), Some(ErrorCode::IllegalTransition));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    // No address ever reaches the audit.
    assert!(!h.audit_text().contains("ana@x.org"));

    let mut last = HashMap::new();
    assert_eq!(run_delivery(&h.daemon.state, &mut last).await, 1);
    assert_eq!(
        run_delivery(&h.daemon.state, &mut last).await,
        0,
        "delivered results are not re-sent"
    );
    let delivered = h.notifier.sent.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, "agent-a");
    assert!(delivered[0].1.contains(&id));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn stale_proposal_needs_acknowledgement() {
    let h = boot().await;
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let id = match h.call(&agent, draft(&thread, "k")).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    h.daemon
        .state
        .store
        .upsert_thread(&Source::Gmail, "ext-1", "Hello (new reply)", &[], 99)
        .unwrap();
    let r = h
        .call(
            &h.human,
            RequestBody::Approve {
                proposal_id: id.clone(),
                acknowledge_stale: false,
            },
        )
        .await;
    assert_eq!(code(&r), Some(ErrorCode::StaleNeedsAcknowledgement));
    let ok = h
        .call(
            &h.human,
            RequestBody::Approve {
                proposal_id: id,
                acknowledge_stale: true,
            },
        )
        .await;
    assert!(matches!(ok, Response::Proposal { .. }));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn delivery_failures_stop_after_three_attempts() {
    let h = boot().await;
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let id = match h.call(&agent, draft(&thread, "k")).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    *h.notifier.fail.lock().unwrap() = true;
    h.call(
        &h.human,
        RequestBody::Reject {
            proposal_id: id.clone(),
            reason: Some("no".into()),
        },
    )
    .await;
    let mut last = HashMap::new();
    for _ in 0..6 {
        run_delivery(&h.daemon.state, &mut last).await;
    }
    assert_eq!(*h.notifier.attempts.lock().unwrap(), 3);
    assert_eq!(
        h.audit_text()
            .matches(r#""action":"herdr.prompt","outcome":"failed""#)
            .count(),
        3
    );
    // The result stays available through status.
    let status = h
        .call(&agent, RequestBody::ProposalStatus { proposal_id: id })
        .await;
    assert!(
        matches!(&status, Response::Proposal { proposal } if proposal.state == ProposalState::Rejected)
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn kill_switch_blocks_send() {
    let h = boot().await;
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    h.daemon.state.register_connector(Arc::new(TestConnector {
        send: true,
        calls: calls.clone(),
    }));
    h.daemon
        .state
        .store
        .set_kill_switch(&Source::Gmail, true)
        .unwrap();
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let id = match h.call(&agent, draft(&thread, "k")).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    h.call(
        &h.human,
        RequestBody::Approve {
            proposal_id: id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    let r = h
        .call(
            &h.human,
            RequestBody::Send {
                proposal_id: id.clone(),
                acknowledge_stale: false,
                recipients_digest: haste_core::send::recipients_digest(&["ana@x.org".to_owned()]),
            },
        )
        .await;
    assert_eq!(code(&r), Some(ErrorCode::Denied));
    // nothing was sent, and the proposal went back to approved so it can be sent later
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Approved
    );
    assert!(h.audit_text().contains("kill_switch_engaged"));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn expiry_pass_expires_overdue_proposals() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = DaemonConfig::new(
        &dir.path().join("haste"),
        Arc::new(RecordingNotifier::default()),
    );
    config.proposal_ttl_secs = -1; // already overdue on creation
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();
    let agent = match call_request(
        &daemon.socket_path,
        &Request::new(
            &human,
            RequestBody::RegisterPrincipal {
                principal_id: "agent-a".into(),
                kind: haste_core::policy::PrincipalKind::Agent,
                allowed_actions: vec!["draft".into()],
            },
        ),
    )
    .await
    {
        Response::Registered { token, .. } => token,
        other => panic!("{other:?}"),
    };
    let r = call_request(
        &daemon.socket_path,
        &Request::new(
            &agent,
            RequestBody::Propose {
                payload: Payload::Archive,
                thread_id: None,
                idempotency_key: "k".into(),
                hints: None,
            },
        ),
    )
    .await;
    // `archive` is outside this token's subset, so nothing was created.
    assert_eq!(code(&r), Some(ErrorCode::Unauthorized));
    let r = call_request(
        &daemon.socket_path,
        &Request::new(
            &agent,
            RequestBody::Propose {
                payload: Payload::Draft {
                    to: None,
                    body: "x".into(),
                },
                thread_id: None,
                idempotency_key: "k".into(),
                hints: None,
            },
        ),
    )
    .await;
    let id = match r {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    assert_eq!(run_expiry(&daemon.state).await, 1);
    assert_eq!(
        daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Expired
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn subscribed_client_receives_proposal_events() {
    let h = boot().await;
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;

    let mut stream = UnixStream::connect(&h.daemon.socket_path).await.unwrap();
    let sub = Request::new(
        &h.human,
        RequestBody::Subscribe {
            topics: vec![Topic::Proposals],
        },
    );
    stream
        .write_all(&encode_frame(&sub).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut stream).await,
        Response::Subscribed
    ));

    let id = match h.call(&agent, draft(&thread, "k")).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    let event = tokio::time::timeout(Duration::from_secs(5), read_response(&mut stream))
        .await
        .unwrap();
    assert!(
        matches!(event, Response::Event { event: Event::ProposalChanged { proposal_id, state } }
        if proposal_id == id && state == ProposalState::Pending)
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn startup_refuses_loose_socket_directory_and_binds_no_network() {
    let dir = tempfile::tempdir().unwrap();
    let config = DaemonConfig::new(
        &dir.path().join("haste"),
        Arc::new(RecordingNotifier::default()),
    );
    fs::create_dir_all(&config.run_dir).unwrap();
    fs::set_permissions(&config.data_dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&config.run_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let err = start(config.clone()).await.err().expect("must refuse");
    assert!(
        err.to_string().contains("run"),
        "error names the directory: {err}"
    );

    fs::set_permissions(&config.run_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let daemon = start(config).await.unwrap();
    let mode = fs::metadata(&daemon.state.config.run_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    let human_mode = fs::metadata(daemon.state.config.human_token_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(human_mode, 0o600);
    let started = Instant::now();
    daemon.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(5));
}

// ------------------------------------------------------------------ send failure paths

/// A Gmail-shaped connector whose send always fails the same way, counting its calls.
struct FailingConnector {
    error: ConnectorError,
    calls: Arc<std::sync::atomic::AtomicU32>,
}

impl Connector for FailingConnector {
    fn source(&self) -> Source {
        Source::Gmail
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            send: true,
            ..Default::default()
        }
    }
    fn fetch(&self, _c: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        Box::pin(async { Ok(Batch::default()) })
    }
    fn send<'a>(
        &'a self,
        _d: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let error = self.error.clone();
        Box::pin(async move { Err(error) })
    }
}

async fn approved_draft(h: &Harness, agent: &str, thread: &str, key: &str) -> String {
    let id = match h.call(agent, draft(thread, key)).await {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    };
    h.call(
        &h.human,
        RequestBody::Approve {
            proposal_id: id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    id
}

fn send_body(id: &str) -> RequestBody {
    RequestBody::Send {
        proposal_id: id.into(),
        acknowledge_stale: false,
        recipients_digest: haste_core::send::recipients_digest(&["ana@x.org".to_owned()]),
    }
}

#[tokio::test]
async fn a_refused_send_returns_to_approved_and_may_be_tried_again() {
    let h = boot().await;
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    h.daemon
        .state
        .register_connector(Arc::new(FailingConnector {
            error: ConnectorError::Other("send refused: http 429".into()),
            calls: calls.clone(),
        }));
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let id = approved_draft(&h, &agent, &thread, "k").await;
    let r = h.call(&h.human, send_body(&id)).await;
    assert_eq!(code(&r), Some(ErrorCode::Internal));
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one attempt, no retry"
    );
    let p = h.daemon.state.store.get_proposal(&id).unwrap();
    assert_eq!(p.state, ProposalState::Approved);
    assert_eq!(p.result.as_deref(), Some("send_refused"));
    // it can be sent again by the human: a second, separate attempt
    h.call(&h.human, send_body(&id)).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(h
        .audit_text()
        .contains(r#""action":"proposal.send","outcome":"failed""#));
    assert!(!h.audit_text().contains("ana@x.org"));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn an_ambiguous_send_stays_executing_is_never_repeated_and_waits_for_the_human() {
    let h = boot().await;
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    h.daemon
        .state
        .register_connector(Arc::new(FailingConnector {
            error: ConnectorError::Ambiguous("dropped after data".into()),
            calls: calls.clone(),
        }));
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    let id = approved_draft(&h, &agent, &thread, "k").await;
    let r = h.call(&h.human, send_body(&id)).await;
    assert_eq!(code(&r), Some(ErrorCode::Internal));
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Executing
    );
    assert_eq!(
        h.daemon.state.store.send_outcome(&id).unwrap().as_deref(),
        Some("outcome_unknown")
    );
    // another send is refused outright: the connector is not called again
    let again = h.call(&h.human, send_body(&id)).await;
    assert_eq!(code(&again), Some(ErrorCode::IllegalTransition));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(h.audit_text().contains(r#""outcome":"needs_check""#));
    // a restart-style report lists it as needing a check and changes nothing
    assert_eq!(hasted::report_interrupted_executions(&h.daemon.state), 1);
    // the human looks in the mailbox and settles it: delivered
    let resolved = h
        .call(
            &h.human,
            RequestBody::ResolveExecuting {
                proposal_id: id.clone(),
                done: true,
            },
        )
        .await;
    assert!(
        matches!(&resolved, Response::Proposal { proposal } if proposal.state == ProposalState::Sent)
    );
    // an agent can neither settle it nor send it
    let denied = h
        .call(
            &agent,
            RequestBody::ResolveExecuting {
                proposal_id: id.clone(),
                done: false,
            },
        )
        .await;
    assert_eq!(code(&denied), Some(ErrorCode::Denied));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn the_eleventh_send_in_an_hour_is_refused_before_the_connector_is_called() {
    let h = boot().await;
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    h.daemon.state.register_connector(Arc::new(TestConnector {
        send: true,
        calls: calls.clone(),
    }));
    let thread = h.seed_thread();
    let agent = h.register("agent-a", &AGENT_ACTIONS).await;
    for i in 0..haste_core::send::MAX_SENDS_PER_HOUR {
        let id = approved_draft(&h, &agent, &thread, &format!("k{i}")).await;
        // each draft supersedes the older one for the same thread; send it straight away
        let r = h.call(&h.human, send_body(&id)).await;
        assert!(
            matches!(&r, Response::Proposal { proposal } if proposal.state == ProposalState::Sent),
            "{i}: {r:?}"
        );
    }
    let id = approved_draft(&h, &agent, &thread, "over").await;
    let r = h.call(&h.human, send_body(&id)).await;
    assert_eq!(code(&r), Some(ErrorCode::Validation));
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        haste_core::send::MAX_SENDS_PER_HOUR,
        "the over-limit send never reached the connector"
    );
    assert_eq!(
        h.daemon.state.store.get_proposal(&id).unwrap().state,
        ProposalState::Approved
    );
    h.daemon.shutdown().await;
}

/// Reads nothing; counts how often it is asked to fetch. Starts engaged like a fresh install.
struct CountingConnector {
    fetches: Arc<std::sync::atomic::AtomicU32>,
}

impl Connector for CountingConnector {
    fn source(&self) -> Source {
        Source::Gmail
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            ..Default::default()
        }
    }
    fn default_engaged(&self) -> bool {
        true
    }
    fn fetch(&self, _c: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        self.fetches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(Batch::default()) })
    }
    fn send<'a>(
        &'a self,
        _d: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        Box::pin(async { Err(ConnectorError::Other("no send".into())) })
    }
}

#[tokio::test]
async fn releasing_a_source_runs_its_first_sync_at_once_not_at_the_next_tick() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let h = boot().await;
    let fetches = Arc::new(AtomicU32::new(0));
    let connector: Arc<dyn Connector> = Arc::new(CountingConnector {
        fetches: Arc::clone(&fetches),
    });
    h.daemon.state.register_connector(Arc::clone(&connector));
    // an hour between ticks: only the release can trigger the next cycle in this test
    tokio::spawn(hasted::scheduler::sync_loop(
        Arc::clone(&h.daemon.state),
        connector,
        Duration::from_secs(3600),
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        0,
        "engaged on a fresh install: the start-up cycle is refused before any fetch"
    );
    let released = h
        .call(
            &h.human,
            RequestBody::ConnectorRelease {
                source: "gmail".into(),
            },
        )
        .await;
    assert!(matches!(released, Response::Done), "{released:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while fetches.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        1,
        "one cycle ran on release"
    );
}
