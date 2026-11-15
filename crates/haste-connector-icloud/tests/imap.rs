//! The connector against a loopback fake IMAP server. No network, no real Keychain.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use haste_connector_icloud::config::{FolderConfig, IcloudConfig};
use haste_connector_icloud::secrets::{MemoryStore, SecretStore};
use haste_connector_icloud::session::{ImapFailure, Opener, PlainOpener, TlsOpener};
use haste_connector_icloud::testing::{
    spawn, FakeFolder, FakeImap, FakeState, TestMessage, TestPart,
};
use haste_connector_icloud::{CycleAudit, IcloudConnector};
use haste_core::connector::{
    CallError, Connector, ConnectorError, Cursor, DraftToSend, Guarded, HistoryRequest, Hooks,
    TRANSIENT_ATTEMPTS_PER_CYCLE,
};
use haste_core::model::Source;

const PASSWORD: &str = "hunter2-app-password";
const ADDRESS: &str = "me@icloud.com";

struct Rig {
    imap: FakeImap,
    connector: IcloudConnector,
    audits: Arc<Mutex<Vec<String>>>,
}

fn messages(uids: &[u32]) -> Vec<TestMessage> {
    uids.iter()
        .map(|u| TestMessage::simple(*u, &format!("Subject number {u}"), "", "SECRET BODY TEXT"))
        .collect()
}

fn state(folders: Vec<(&str, u32, Vec<TestMessage>)>, password: &str) -> FakeState {
    let mut s = FakeState {
        password: password.to_owned(),
        ..FakeState::default()
    };
    for (name, validity, messages) in folders {
        s.folders.insert(
            name.to_owned(),
            FakeFolder {
                uid_validity: validity,
                messages,
            },
        );
    }
    s
}

fn rig_with(state: FakeState, tweak: impl FnOnce(&mut IcloudConfig)) -> Rig {
    let imap = spawn(state);
    let mut config = IcloudConfig::new(ADDRESS);
    config.host = "127.0.0.1".into();
    config.port = imap.port;
    tweak(&mut config);
    let secrets = Arc::new(MemoryStore::default());
    secrets.set(ADDRESS, PASSWORD).unwrap();
    let audits = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&audits);
    let audit: CycleAudit =
        Arc::new(move |outcome, detail| sink.lock().unwrap().push(format!("{outcome} {detail}")));
    let connector = IcloudConnector::new(config, secrets, Arc::new(PlainOpener), audit);
    Rig {
        imap,
        connector,
        audits,
    }
}

fn rig(uids: &[u32]) -> Rig {
    rig_with(state(vec![("INBOX", 42, messages(uids))], PASSWORD), |_| {})
}

fn commands(r: &Rig) -> Vec<String> {
    r.imap.state.lock().unwrap().commands.clone()
}

#[derive(Default)]
struct TestHooks {
    engaged: Mutex<HashSet<String>>,
    notices: Mutex<Vec<String>>,
    audits: Mutex<Vec<String>>,
}

impl Hooks for TestHooks {
    fn kill_switch_engaged(&self, s: &Source) -> bool {
        self.engaged.lock().unwrap().contains(&s.as_name())
    }
    fn set_kill_switch(&self, s: &Source, on: bool) {
        let mut set = self.engaged.lock().unwrap();
        if on {
            set.insert(s.as_name());
        } else {
            set.remove(&s.as_name());
        }
    }
    fn audit(&self, action: &str, _s: &Source, outcome: &str) {
        self.audits
            .lock()
            .unwrap()
            .push(format!("{action}:{outcome}"));
    }
    fn notify(&self, m: &str) {
        self.notices.lock().unwrap().push(m.to_owned());
    }
}

#[test]
fn capabilities_are_read_send_and_the_humans_archive() {
    let r = rig(&[]);
    let caps = r.connector.capabilities();
    // `send` is consulted only by the human path: the gate denies Action::Send to agents
    assert!(caps.read && !caps.draft && caps.send && caps.archive);
    assert_eq!(r.connector.source(), Source::Icloud);
}

