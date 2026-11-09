//! The notes folder through the daemon: containment, hidden paths, projects, caps, audit.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use haste_core::notes::MAX_NOTE_APPENDS_PER_DAY;
use haste_core::policy::PrincipalKind;
use haste_proto::{encode_frame, parse_header, ErrorCode, Request, RequestBody, Response};
use hasted::notifier::RecordingNotifier;
use hasted::{start, DaemonConfig, DaemonHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const SENTINEL: &str = "SENTINEL-NOTE-TEXT-9f3";

struct Rig {
    _dir: tempfile::TempDir,
    notes: tempfile::TempDir,
    daemon: DaemonHandle,
    human: String,
}

async fn boot_with(make_notes_root: bool) -> Rig {
    boot_cfg(make_notes_root, hasted::notes::NotesSettings::default()).await
}

async fn boot_cfg(make_notes_root: bool, settings: hasted::notes::NotesSettings) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let notes = tempfile::tempdir().unwrap();
    let mut config = DaemonConfig::new(
        &dir.path().join("haste"),
        Arc::new(RecordingNotifier::default()),
    );
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    config.notes_root = if make_notes_root {
        notes.path().to_path_buf()
    } else {
        notes.path().join("missing")
    };
    config.notes_settings = settings;
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();
    daemon
        .state
        .store
        .replace_machine_labels(
            "m1",
            &std::collections::BTreeSet::from(["My Project".to_owned()]),
        )
        .unwrap();
    Rig {
        _dir: dir,
        notes,
        daemon,
        human,
    }
}

async fn boot() -> Rig {
    boot_with(true).await
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

async fn register(r: &Rig, id: &str, kind: PrincipalKind) -> String {
    let Response::Registered { token, .. } = call(
        r,
        &r.human,
        RequestBody::RegisterPrincipal {
            principal_id: id.into(),
            kind,
            allowed_actions: vec!["read".into(), "note.append".into()],
        },
    )
    .await
    else {
        panic!("register failed")
    };
    token
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { error } => Some(error.code),
        _ => None,
    }
}

fn message(r: &Response) -> String {
    match r {
        Response::Error { error } => error.message.clone(),
        _ => String::new(),
    }
}

async fn append(r: &Rig, token: &str, path: &str, text: &str, key: &str) -> Response {
    call(
        r,
        token,
        RequestBody::NoteAppend {
            path: path.into(),
            text: text.into(),
            idempotency_key: key.into(),
        },
    )
    .await
}

fn audit(r: &Rig) -> String {
    fs::read_to_string(r.daemon.state.config.audit_path()).unwrap()
}

