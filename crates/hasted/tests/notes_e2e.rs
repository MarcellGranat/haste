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