#[tokio::test]
async fn archiving_moves_the_threads_inbox_messages_to_the_archive_folder_and_nothing_else() {
    use haste_core::connector::ArchiveTarget;
    let r = rig_with(
        state(
            vec![
                ("INBOX", 42, messages(&[1, 2, 3])),
                ("Archive", 7, Vec::new()),
            ],
            PASSWORD,
        ),
        |_| {},
    );
    let uids = |r: &Rig, f: &str| -> Vec<u32> {
        r.imap.state.lock().unwrap().folders[f]
            .messages
            .iter()
            .map(|m| m.uid)
            .collect()
    };
    let target = |ids: &[&str]| ArchiveTarget {
        thread_external_id: "t".into(),
        message_external_ids: ids.iter().map(|s| (*s).to_owned()).collect(),
    };
    // a recycled folder (other UIDVALIDITY) moves nothing
    assert!(r
        .connector
        .archive_thread(&target(&["INBOX:99:1"]))
        .await
        .is_err());
    assert_eq!(uids(&r, "INBOX"), vec![1, 2, 3]);
    // ids of other folders or without the shape are left alone: nothing to do is an error
    assert!(r
        .connector
        .archive_thread(&target(&["Sent Messages:42:1", "m1@example.org"]))
        .await
        .is_err());
    assert_eq!(uids(&r, "INBOX"), vec![1, 2, 3]);
    // a message without a Message-ID is named by its UID: `INBOX:<uidvalidity>:<uid>`
    // (checked at the end); the thread's two messages move, the third stays
    // real mail is named by its Message-ID header
    r.connector
        .archive_thread(&target(&["m1@example.org", "<m2@example.org>"]))
        .await
        .unwrap();
    assert_eq!(uids(&r, "INBOX"), vec![3]);
    assert_eq!(uids(&r, "Archive"), vec![1, 2]);
    let cmds = commands(&r).join("\n");
    assert!(
        cmds.contains("UID MOVE 1,2 \"Archive\"") || cmds.contains("UID MOVE 1,2 Archive"),
        "{cmds}"
    );
    assert!(!cmds.contains("STORE") && !cmds.contains("EXPUNGE") && !cmds.contains("DELETE"));
    // a missing Archive folder is refused by the server and nothing is lost
    let missing = rig_with(state(vec![("INBOX", 42, messages(&[1]))], PASSWORD), |_| {});
    assert!(missing
        .connector
        .archive_thread(&target(&["INBOX:42:1"]))
        .await
        .is_err());
    assert_eq!(
        missing.imap.state.lock().unwrap().folders["INBOX"]
            .messages
            .len(),
        1
    );
}

#[tokio::test]
async fn an_attachment_is_listed_with_its_part_and_fetched_on_demand_with_peek_only() {
    use haste_core::connector::AttachmentRequest;
    let mut with_file = TestMessage::simple(1, "Report", "", "see the file");
    with_file.parts.push(TestPart::pdf("scan.pdf", 8));
    let r = rig_with(
        state(vec![("INBOX", 42, vec![with_file])], PASSWORD),
        |_| {},
    );
    let batch = r.connector.fetch(None).await.unwrap();
    let body = &batch.messages[0].body_text;
    let lines = haste_core::attachment::lines_of(body);
    assert_eq!(lines.len(), 1, "{body}");
    assert_eq!(lines[0].name, "scan.pdf");
    assert_eq!(lines[0].reference.as_deref(), Some("2|base64"));
    let before = commands(&r).len();
    let bytes = r
        .connector
        .fetch_attachment(&AttachmentRequest {
            message_external_id: batch.messages[0].external_id.clone(),
            thread_external_id: String::new(),
            reference: lines[0].reference.clone().unwrap(),
            bytes: lines[0].bytes,
        })
        .await
        .unwrap();
    // eight base64 characters ("AAAAAAAA") decode to six zero bytes
    assert_eq!(bytes, vec![0_u8; 6]);
    let cmds = commands(&r)[before..].join("\n");
    assert!(
        cmds.contains("EXAMINE") && cmds.contains("BODY.PEEK[2]"),
        "{cmds}"
    );
    assert!(!cmds.contains("SELECT") && !cmds.contains("STORE") && !cmds.contains("MOVE"));
    // an old line without a reference says what to do
    let none = r
        .connector
        .fetch_attachment(&AttachmentRequest::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&none, ConnectorError::Other(m) if m.contains("resync")),
        "{none:?}"
    );
}