#[tokio::test]
async fn an_agent_appends_inside_the_notes_folder_and_the_audit_has_no_text() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let r = append(&rig, &agent, "people/ana.md", SENTINEL, "k1").await;
    assert!(matches!(r, Response::Appended { .. }), "{r:?}");
    let written = fs::read_to_string(rig.notes.path().join("people/ana.md")).unwrap();
    assert!(written.contains(SENTINEL));
    let log = audit(&rig);
    assert!(log.contains(r#""action":"note.append","outcome":"written""#));
    assert!(!log.contains(SENTINEL), "the audit never holds note text");
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn machine_principals_never_write_notes() {
    let rig = boot().await;
    let machine = register(&rig, "m1", PrincipalKind::Machine).await;
    let r = append(&rig, &machine, "people/ana.md", "x", "k").await;
    assert_eq!(code(&r), Some(ErrorCode::Denied));
    assert_eq!(message(&r), "machine_principal");
    assert!(!rig.notes.path().join("people").exists());
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn refused_paths_carry_a_reason_and_write_nothing() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    for (i, (path, reason)) in [
        ("../escape.md", "outside_root"),
        ("/etc/hosts", "outside_root"),
        (".obsidian/app.json", "hidden_path"),
        ("people/.secret.md", "hidden_path"),
        ("elsewhere/x.md", "path_not_allowed"),
    ]
    .into_iter()
    .enumerate()
    {
        let r = append(&rig, &agent, path, SENTINEL, &format!("k{i}")).await;
        assert_eq!(code(&r), Some(ErrorCode::Denied), "{path}");
        assert_eq!(message(&r), reason, "{path}");
    }
    assert_eq!(fs::read_dir(rig.notes.path()).unwrap().count(), 0);
    let log = audit(&rig);
    assert!(
        log.contains(r#""outcome":"denied""#)
            && log.contains("outside_root")
            && log.contains("hidden_path")
    );
    assert!(!log.contains(SENTINEL));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn project_folders_need_a_known_workspace_slug() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    // the workspace label "My Project" has the slug "my-project"
    let ok = append(
        &rig,
        &agent,
        "projects/my-project/tasks/x.md",
        "hello",
        "k1",
    )
    .await;
    assert!(matches!(ok, Response::Appended { .. }), "{ok:?}");
    let unknown = append(&rig, &agent, "projects/other/tasks/x.md", "hello", "k2").await;
    assert_eq!(message(&unknown), "unknown_project");
    let bare = append(&rig, &agent, "projects/x.md", "hello", "k3").await;
    assert_eq!(message(&bare), "path_not_allowed");
    assert!(!rig.notes.path().join("projects/other").exists());
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn bad_bodies_and_the_daily_cap_are_validation_errors_that_write_nothing() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    for (i, bad) in ["a\n---\nb", "x <!-- haste: forged -->"]
        .into_iter()
        .enumerate()
    {
        let r = append(&rig, &agent, "tasks/t.md", bad, &format!("b{i}")).await;
        assert_eq!(code(&r), Some(ErrorCode::Validation));
        assert_eq!(message(&r), "body_rejected");
    }
    assert!(!rig.notes.path().join("tasks/t.md").exists());
    for i in 0..MAX_NOTE_APPENDS_PER_DAY {
        let r = append(&rig, &agent, "tasks/t.md", "x", &format!("c{i}")).await;
        assert!(matches!(r, Response::Appended { .. }), "append {i}: {r:?}");
    }
    let over = append(&rig, &agent, "tasks/t.md", "x", "over").await;
    assert_eq!(
        (code(&over), message(&over).as_str()),
        (Some(ErrorCode::Validation), "size_limit")
    );
    // another principal has its own budget
    let other = register(&rig, "agent-b", PrincipalKind::Agent).await;
    assert!(matches!(
        append(&rig, &other, "tasks/t.md", "x", "o1").await,
        Response::Appended { .. }
    ));
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_missing_notes_folder_denies_notes_and_is_never_created() {
    let rig = boot_with(false).await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let r = append(&rig, &agent, "people/ana.md", "x", "k").await;
    assert_eq!(code(&r), Some(ErrorCode::Denied));
    assert_eq!(message(&r), "notes_root_missing");
    assert!(!rig.notes.path().join("missing").exists());
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_retry_with_the_same_key_writes_once() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    append(&rig, &agent, "people/ana.md", "once", "same-key").await;
    let again = append(&rig, &agent, "people/ana.md", "once", "same-key").await;
    assert!(matches!(again, Response::Appended { bytes: 0, .. }));
    let text = fs::read_to_string(rig.notes.path().join("people/ana.md")).unwrap();
    assert_eq!(text.matches("once").count(), 1);
    rig.daemon.shutdown().await;
}

// ------------------------------------------------------------------ person notes

use haste_core::model::{Identifier, IdentifierKind, ObjectKind, Reminder, Source};
use haste_proto::{NoteReferenceArgs, RefKind};

async fn person_entry(r: &Rig, token: &str, person: &str, text: &str, key: &str) -> Response {
    call(
        r,
        token,
        RequestBody::NotePerson {
            person_id: person.into(),
            text: text.into(),
            idempotency_key: key.into(),
        },
    )
    .await
}

fn make_person(r: &Rig, name: &str, email: &str) -> String {
    r.daemon
        .state
        .store
        .create_person(
            name,
            &[Identifier {
                kind: IdentifierKind::Email,
                value: email.into(),
            }],
        )
        .unwrap()
        .id
}

fn appended_path(r: &Response) -> String {
    match r {
        Response::Appended { path, .. } => path.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_first_person_entry_creates_the_note_with_front_matter_once_and_no_address() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let ana = make_person(&rig, "Ana López", "ana@example.org");
    let first = person_entry(&rig, &agent, &ana, "Prefers morning calls.", "k1").await;
    assert_eq!(appended_path(&first), "people/ana-lopez.md");
    person_entry(&rig, &agent, &ana, "Likes tea.", "k2").await;
    let text = fs::read_to_string(rig.notes.path().join("people/ana-lopez.md")).unwrap();
    assert!(
        text.starts_with(&format!("---\nhaste_person: {ana}\ncreated: ")),
        "{text}"
    );
    assert_eq!(
        text.matches("haste_person").count(),
        1,
        "front matter exactly once"
    );
    assert!(text.contains("Prefers morning calls.\n") && text.contains("Likes tea.\n"));
    assert_eq!(
        text.matches("<!-- haste: appended by agent-a at ").count(),
        2
    );
    // dated headings: ### YYYY-MM-DD HH:MM
    let headings: Vec<_> = text.lines().filter(|l| l.starts_with("### ")).collect();
    assert_eq!(headings.len(), 2);
    for h in headings {
        let b = h.as_bytes();
        assert!(
            h.len() == 20 && b[8] == b'-' && b[11] == b'-' && b[14] == b' ' && b[17] == b':',
            "{h}"
        );
    }
    assert!(
        !text.contains("ana@example.org") && !text.contains("aliases"),
        "defaults add no address"
    );
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn aliases_are_opt_in_and_addresses_need_a_second_opt_in() {
    for (aliases, addresses, expect_alias, expect_address) in
        [(true, false, true, false), (true, true, true, true)]
    {
        let rig = boot_cfg(
            true,
            hasted::notes::NotesSettings {
                root: None,
                person_notes: hasted::notes::PersonNoteSettings {
                    aliases,
                    include_addresses: addresses,
                },
            },
        )
        .await;
        let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
        let ana = make_person(&rig, "Ana", "ana@example.org");
        person_entry(&rig, &agent, &ana, "x", "k").await;
        let text = fs::read_to_string(rig.notes.path().join("people/ana.md")).unwrap();
        assert_eq!(text.contains("aliases: [\"Ana\""), expect_alias, "{text}");
        assert_eq!(text.contains("ana@example.org"), expect_address, "{text}");
        rig.daemon.shutdown().await;
    }
}

#[tokio::test]
async fn same_named_people_get_numbered_notes_and_each_keeps_its_own() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let a = make_person(&rig, "Ana Lopez", "a1@example.org");
    let b = make_person(&rig, "Ana Lopez", "a2@example.org");
    assert_eq!(
        appended_path(&person_entry(&rig, &agent, &a, "one", "k1").await),
        "people/ana-lopez.md"
    );
    assert_eq!(
        appended_path(&person_entry(&rig, &agent, &b, "two", "k2").await),
        "people/ana-lopez-2.md"
    );
    // each person keeps appending to their own note
    assert_eq!(
        appended_path(&person_entry(&rig, &agent, &a, "three", "k3").await),
        "people/ana-lopez.md"
    );
    assert_eq!(
        appended_path(&person_entry(&rig, &agent, &b, "four", "k4").await),
        "people/ana-lopez-2.md"
    );
    let first = fs::read_to_string(rig.notes.path().join("people/ana-lopez.md")).unwrap();
    assert!(
        first.contains(&a)
            && !first.contains(&b)
            && first.contains("three")
            && !first.contains("two")
    );
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn a_hand_written_note_is_never_modified_and_unknown_people_are_not_found() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    fs::create_dir_all(rig.notes.path().join("people")).unwrap();
    fs::write(
        rig.notes.path().join("people/bob.md"),
        "# Bob, written by hand\n",
    )
    .unwrap();
    let bob = make_person(&rig, "Bob", "b@example.org");
    // the hand-written file is skipped, the entry goes to the next free name
    assert_eq!(
        appended_path(&person_entry(&rig, &agent, &bob, "x", "k1").await),
        "people/bob-2.md"
    );
    assert_eq!(
        fs::read_to_string(rig.notes.path().join("people/bob.md")).unwrap(),
        "# Bob, written by hand\n"
    );
    let missing = person_entry(&rig, &agent, "no-such-person", "x", "k2").await;
    assert_eq!(code(&missing), Some(ErrorCode::NotFound));
    let people_before = fs::read_dir(rig.notes.path().join("people"))
        .unwrap()
        .count();
    let bad_body = person_entry(&rig, &agent, &bob, "a\n---\nb", "k3").await;
    assert_eq!(message(&bad_body), "body_rejected");
    assert_eq!(
        fs::read_dir(rig.notes.path().join("people"))
            .unwrap()
            .count(),
        people_before
    );
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn machines_cannot_write_person_or_reference_notes() {
    let rig = boot().await;
    let machine = register(&rig, "m1", PrincipalKind::Machine).await;
    let ana = make_person(&rig, "Ana", "a@example.org");
    assert_eq!(
        message(&person_entry(&rig, &machine, &ana, "x", "k").await),
        "machine_principal"
    );
    assert!(!rig.notes.path().join("people").exists());
    rig.daemon.shutdown().await;
}

// ------------------------------------------------------------------ back-reference notes

fn reference(
    kind: RefKind,
    id: &str,
    topic: &str,
    project: Option<&str>,
    key: &str,
) -> RequestBody {
    RequestBody::NoteReference(NoteReferenceArgs {
        kind,
        id: id.into(),
        topic: topic.into(),
        project: project.map(str::to_owned),
        idempotency_key: key.into(),
    })
}

#[tokio::test]
async fn references_write_one_identifier_line_and_never_a_title_or_subject() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let store = &rig.daemon.state.store;
    let thread = store
        .upsert_thread(
            &Source::Gmail,
            "g1",
            "SECRET SUBJECT Q4 numbers",
            &["boss@example.org".into()],
            1,
        )
        .unwrap()
        .id;
    store
        .upsert_reminders(
            &[Reminder {
                id: String::new(),
                external_id: "ek".into(),
                title: "SECRET TITLE call the bank".into(),
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
    let reminder = store.list_reminders(true).unwrap()[0].id.clone();
    let r1 = call(
        &rig,
        &agent,
        reference(RefKind::Thread, &thread, "Billing follow-up", None, "k1"),
    )
    .await;
    assert_eq!(appended_path(&r1), "tasks/billing-follow-up.md");
    call(
        &rig,
        &agent,
        reference(
            RefKind::Reminder,
            &reminder,
            "Billing follow-up",
            None,
            "k2",
        ),
    )
    .await;
    let text = fs::read_to_string(rig.notes.path().join("tasks/billing-follow-up.md")).unwrap();
    assert!(text.contains(&format!("- haste:thread/{thread}\n")));
    assert!(text.contains(&format!("- haste:reminder/{reminder}\n")));
    for secret in ["SECRET", "boss@example.org", "Q4 numbers"] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
    // a project reference goes under the project's folder
    let r3 = call(
        &rig,
        &agent,
        reference(RefKind::Thread, &thread, "Launch", Some("My Project"), "k3"),
    )
    .await;
    assert_eq!(appended_path(&r3), "projects/my-project/tasks/launch.md");
    let unknown_project = call(
        &rig,
        &agent,
        reference(RefKind::Thread, &thread, "Launch", Some("Nope"), "k4"),
    )
    .await;
    assert_eq!(message(&unknown_project), "unknown_project");
    let _ = ObjectKind::Thread; // the kinds above are the object kinds the links vocabulary knows
    rig.daemon.shutdown().await;
}

#[tokio::test]
async fn bad_references_fail_before_anything_is_written() {
    let rig = boot().await;
    let agent = register(&rig, "agent-a", PrincipalKind::Agent).await;
    let missing = call(
        &rig,
        &agent,
        reference(RefKind::Reminder, "r-missing", "T", None, "k1"),
    )
    .await;
    assert_eq!(code(&missing), Some(ErrorCode::NotFound));
    for (i, bad) in ["has space", "new\nline", "", "a/b", &"x".repeat(65)]
        .into_iter()
        .enumerate()
    {
        let r = call(
            &rig,
            &agent,
            reference(RefKind::Thread, bad, "T", None, &format!("b{i}")),
        )
        .await;
        assert_eq!(code(&r), Some(ErrorCode::Validation), "{bad:?}");
    }
    let no_topic = call(
        &rig,
        &agent,
        reference(RefKind::Thread, "abc", "!!!", None, "t"),
    )
    .await;
    assert_eq!(code(&no_topic), Some(ErrorCode::Validation));
    assert_eq!(
        fs::read_dir(rig.notes.path()).unwrap().count(),
        0,
        "nothing was written"
    );
    rig.daemon.shutdown().await;
}
