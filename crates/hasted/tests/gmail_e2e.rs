//! Daemon + Gmail connector against a local fake Google. No live calls, no real Keychain.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use haste_connector_gmail::keychain::{MemoryStore, TokenStore};
use haste_connector_gmail::testing::{spawn, FakeServer, Recorded};
use haste_connector_gmail::{GmailConfig, GmailConnector};
use haste_core::connector::{CallError, Connector};
use haste_core::model::Source;
use haste_core::policy::PrincipalKind;
use haste_core::proposal::{Payload, ProposalState};
use haste_proto::{encode_frame, parse_header, ErrorCode, Request, RequestBody, Response};
use hasted::notifier::RecordingNotifier;
use hasted::scheduler::sync_once;
use hasted::{start, DaemonConfig, DaemonHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const THREAD_FULL: &str =
    include_str!("../../haste-connector-gmail/tests/fixtures/thread_three_messages.json");
const REFRESH: &str = "REFRESH-TOKEN-XYZ";

struct Harness {
    _dir: tempfile::TempDir,
    daemon: DaemonHandle,
    google: FakeServer,
    connector: Arc<dyn Connector>,
    human: String,
    disabled: Arc<AtomicBool>,
    /// Gmail answers `threads.modify` with "insufficient permissions" (an old token).
    no_modify: Arc<AtomicBool>,
}

async fn boot() -> Harness {
    let disabled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&disabled);
    let no_modify = Arc::new(AtomicBool::new(false));
    let no_modify_flag = Arc::clone(&no_modify);
    let google = spawn(move |r: &Recorded| {
        if r.path == "/token" {
            return (200, r#"{"access_token":"AT-SECRET","expires_in":3600}"#.into());
        }
        if flag.load(Ordering::SeqCst) {
            return (403, r#"{"error":{"message":"Account has been disabled.","errors":[{"reason":"accountDisabled"}]}}"#.into());
        }
        match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/gmail/v1/users/me/profile") => (200, r#"{"emailAddress":"me@example.com","historyId":"900"}"#.into()),
            ("GET", "/gmail/v1/users/me/threads") => (200, r#"{"threads":[{"id":"t100"}]}"#.into()),
            ("GET", "/gmail/v1/users/me/threads/t100") => (200, THREAD_FULL.into()),
            ("GET", "/gmail/v1/users/me/history") => (200, r#"{"historyId":"900"}"#.into()),
            ("GET", "/gmail/v1/users/me/messages/m-att/attachments/ATT1") => {
                // "hello" as base64url
                (200, r#"{"size":5,"data":"aGVsbG8"}"#.into())
            }
            ("POST", "/gmail/v1/users/me/threads/t100/modify") => {
                if no_modify_flag.load(Ordering::SeqCst) {
                    (403, r#"{"error":{"message":"insufficient authentication scopes","errors":[{"reason":"insufficientPermissions"}]}}"#.into())
                } else {
                    (200, "{}".into())
                }
            }
            ("POST", "/gmail/v1/users/me/drafts") => (200, r#"{"id":"draft-1"}"#.into()),
            ("POST", "/gmail/v1/users/me/messages/send") => (200, r#"{"id":"sent-1"}"#.into()),
            ("DELETE", "/gmail/v1/users/me/drafts/draft-1") => (200, "{}".into()),
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
    tokens.set("me@example.com", REFRESH).unwrap();
    let cfg = GmailConfig {
        client_id: "cid".into(),
        client_secret: "s".into(),
        account: "me@example.com".into(),
    };
    let connector: Arc<dyn Connector> = Arc::new(GmailConnector::new(
        &cfg,
        &google.base,
        &format!("{}/token", google.base),
        tokens,
    ));
    daemon.state.register_connector(Arc::clone(&connector));
    Harness {
        _dir: dir,
        daemon,
        google,
        connector,
        human,
        disabled,
        no_modify,
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

async fn synced_thread(h: &Harness) -> String {
    sync_once(&h.daemon.state, &h.connector).await.unwrap();
    h.daemon
        .state
        .store
        .list_threads(&Default::default())
        .unwrap()[0]
        .0
        .id
        .clone()
}

async fn draft_proposal(h: &Harness, agent: &str, thread: &str, key: &str) -> String {
    let r = call_with(
        h,
        agent,
        RequestBody::Propose {
            payload: Payload::Draft {
                attachments: Vec::new(),
                to: None,
                body: "Noon is great.".into(),
            },
            thread_id: Some(thread.to_owned()),
            idempotency_key: key.into(),
            hints: None,
        },
    )
    .await;
    match r {
        Response::Proposal { proposal } => proposal.id,
        other => panic!("{other:?}"),
    }
}

fn drafts_requests(h: &Harness) -> usize {
    h.google.count("/gmail/v1/users/me/drafts")
}

fn audit(h: &Harness) -> String {
    fs::read_to_string(h.daemon.state.config.audit_path()).unwrap()
}

#[tokio::test]
async fn sync_stores_threads_messages_and_cursor_then_goes_incremental() {
    let h = boot().await;
    let report = sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert_eq!((report.threads, report.messages), (1, 3));
    let store = &h.daemon.state.store;
    let (thread, _) = &store.list_threads(&Default::default()).unwrap()[0];
    assert_eq!(thread.source, Source::Gmail);
    assert_eq!(thread.external_id, "t100");
    assert_eq!(store.messages_for_thread(&thread.id).unwrap().len(), 3);
    assert_eq!(
        store.connector_cursor(&Source::Gmail).unwrap().as_deref(),
        Some("900")
    );

    let second = sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert_eq!((second.threads, second.messages), (0, 0));
    assert_eq!(
        h.google.count("/gmail/v1/users/me/profile"),
        1,
        "second cycle used history, not a backfill"
    );
    assert_eq!(h.google.count("/gmail/v1/users/me/history"), 1);
    assert!(audit(&h).contains(r#""principal":"connector","action":"fetch","outcome":"ok""#));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn kill_switch_stops_sync_keeps_the_cursor_and_release_resumes_from_it() {
    let h = boot().await;
    sync_once(&h.daemon.state, &h.connector).await.unwrap();
    let before = h.google.requests().len();

    assert!(matches!(
        human(
            &h,
            RequestBody::ConnectorEngage {
                source: "gmail".into()
            }
        )
        .await,
        Response::Done
    ));
    assert_eq!(
        sync_once(&h.daemon.state, &h.connector).await,
        Err(CallError::KillSwitchEngaged)
    );
    assert_eq!(
        h.google.requests().len(),
        before,
        "nothing reached Google while engaged"
    );
    assert!(audit(&h).contains(r#""action":"fetch","outcome":"refused_kill_switch""#));
    assert_eq!(
        h.daemon
            .state
            .store
            .connector_cursor(&Source::Gmail)
            .unwrap()
            .as_deref(),
        Some("900")
    );

    assert!(matches!(
        human(
            &h,
            RequestBody::ConnectorRelease {
                source: "gmail".into()
            }
        )
        .await,
        Response::Done
    ));
    sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert_eq!(
        h.google.count("/gmail/v1/users/me/profile"),
        1,
        "resumed from the stored cursor, no re-backfill"
    );
    assert!(
        audit(&h).contains(r#""action":"connector.release","outcome":"released","object":"gmail""#)
    );
    assert!(
        audit(&h).contains(r#""action":"connector.engage","outcome":"engaged","object":"gmail""#)
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn account_restriction_trips_the_breaker_and_release_then_trips_it_again() {
    let h = boot().await;
    let mut events = h.daemon.state.events.subscribe();
    h.disabled.store(true, Ordering::SeqCst);
    assert!(matches!(
        sync_once(&h.daemon.state, &h.connector).await,
        Err(CallError::BreakerOpened(_))
    ));
    assert!(h
        .daemon
        .state
        .store
        .kill_switch_engaged(&Source::Gmail)
        .unwrap());
    assert_eq!(h.google.count("/gmail/v1/users/me/profile"), 1, "no retry");
    assert!(audit(&h).contains(r#""action":"fetch","outcome":"breaker_opened""#));
    let notice = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(notice, haste_proto::Event::Notice { message } if message.contains("circuit breaker"))
    );

    // The human releases while Google still restricts the account: it trips again at once.
    human(
        &h,
        RequestBody::ConnectorRelease {
            source: "gmail".into(),
        },
    )
    .await;
    assert!(!h
        .daemon
        .state
        .store
        .kill_switch_engaged(&Source::Gmail)
        .unwrap());
    assert!(matches!(
        sync_once(&h.daemon.state, &h.connector).await,
        Err(CallError::BreakerOpened(_))
    ));
    assert!(h
        .daemon
        .state
        .store
        .kill_switch_engaged(&Source::Gmail)
        .unwrap());
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn only_an_approved_draft_reaches_gmail_and_only_the_human_can_send_it() {
    let h = boot().await;
    let thread = synced_thread(&h).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;

    let rejected = draft_proposal(&h, &agent, &thread, "k1").await;
    assert_eq!(drafts_requests(&h), 0, "a pending proposal creates nothing");
    human(
        &h,
        RequestBody::Reject {
            proposal_id: rejected,
            reason: None,
        },
    )
    .await;
    assert_eq!(
        drafts_requests(&h),
        0,
        "a rejected proposal creates nothing"
    );

    let id = draft_proposal(&h, &agent, &thread, "k2").await;
    let approved = human(
        &h,
        RequestBody::Approve {
            proposal_id: id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    let Response::Proposal { proposal } = approved else {
        panic!("{approved:?}")
    };
    assert_eq!(proposal.state, ProposalState::Approved);
    assert_eq!(proposal.result.as_deref(), Some("draft:draft-1"));
    assert_eq!(drafts_requests(&h), 1);
    assert!(audit(&h).contains(r#""action":"draft.create","outcome":"created""#));
    assert!(audit(&h).contains("draft_id=draft-1"));

    // An agent can never send, whatever it presents.
    let forged = call_with(
        &h,
        &agent,
        RequestBody::Send {
            proposal_id: id.clone(),
            acknowledge_stale: true,
            recipients_digest: "x".into(),
        },
    )
    .await;
    assert_eq!(code(&forged), Some(ErrorCode::Denied));
    assert_eq!(h.google.count("/gmail/v1/users/me/messages/send"), 0);

    // The human previews (nothing is sent) and then sends what the preview showed.
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
    assert_eq!(preview.source, "gmail");
    assert_eq!(preview.recipients.len(), 1);
    assert_ne!(
        preview.recipients[0], "me@example.com",
        "never a reply to oneself"
    );
    assert!(preview.subject.starts_with("Re: "));
    assert_eq!(
        h.google.count("/gmail/v1/users/me/messages/send"),
        0,
        "a preview sends nothing"
    );
    let sent = human(
        &h,
        RequestBody::Send {
            proposal_id: id.clone(),
            acknowledge_stale: false,
            recipients_digest: preview.recipients_digest.clone(),
        },
    )
    .await;
    let Response::Proposal { proposal } = sent else {
        panic!("{sent:?}")
    };
    assert_eq!(proposal.state, ProposalState::Sent);
    assert_eq!(proposal.result.as_deref(), Some("sent:sent-1"));
    assert_eq!(h.google.count("/gmail/v1/users/me/messages/send"), 1);
    // the approval-time draft was removed, once
    assert_eq!(
        h.google
            .requests()
            .iter()
            .filter(|r| r.method == "DELETE")
            .count(),
        1
    );
    // the audit has ids and counts only: no address, subject or body
    let log = audit(&h);
    assert!(log.contains(r#""action":"proposal.send","outcome":"sent""#));
    for secret in [preview.recipients[0].as_str(), "Noon is great", "Re: "] {
        assert!(!log.contains(secret), "audit leaked {secret}");
    }
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn only_the_human_can_attach_files_and_they_go_out_in_a_multipart_message() {
    use base64::Engine;
    let h = boot().await;
    let thread = synced_thread(&h).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let dir = h.daemon.state.config.data_dir.join("attachments");
    fs::create_dir_all(&dir).unwrap();
    let content: Vec<u8> = (0..300_u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    fs::write(dir.join("100-report.pdf"), &content).unwrap();
    std::os::unix::fs::symlink("/etc/hosts", dir.join("100-link")).unwrap();
    let propose = |names: &[&str], key: &str| RequestBody::Propose {
        payload: Payload::Draft {
            attachments: names.iter().map(|n| (*n).to_owned()).collect(),
            to: None,
            body: "Here it is.".into(),
        },
        thread_id: Some(thread.clone()),
        idempotency_key: key.into(),
        hints: None,
    };

    // an agent can never attach a file, even one that exists
    let denied = call_with(&h, &agent, propose(&["100-report.pdf"], "a1")).await;
    assert_eq!(code(&denied), Some(ErrorCode::Denied));
    // the human is refused names that escape the folder, links and files that are not there
    for bad in [
        "../haste.db",
        "/etc/hosts",
        "100-link",
        "missing.pdf",
        ".hidden",
    ] {
        let r = human(&h, propose(&[bad], "h-bad")).await;
        assert_eq!(code(&r), Some(ErrorCode::Validation), "{bad}");
    }

    let Response::Proposal { proposal } = human(&h, propose(&["100-report.pdf"], "h1")).await
    else {
        panic!("no proposal")
    };
    human(
        &h,
        RequestBody::Approve {
            proposal_id: proposal.id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    let Response::SendPreview { preview } = human(
        &h,
        RequestBody::SendPreview {
            proposal_id: proposal.id.clone(),
        },
    )
    .await
    else {
        panic!("no preview")
    };
    assert_eq!(preview.attachments.len(), 1);
    assert_eq!(preview.attachments[0].bytes, 300);
    let sent = human(
        &h,
        RequestBody::Send {
            proposal_id: proposal.id.clone(),
            acknowledge_stale: false,
            recipients_digest: preview.recipients_digest.clone(),
        },
    )
    .await;
    assert!(matches!(sent, Response::Proposal { .. }), "{sent:?}");

    // what reached Gmail: a multipart message holding the file
    let req = h
        .google
        .requests()
        .into_iter()
        .find(|r| r.path == "/gmail/v1/users/me/messages/send")
        .expect("sent");
    let raw = serde_json::from_str::<serde_json::Value>(&req.body).unwrap()["raw"]
        .as_str()
        .unwrap()
        .to_owned();
    let mime = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .unwrap(),
    )
    .unwrap();
    assert!(mime.contains("multipart/mixed"));
    assert!(mime.contains("filename=\"100-report.pdf\""));
    let encoded = base64::engine::general_purpose::STANDARD.encode(&content);
    assert!(mime.replace("\r\n", "").contains(&encoded));
    // the audit counts files and never names one
    let log = audit(&h);
    assert!(log.contains("attachments=1"));
    assert!(!log.contains("report.pdf"));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn archiving_a_gmail_thread_leaves_the_inbox_first_and_only_then_here() {
    let h = boot().await;
    let thread = synced_thread(&h).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let archive = |t: &str| RequestBody::TriageSet {
        thread_id: t.to_owned(),
        priority: None,
        set_aside: None,
        snoozed_until: None,
        archived: Some(true),
    };
    let modifies = |h: &Harness| h.google.count("/gmail/v1/users/me/threads/t100/modify");
    let archived = |h: &Harness| {
        h.daemon
            .state
            .store
            .get_thread(&thread)
            .unwrap()
            .unwrap()
            .1
            .archived
    };

    // an agent cannot archive at all
    assert_eq!(
        code(&call_with(&h, &agent, archive(&thread)).await),
        Some(ErrorCode::Denied)
    );
    assert_eq!(modifies(&h), 0);

    // an old token (no modify permission): refused with the fix, nothing archived, no breaker
    h.no_modify.store(true, Ordering::SeqCst);
    let refused = human(&h, archive(&thread)).await;
    assert_eq!(code(&refused), Some(ErrorCode::Denied));
    assert!(!archived(&h));
    assert!(audit(&h).contains(r#""action":"thread.archive","outcome":"failed""#));
    h.no_modify.store(false, Ordering::SeqCst);

    // with the permission: removed from the Gmail inbox, then archived here
    assert!(matches!(human(&h, archive(&thread)).await, Response::Done));
    assert!(archived(&h));
    assert_eq!(modifies(&h), 2, "one refused attempt, one that worked");
    assert!(audit(&h).contains("archived_upstream"));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn a_resync_forgets_the_local_mail_and_reads_the_inbox_again() {
    let h = boot().await;
    let thread = synced_thread(&h).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let resync = RequestBody::ConnectorResync {
        source: "gmail".into(),
    };
    assert_eq!(
        code(&call_with(&h, &agent, resync.clone()).await),
        Some(ErrorCode::Denied)
    );
    assert_eq!(
        code(
            &human(
                &h,
                RequestBody::ConnectorResync {
                    source: "whatsapp".into()
                }
            )
            .await
        ),
        Some(ErrorCode::Validation),
        "chats are not re-read this way"
    );
    let profile_before = h.google.count("/gmail/v1/users/me/profile");
    assert!(matches!(human(&h, resync).await, Response::Done));
    let store = &h.daemon.state.store;
    // the old copy is gone and the cursor was reset, so the next cycle is a backfill again
    assert!(store.get_thread(&thread).unwrap().is_none());
    assert_eq!(store.connector_cursor(&Source::Gmail).unwrap(), None);
    sync_once(&h.daemon.state, &h.connector).await.unwrap();
    assert_eq!(
        h.google.count("/gmail/v1/users/me/profile"),
        profile_before + 1
    );
    assert_eq!(store.list_threads(&Default::default()).unwrap().len(), 1);
    assert!(audit(&h).contains(r#""action":"connector.resync","outcome":"purged""#));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn only_the_human_can_save_an_attachment_and_it_never_overwrites() {
    let h = boot().await;
    let thread = synced_thread(&h).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let store = &h.daemon.state.store;
    let stored = store
        .upsert_message(&haste_core::model::Message {
            id: String::new(),
            thread_id: thread.clone(),
            source: Source::Gmail,
            external_id: "m-att".into(),
            sender_person_id: None,
            sender_identifier: Some("ana@example.org".into()),
            body_text: "see attached\n[attachment: report.pdf (application/pdf, 5 bytes) #ATT1]"
                .into(),
            sent_at: 1_700_000_000,
            is_own: false,
        })
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    let save = |dir: Option<&std::path::Path>, index: usize| RequestBody::AttachmentSave {
        message_id: stored.id.clone(),
        index,
        dir: dir.map(|d| d.to_string_lossy().into_owned()),
    };
    // an agent cannot
    assert_eq!(
        code(&call_with(&h, &agent, save(Some(out.path()), 0)).await),
        Some(ErrorCode::Denied)
    );
    // the human is refused a missing or relative folder and an attachment that is not there
    assert_eq!(
        code(&human(&h, save(Some(std::path::Path::new("/no/such/dir")), 0)).await),
        Some(ErrorCode::Validation)
    );
    assert_eq!(
        code(&human(&h, save(Some(std::path::Path::new("relative")), 0)).await),
        Some(ErrorCode::Validation)
    );
    assert_eq!(
        code(&human(&h, save(Some(out.path()), 3)).await),
        Some(ErrorCode::NotFound)
    );
    // the file is written, then a second save of the same name gets a new one
    for expect in ["report.pdf", "report (1).pdf"] {
        let Response::AttachmentSaved { name, bytes, .. } =
            human(&h, save(Some(out.path()), 0)).await
        else {
            panic!("not saved")
        };
        assert_eq!((name.as_str(), bytes), (expect, 5));
        assert_eq!(std::fs::read(out.path().join(expect)).unwrap(), b"hello");
    }
    // a preview goes to the private folder inside the data directory
    let Response::AttachmentSaved { path, .. } = human(&h, save(None, 0)).await else {
        panic!("no preview file")
    };
    assert!(path.starts_with(
        h.daemon
            .state
            .config
            .data_dir
            .join("preview")
            .to_str()
            .unwrap()
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    // the audit counts bytes and never names the file
    let log = audit(&h);
    assert!(log.contains(r#""action":"attachment.save","outcome":"saved""#));
    assert!(!log.contains("report"));
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn stale_thread_needs_acknowledgement_before_any_draft_is_created() {
    let h = boot().await;
    let thread = synced_thread(&h).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let id = draft_proposal(&h, &agent, &thread, "k").await;
    // A new reply arrives after the proposal was made.
    h.daemon
        .state
        .store
        .upsert_thread(
            &Source::Gmail,
            "t100",
            "Lunch on Friday?",
            &[],
            9_999_999_999,
        )
        .unwrap();

    let refused = human(
        &h,
        RequestBody::Approve {
            proposal_id: id.clone(),
            acknowledge_stale: false,
        },
    )
    .await;
    assert_eq!(code(&refused), Some(ErrorCode::StaleNeedsAcknowledgement));
    assert_eq!(drafts_requests(&h), 0);
    let ok = human(
        &h,
        RequestBody::Approve {
            proposal_id: id,
            acknowledge_stale: true,
        },
    )
    .await;
    assert!(matches!(ok, Response::Proposal { .. }));
    assert_eq!(drafts_requests(&h), 1);
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn connector_admin_is_human_only_validated_and_audited() {
    let h = boot().await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let machine = register(&h, "box-1", PrincipalKind::Machine).await;
    for token in [&agent, &machine] {
        for body in [
            RequestBody::ConnectorStatus,
            RequestBody::ConnectorRelease {
                source: "gmail".into(),
            },
            RequestBody::ConnectorEngage {
                source: "gmail".into(),
            },
            RequestBody::ConnectorAuthorized {
                source: "gmail".into(),
            },
        ] {
            assert_eq!(
                code(&call_with(&h, token, body).await),
                Some(ErrorCode::Denied)
            );
        }
    }
    assert!(
        !h.daemon
            .state
            .store
            .kill_switch_engaged(&Source::Gmail)
            .unwrap(),
        "nothing changed"
    );
    assert!(audit(&h).contains(r#""principal":"agent-a","action":"manage","outcome":"denied""#));

    let unknown = human(
        &h,
        RequestBody::ConnectorRelease {
            source: "whatsapp".into(),
        },
    )
    .await;
    assert_eq!(code(&unknown), Some(ErrorCode::Validation));

    human(
        &h,
        RequestBody::ConnectorEngage {
            source: "gmail".into(),
        },
    )
    .await;
    match human(&h, RequestBody::ConnectorStatus).await {
        Response::Connectors { connectors } => {
            assert_eq!(connectors.len(), 1);
            assert_eq!(connectors[0].source, "gmail");
            assert!(connectors[0].engaged && connectors[0].registered);
        }
        other => panic!("{other:?}"),
    }
    human(
        &h,
        RequestBody::ConnectorAuthorized {
            source: "gmail".into(),
        },
    )
    .await;
    assert!(
        audit(&h).contains(r#""action":"connector.auth","outcome":"authorized","object":"gmail""#)
    );
    h.daemon.shutdown().await;
}

#[tokio::test]
async fn audit_log_and_store_never_hold_tokens_or_message_bodies() {
    let h = boot().await;
    let thread = synced_thread(&h).await;
    let agent = register(&h, "agent-a", PrincipalKind::Agent).await;
    let id = draft_proposal(&h, &agent, &thread, "k").await;
    human(
        &h,
        RequestBody::Approve {
            proposal_id: id,
            acknowledge_stale: false,
        },
    )
    .await;

    let log = audit(&h);
    for secret in [
        REFRESH,
        "AT-SECRET",
        "Shall we have lunch",
        "Noon works",
        "Noon is great",
    ] {
        assert!(!log.contains(secret), "audit log leaked {secret}");
    }
    let db = fs::read(h.daemon.state.config.db_path()).unwrap();
    let wal =
        fs::read(h.daemon.state.config.db_path().with_extension("db-wal")).unwrap_or_default();
    for secret in [REFRESH.as_bytes(), b"AT-SECRET"] {
        assert!(
            !db.windows(secret.len()).any(|w| w == secret),
            "database holds a token"
        );
        assert!(
            !wal.windows(secret.len()).any(|w| w == secret),
            "wal holds a token"
        );
    }
    h.daemon.shutdown().await;
}