#[tokio::test]
async fn a_cycle_sends_only_read_commands_and_always_uses_body_peek() {
    let mut mixed = TestMessage::simple(3, "Report", "", "see attached");
    mixed.parts.push(TestPart::pdf("q3.pdf", 5000));
    let mut html_only = TestMessage::simple(4, "Newsletter", "", "");
    html_only.parts = vec![TestPart::html(
        "<p>hi</p><img src=\"http://tracker/x.gif\">",
    )];
    let mut msgs = messages(&[1, 2]);
    msgs.push(mixed);
    msgs.push(html_only);
    let r = rig_with(state(vec![("INBOX", 42, msgs)], PASSWORD), |_| {});

    let batch = r.connector.fetch(None).await.unwrap();
    assert_eq!(batch.messages.len(), 4);

    let cmds = commands(&r);
    for c in &cmds {
        let verb = c.split_whitespace().next().unwrap_or("");
        let second = c.split_whitespace().nth(1).unwrap_or("");
        let allowed = matches!(verb, "LOGIN" | "EXAMINE" | "LOGOUT")
            || (verb == "UID" && matches!(second, "SEARCH" | "FETCH"));
        assert!(allowed, "unexpected IMAP command: {c}");
    }
    for forbidden in [
        "STORE", "APPEND", "COPY", "MOVE", "EXPUNGE", "CREATE", "DELETE", "RENAME", "SELECT",
        "RFC822",
    ] {
        assert!(
            !cmds.iter().any(|c| c.contains(forbidden)),
            "{forbidden} was sent: {cmds:?}"
        );
    }
    for c in cmds.iter().filter(|c| c.starts_with("UID FETCH")) {
        assert!(
            c.contains("BODY.PEEK"),
            "a fetch without PEEK would mark mail as seen: {c}"
        );
        assert!(
            !c.replace("BODY.PEEK", "").contains("BODY["),
            "non-peek body fetch: {c}"
        );
    }
    // Only the text/plain part of the mixed message is requested, never the attachment or the HTML.
    assert!(cmds.iter().any(|c| c.contains("UID FETCH 3 (BODY.PEEK[1]")));
    assert!(
        !cmds.iter().any(|c| c.contains("BODY.PEEK[2]")),
        "attachment body requested: {cmds:?}"
    );
    // The HTML-only message is fetched once (its only text part), with PEEK like everything else.
    assert_eq!(
        cmds.iter()
            .filter(|c| c.starts_with("UID FETCH 4 (BODY.PEEK[1]"))
            .count(),
        1,
        "{cmds:?}"
    );

    let report = batch
        .messages
        .iter()
        .find(|m| m.external_id == "m3@example.org")
        .unwrap();
    assert!(report.body_text.contains("see attached"));
    assert!(report
        .body_text
        .contains("[attachment: q3.pdf (application/pdf, 5000 bytes) #"));
    let newsletter = batch
        .messages
        .iter()
        .find(|m| m.external_id == "m4@example.org")
        .unwrap();
    assert_eq!(
        newsletter.body_text.trim(),
        "hi",
        "{:?}",
        newsletter.body_text
    );
    assert!(
        !newsletter.body_text.contains("tracker"),
        "{}",
        newsletter.body_text
    );
}

#[tokio::test]
async fn first_sync_then_delta_then_idempotent_resync() {
    let r = rig_with(
        state(vec![("INBOX", 42, messages(&[1, 2, 3, 4, 5]))], PASSWORD),
        |c| c.backfill_count = 3,
    );
    let first = r.connector.fetch(None).await.unwrap();
    assert_eq!(
        first.messages.len(),
        3,
        "first sync is bounded to the newest 3"
    );
    let cursor = first.next_cursor.clone().unwrap();

    // Nothing new: an empty batch and the same position.
    let quiet = r.connector.fetch(Some(cursor.clone())).await.unwrap();
    assert!(quiet.messages.is_empty());
    assert_eq!(quiet.next_cursor, Some(cursor.clone()));

    // New mail arrives: only the new UID is fetched.
    r.imap
        .state
        .lock()
        .unwrap()
        .folders
        .get_mut("INBOX")
        .unwrap()
        .messages
        .push(TestMessage::simple(6, "Fresh", "", "x"));
    let delta = r.connector.fetch(Some(cursor.clone())).await.unwrap();
    assert_eq!(delta.messages.len(), 1);
    assert_eq!(delta.messages[0].external_id, "m6@example.org");
    assert!(delta.next_cursor.unwrap().0.contains("\"last_uid\":6"));

    // Fetching the same range again gives byte-identical ids.
    let again = r.connector.fetch(None).await.unwrap();
    let once_more = r.connector.fetch(None).await.unwrap();
    assert_eq!(again.threads, once_more.threads);
    assert_eq!(again.messages, once_more.messages);
}

#[tokio::test]
async fn uidvalidity_change_resets_only_that_folder() {
    let r = rig_with(
        state(
            vec![
                ("INBOX", 42, messages(&[1, 2])),
                ("Archive", 7, messages(&[1, 2, 3])),
            ],
            PASSWORD,
        ),
        |c| c.folders = vec![FolderConfig::new("INBOX"), FolderConfig::new("Archive")],
    );
    let first = r.connector.fetch(None).await.unwrap();
    let cursor = first.next_cursor.unwrap();
    r.imap
        .state
        .lock()
        .unwrap()
        .folders
        .get_mut("INBOX")
        .unwrap()
        .uid_validity = 99;
    let second = r.connector.fetch(Some(cursor)).await.unwrap();
    assert_eq!(
        second.messages.len(),
        2,
        "INBOX backfilled again, Archive untouched"
    );
    let audits = r.audits.lock().unwrap().clone();
    assert!(
        audits.last().unwrap().contains("INBOX:new=2,reset=true"),
        "{audits:?}"
    );
    assert!(
        audits.last().unwrap().contains("Archive:new=0,reset=false"),
        "{audits:?}"
    );
}

#[tokio::test]
async fn user_folders_are_synced_and_a_missing_one_is_skipped() {
    let r = rig_with(
        state(
            vec![
                ("INBOX", 1, messages(&[1])),
                ("Sent Messages", 2, messages(&[1, 2])),
            ],
            PASSWORD,
        ),
        |c| {
            c.folders = vec![
                FolderConfig::new("Missing"),
                FolderConfig::new("INBOX"),
                FolderConfig {
                    name: "Sent Messages".into(),
                    backfill: Some(1),
                },
            ]
        },
    );
    let batch = r.connector.fetch(None).await.unwrap();
    assert_eq!(
        batch.messages.len(),
        2,
        "1 from INBOX + the newest 1 from Sent Messages"
    );
    assert!(r
        .audits
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .contains("Missing:new=0,reset=false,skipped=true"));
}

#[tokio::test]
async fn a_wrong_password_is_terminal_trips_the_breaker_and_is_never_retried() {
    let r = rig_with(
        state(vec![("INBOX", 1, messages(&[1]))], "a-different-password"),
        |_| {},
    );
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &r.connector,
        hooks: &hooks,
    };
    let result = guard.fetch(None).await;
    assert!(
        matches!(
            result,
            Err(CallError::BreakerOpened(ConnectorError::Checkpoint(_)))
        ),
        "{result:?}"
    );
    assert_eq!(
        r.imap.state.lock().unwrap().login_attempts,
        1,
        "no retry after a rejected login"
    );
    assert!(hooks.engaged.lock().unwrap().contains("icloud"));
    assert_eq!(hooks.notices.lock().unwrap().len(), 1);

    // Until the human releases it, nothing even tries to log in.
    assert_eq!(guard.fetch(None).await, Err(CallError::KillSwitchEngaged));
    assert_eq!(r.imap.state.lock().unwrap().login_attempts, 1);

    // After release (and a good password) the next cycle works.
    hooks.engaged.lock().unwrap().clear();
    r.imap.state.lock().unwrap().password = PASSWORD.to_owned();
    assert!(guard.fetch(None).await.is_ok());
}

#[tokio::test]
async fn a_missing_keychain_item_fails_closed_without_connecting() {
    let imap = spawn(state(vec![("INBOX", 1, messages(&[1]))], PASSWORD));
    let mut config = IcloudConfig::new(ADDRESS);
    config.host = "127.0.0.1".into();
    config.port = imap.port;
    let audit: CycleAudit = Arc::new(|_, _| {});
    let connector = IcloudConnector::new(
        config,
        Arc::new(MemoryStore::default()),
        Arc::new(PlainOpener),
        audit,
    );
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &connector,
        hooks: &hooks,
    };
    assert!(matches!(
        guard.fetch(None).await,
        Err(CallError::BreakerOpened(_))
    ));
    assert_eq!(imap.state.lock().unwrap().login_attempts, 0);
}

#[tokio::test]
async fn transient_failures_use_the_cycle_budget_and_never_trip_the_breaker() {
    // One dropped connection is absorbed by the second attempt.
    let r = rig(&[1]);
    r.imap.state.lock().unwrap().drop_next = 1;
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &r.connector,
        hooks: &hooks,
    };
    assert!(guard.fetch(None).await.is_ok());

    // Dropping every connection ends the cycle after exactly the budget.
    let r = rig(&[1]);
    r.imap.state.lock().unwrap().drop_next = 100;
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &r.connector,
        hooks: &hooks,
    };
    assert!(matches!(
        guard.fetch(None).await,
        Err(CallError::TransientExhausted(_))
    ));
    assert_eq!(
        100 - r.imap.state.lock().unwrap().drop_next,
        TRANSIENT_ATTEMPTS_PER_CYCLE
    );
    assert!(hooks.engaged.lock().unwrap().is_empty());
}

#[test]
fn a_certificate_that_does_not_validate_is_refused_as_transient() {
    // The fake speaks plaintext, so the TLS handshake cannot succeed: the connection is refused.
    let imap = spawn(state(vec![("INBOX", 1, vec![])], PASSWORD));
    let mut config = IcloudConfig::new(ADDRESS);
    config.host = "127.0.0.1".into();
    config.port = imap.port;
    let result = TlsOpener
        .open(&config, PASSWORD)
        .err()
        .expect("must not connect");
    assert!(matches!(result, ImapFailure::Transient(_)), "{result:?}");
    assert_eq!(
        imap.state.lock().unwrap().login_attempts,
        0,
        "no credentials were sent"
    );
}

#[tokio::test]
async fn neither_the_password_nor_message_text_reaches_audit_lines_errors_or_the_command_log() {
    let wrong = rig_with(
        state(vec![("INBOX", 1, messages(&[1]))], "something-else"),
        |_| {},
    );
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &wrong.connector,
        hooks: &hooks,
    };
    let failure = format!("{:?}", guard.fetch(None).await);

    let ok = rig(&[1, 2]);
    ok.connector.fetch(None).await.unwrap();

    let mut haystack = vec![failure];
    haystack.extend(hooks.audits.lock().unwrap().clone());
    haystack.extend(hooks.notices.lock().unwrap().clone());
    haystack.extend(ok.audits.lock().unwrap().clone());
    haystack.extend(commands(&ok));
    haystack.extend(commands(&wrong));
    let all = haystack.join("\n");
    for secret in [
        PASSWORD,
        "something-else",
        "SECRET BODY TEXT",
        "Subject number",
        "ana@example.org",
    ] {
        assert!(!all.contains(secret), "leaked {secret:?} in:\n{all}");
    }
    assert!(ok.audits.lock().unwrap()[0].starts_with("ok INBOX:new=2"));
}

#[tokio::test]
async fn drafts_are_still_refused_by_the_guard_before_anything_reaches_the_server() {
    let r = rig(&[]);
    let hooks = TestHooks::default();
    let guard = Guarded {
        connector: &r.connector,
        hooks: &hooks,
    };
    let draft = DraftToSend {
        thread_id: None,
        to: Some("a@x.org".into()),
        body: "hi".into(),
        ..Default::default()
    };
    assert_eq!(
        guard.create_draft(&draft).await,
        Err(CallError::CapabilityMissing("draft.create"))
    );
    assert!(commands(&r).is_empty(), "nothing reached the server");
    let _ = Cursor("x".into());
}

#[tokio::test]
async fn plain_text_wins_and_html_is_fetched_only_when_there_is_no_plain_part() {
    let mut both = TestMessage::simple(1, "Both", "", "the plain text");
    both.parts
        .push(TestPart::html("<p>the <i>html</i> version</p>"));
    let mut html_only = TestMessage::simple(2, "Html only", "", "");
    html_only.parts = vec![TestPart::html(
        "<style>p{color:red}</style><p>Only <b>HTML</b> here</p><img src=\"http://tracker.example/p.gif\">",
    )];
    let mut pdf_only = TestMessage::simple(3, "Just a file", "", "");
    pdf_only.parts = vec![TestPart::pdf("scan.pdf", 4000)];
    let r = rig_with(
        state(
            vec![("INBOX", 42, vec![both, html_only, pdf_only])],
            PASSWORD,
        ),
        |_| {},
    );

    let batch = r.connector.fetch(None).await.unwrap();
    let body = |id: &str| {
        batch
            .messages
            .iter()
            .find(|m| m.external_id == id)
            .unwrap()
            .body_text
            .clone()
    };
    assert_eq!(body("m1@example.org"), "the plain text");
    assert_eq!(
        body("m2@example.org")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        "Only HTML here"
    );
    assert!(
        body("m3@example.org").starts_with("[body unavailable]"),
        "neither part: {}",
        body("m3@example.org")
    );
    assert!(
        body("m3@example.org").contains("[attachment: scan.pdf (application/pdf, 4000 bytes) #")
    );

    let cmds = commands(&r);
    // Message 1 has both parts: only part 1 (plain) is fetched, never part 2 (html).
    assert!(cmds
        .iter()
        .any(|c| c.starts_with("UID FETCH 1 (BODY.PEEK[1]")));
    assert!(
        !cmds
            .iter()
            .any(|c| c.starts_with("UID FETCH 1 (BODY.PEEK[2]")),
        "{cmds:?}"
    );
    // Message 3 has no text part at all: its attachment is never requested.
    assert!(
        !cmds
            .iter()
            .any(|c| c.starts_with("UID FETCH 3 (BODY.PEEK[")),
        "{cmds:?}"
    );
    // The page the HTML links to is never contacted, and nothing the server stores changes.
    assert!(!batch
        .messages
        .iter()
        .any(|m| m.body_text.contains("tracker.example")));
}

#[tokio::test]
async fn opening_a_thread_reads_its_older_messages_from_every_folder() {
    let msg = |uid: u32, subject: &str| TestMessage::simple(uid, subject, "", "BODY");
    let r = rig_with(
        state(
            vec![
                (
                    "INBOX",
                    42,
                    vec![
                        msg(5, "Invoice 5"),
                        msg(6, "Re: Invoice 5"),
                        msg(7, "Invoice 55"),
                    ],
                ),
                (
                    "Archive",
                    9,
                    vec![msg(1, "Invoice 5"), msg(2, "Other topic")],
                ),
            ],
            PASSWORD,
        ),
        |_| {},
    );
    let request = HistoryRequest {
        thread_external_id: "m5@example.org".into(),
        subject: "Re: Invoice 5".into(),
        last_activity_at: 1_700_000_000,
        oldest_known_at: 1_700_000_000,
        known: 2,
    };
    let batch = r.connector.fetch_thread_history(&request).await.unwrap();
    // the same conversation from INBOX and Archive; "Invoice 55" and "Other topic" stay out
    let mut ids: Vec<&str> = batch
        .messages
        .iter()
        .map(|m| m.external_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["m1@example.org", "m5@example.org", "m6@example.org"]);
    assert!(batch
        .messages
        .iter()
        .all(|m| m.thread_id == "m5@example.org"));
    assert_eq!(batch.threads.len(), 1);
    assert_eq!(batch.threads[0].external_id, "m5@example.org");
    assert_eq!(batch.threads[0].last_activity_at, 1_700_000_000);
    assert!(
        batch.next_cursor.is_none(),
        "a history read never moves the cursor"
    );
    // read-only: nothing is changed in the mailbox
    let cmds = commands(&r).join("\n");
    assert!(
        !cmds.contains("STORE") && !cmds.contains("MOVE") && !cmds.contains("EXPUNGE"),
        "{cmds}"
    );
}
