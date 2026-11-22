//! The local AI helpers end to end: a real daemon, a fake `apfel` (no model is ever started).

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haste_core::ai::{AiOutcome, Pressure, RewriteMode, Sample, TodoPriority};
use haste_core::model::{Message, Source};
use haste_proto::{AiState, ErrorCode, Request, RequestBody, Response};
use hasted::apfel::{CallResult, Prompt, Runner, Sampler, Settings};
use hasted::notifier::RecordingNotifier;
use hasted::{start, DaemonConfig, DaemonHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const SECRET: &str = "SECRET-BODY-TEXT";
const MEMORY_SECRET: &str = "MEMORY-SECRET-TEXT";

struct Harness {
    _dir: tempfile::TempDir,
    daemon: DaemonHandle,
    human: String,
    runner: Arc<FakeRunner>,
    sampler: Arc<FakeSampler>,
}

#[derive(Default)]
struct FakeRunner {
    prompts: Mutex<Vec<(Prompt, bool)>>,
    completes: AtomicUsize,
    refuse: AtomicBool,
    bad_todo: AtomicBool,
}

impl Runner for FakeRunner {
    fn available(&self) -> bool {
        true
    }

    fn count_tokens(&self, text: &str) -> Option<usize> {
        Some(text.len() / 4)
    }

    fn complete(&self, prompt: &Prompt, schema: Option<&Path>, _max_tokens: u32) -> CallResult {
        self.completes.fetch_add(1, Ordering::SeqCst);
        self.prompts
            .lock()
            .unwrap()
            .push((prompt.clone(), schema.is_some()));
        if self.refuse.load(Ordering::SeqCst) {
            return CallResult {
                outcome: AiOutcome::Refused,
                text: String::new(),
            };
        }
        let text = match (schema.is_some(), self.bad_todo.load(Ordering::SeqCst)) {
            (true, true) => "Sure, here is a task".to_owned(),
            (true, false) => r#"{"has_task":true,"title":"Send the form","notes":"By Friday","due":"2026-11-27","priority":"high"}"#.to_owned(),
            (false, _) => "Dear Kata, thank you.".to_owned(),
        };
        CallResult {
            outcome: AiOutcome::Ok,
            text,
        }
    }
}

struct FakeSampler {
    busy: AtomicBool,
    broken: AtomicBool,
}

impl Sampler for FakeSampler {
    fn sample(&self) -> Sample {
        if self.broken.load(Ordering::SeqCst) {
            return Sample {
                load1: None,
                ncpu: Some(12),
                free_mb: Some(20_000),
                pressure: Some(Pressure::Normal),
            };
        }
        Sample {
            load1: Some(if self.busy.load(Ordering::SeqCst) {
                9.0
            } else {
                1.0
            }),
            ncpu: Some(12),
            free_mb: Some(20_000),
            pressure: Some(Pressure::Normal),
        }
    }
}

async fn boot() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let notifier = Arc::new(RecordingNotifier::default());
    let mut config = DaemonConfig::new(&dir.path().join("haste"), notifier);
    config.expiry_tick = Duration::from_secs(3600);
    config.delivery_tick = Duration::from_secs(3600);
    let daemon = start(config).await.unwrap();
    let human = fs::read_to_string(daemon.state.config.human_token_path()).unwrap();
    let runner = Arc::new(FakeRunner::default());
    let sampler = Arc::new(FakeSampler {
        busy: AtomicBool::new(false),
        broken: AtomicBool::new(false),
    });
    daemon.state.apfel.set_runner(runner.clone());
    daemon.state.apfel.set_sampler(sampler.clone());
    daemon.state.apfel.set_settings(Settings {
        enabled: true,
        ..Settings::default()
    });
    Harness {
        _dir: dir,
        daemon,
        human,
        runner,
        sampler,
    }
}

impl Harness {
    async fn call(&self, token: &str, body: RequestBody) -> Response {
        let mut stream = UnixStream::connect(&self.daemon.socket_path).await.unwrap();
        let frame = haste_proto::encode_frame(&Request::new(token, body)).unwrap();
        stream.write_all(&frame).await.unwrap();
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header).await.unwrap();
        let mut body = vec![0_u8; haste_proto::parse_header(header).unwrap()];
        stream.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn suggestion(&self, thread: &str) -> Response {
        self.call(
            &self.human,
            RequestBody::AiSuggestionGet {
                thread_id: thread.into(),
            },
        )
        .await
    }

    /// A mail thread with `n` messages from Kata (every third one is the human's own reply).
    fn seed(&self, sender: &str, n: usize) -> String {
        let store = &self.daemon.state.store;
        let thread = store
            .upsert_thread(
                &Source::Gmail,
                &format!("ext-{sender}"),
                "Form",
                &[sender.into()],
                100 + n as i64,
            )
            .unwrap();
        for i in 0..n {
            let own = i % 3 == 2 && i + 1 != n;
            store
                .upsert_message(&Message {
                    id: String::new(),
                    thread_id: thread.id.clone(),
                    source: Source::Gmail,
                    external_id: format!("{sender}-m{i}"),
                    sender_person_id: None,
                    sender_identifier: Some(if own {
                        "me@x.org".into()
                    } else {
                        sender.into()
                    }),
                    body_text: format!("{SECRET} message {i}"),
                    sent_at: 100 + i as i64,
                    is_own: own,
                })
                .unwrap();
        }
        thread.id
    }

    fn allow(&self, pattern: &str) {
        fs::write(
            self.daemon.state.config.data_dir.join("allowed_senders"),
            pattern,
        )
        .unwrap();
    }

    fn memory(&self, file: &str, yaml: &str) {
        let dir = self.daemon.state.config.data_dir.join("memories");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file), yaml).unwrap();
    }

    fn tick(&self) {
        hasted::apfel::tick(&self.daemon.state);
    }

    fn audit_text(&self) -> String {
        fs::read_to_string(self.daemon.state.config.audit_path()).unwrap_or_default()
    }
}

fn states(r: &Response) -> (AiState, AiState) {
    match r {
        Response::AiSuggestion {
            draft_state,
            todo_state,
            ..
        } => (*draft_state, *todo_state),
        other => panic!("{other:?}"),
    }
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { error } => Some(error.code),
        _ => None,
    }
}

#[tokio::test]
async fn agents_and_machines_cannot_ask_for_ai_results() {
    let h = boot().await;
    let thread = h.seed("kata@x.org", 3);
    let reg = h
        .call(
            &h.human,
            RequestBody::RegisterPrincipal {
                principal_id: "agent-a".into(),
                kind: haste_core::policy::PrincipalKind::Agent,
                allowed_actions: vec!["read".into()],
            },
        )
        .await;
    let Response::Registered { token, .. } = reg else {
        panic!("{reg:?}")
    };
    for body in [
        RequestBody::AiSuggestionGet {
            thread_id: thread.clone(),
        },
        RequestBody::AiRewrite {
            thread_id: thread.clone(),
            draft: "hi".into(),
            mode: RewriteMode::Grammar,
            instruction: None,
        },
        RequestBody::AiMemoriesList {
            thread_id: thread.clone(),
        },
    ] {
        let r = h.call(&token, body).await;
        assert_eq!(code(&r), Some(ErrorCode::Denied), "{r:?}");
    }
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_approved_thread_gets_a_draft_and_a_todo_and_the_cache_prevents_reruns() {
    let h = boot().await;
    h.allow(r"^kata@x\.org$");
    h.memory(
        "kata.yml",
        &format!("id: kata\ntitle: Kata is a client\nsenders: ['kata']\ntext: {MEMORY_SECRET}\n"),
    );
    h.memory(
        "other.yml",
        "id: other\ntitle: Other\nsenders: ['someone else']\ntext: NOT-FOR-KATA\n",
    );
    let thread = h.seed("kata@x.org", 8);

    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::Pending, AiState::Pending)
    );
    assert_eq!(
        h.runner.completes.load(Ordering::SeqCst),
        0,
        "nothing runs on open"
    );
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 2);

    let r = h.suggestion(&thread).await;
    let Response::AiSuggestion { draft, todo, .. } = &r else {
        panic!("{r:?}")
    };
    assert_eq!(states(&r), (AiState::Ready, AiState::Ready));
    assert_eq!(draft.as_deref(), Some("Dear Kata, thank you."));
    let todo = todo.as_ref().unwrap();
    assert_eq!(
        (todo.title.as_str(), todo.priority),
        ("Send the form", TodoPriority::High)
    );

    // the prompts: the latest message and 4 earlier ones, the matching memory only
    {
        let prompts = h.runner.prompts.lock().unwrap();
        let (draft_prompt, with_schema) = prompts.iter().find(|(_, s)| !s).unwrap();
        assert!(!with_schema);
        assert_eq!(draft_prompt.user.matches(SECRET).count(), 5);
        assert!(draft_prompt.user.contains("message 7"));
        assert!(
            !draft_prompt.user.contains("message 2"),
            "older than 4 earlier ones"
        );
        assert!(draft_prompt.system.contains(MEMORY_SECRET));
        assert!(!draft_prompt.system.contains("NOT-FOR-KATA"));
    }

    // opened again with nothing new: no model call
    h.suggestion(&thread).await;
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 2);

    // new mail: a new version, so both are computed again
    let thread = h.seed("kata@x.org", 9);
    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::Pending, AiState::Pending)
    );
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 4);

    // editing a memory invalidates the cache too
    h.memory(
        "kata.yml",
        &format!(
            "id: kata\ntitle: Kata is a client\nsenders: ['kata']\ntext: {MEMORY_SECRET} edited\n"
        ),
    );
    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::Pending, AiState::Pending)
    );

    // the audit trail and the memory list carry no content
    let audit = h.audit_text();
    assert!(audit.contains("ai.call"));
    assert!(
        !audit.contains(SECRET) && !audit.contains(MEMORY_SECRET) && !audit.contains("Dear Kata")
    );
    assert!(
        !audit.contains(&thread),
        "only a short hash of the thread id"
    );
    let listed = h
        .call(&h.human, RequestBody::AiMemoriesList { thread_id: thread })
        .await;
    let Response::AiMemories { memories } = listed else {
        panic!("{listed:?}")
    };
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].id, "kata");
}

#[tokio::test]
async fn other_senders_and_answered_threads_queue_nothing() {
    let h = boot().await;
    h.allow(r"^kata@x\.org$");
    let stranger = h.seed("stranger@x.org", 3);
    assert_eq!(
        states(&h.suggestion(&stranger).await),
        (AiState::None, AiState::None)
    );
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 0);

    // the human already replied to Kata's last message: no draft is needed, the TODO still is
    let store = &h.daemon.state.store;
    let thread = h.seed("kata@x.org", 2);
    store
        .upsert_message(&Message {
            id: String::new(),
            thread_id: thread.clone(),
            source: Source::Gmail,
            external_id: "reply".into(),
            sender_person_id: None,
            sender_identifier: Some("me@x.org".into()),
            body_text: "Thanks".into(),
            sent_at: 500,
            is_own: true,
        })
        .unwrap();
    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::None, AiState::Pending)
    );
}

#[tokio::test]
async fn background_jobs_wait_while_the_machine_is_busy() {
    let h = boot().await;
    h.allow(r"^kata@x\.org$");
    let thread = h.seed("kata@x.org", 3);
    h.suggestion(&thread).await;
    h.sampler.busy.store(true, Ordering::SeqCst);
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 0, "busy machine");
    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::Pending, AiState::Pending)
    );
}

#[tokio::test]
async fn background_jobs_fail_closed_when_the_load_cannot_be_read() {
    let h = boot().await;
    h.allow(r"^kata@x\.org$");
    let thread = h.seed("kata@x.org", 3);
    h.suggestion(&thread).await;
    h.sampler.broken.store(true, Ordering::SeqCst);
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 0);
    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::Pending, AiState::Pending)
    );
}

#[tokio::test]
async fn a_free_machine_runs_the_jobs_and_a_refusal_is_remembered() {
    let h = boot().await;
    h.allow(r"^kata@x\.org$");
    h.runner.refuse.store(true, Ordering::SeqCst);
    let thread = h.seed("kata@x.org", 3);
    h.suggestion(&thread).await;
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 2);
    // refused: nothing shown, and not asked again for this version
    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::None, AiState::None)
    );
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_invalid_todo_answer_is_discarded() {
    let h = boot().await;
    h.allow(r"^kata@x\.org$");
    h.runner.bad_todo.store(true, Ordering::SeqCst);
    let thread = h.seed("kata@x.org", 3);
    h.suggestion(&thread).await;
    h.tick();
    let r = h.suggestion(&thread).await;
    let Response::AiSuggestion {
        todo, todo_state, ..
    } = r
    else {
        panic!("{r:?}")
    };
    assert!(todo.is_none());
    assert_eq!(
        todo_state,
        AiState::Pending,
        "invalid output is not cached; it is tried again later"
    );
}

#[tokio::test]
async fn rewrite_runs_on_a_busy_machine_and_is_never_cached() {
    let h = boot().await;
    h.sampler.busy.store(true, Ordering::SeqCst);
    let thread = h.seed("anyone@x.org", 5);
    let ask = |draft: &str| {
        h.call(
            &h.human,
            RequestBody::AiRewrite {
                thread_id: thread.clone(),
                draft: draft.into(),
                mode: RewriteMode::Custom,
                instruction: Some("make it shorter".into()),
            },
        )
    };
    for _ in 0..2 {
        let r = ask("hello there").await;
        let Response::AiRewritten { outcome, text } = r else {
            panic!("{r:?}")
        };
        assert_eq!(outcome, AiOutcome::Ok);
        assert_eq!(text.as_deref(), Some("Dear Kata, thank you."));
    }
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 2, "no cache");
    {
        let prompts = h.runner.prompts.lock().unwrap();
        let (p, _) = &prompts[0];
        assert!(p.system.contains("make it shorter"));
        assert_eq!(
            p.user.matches("message ").count(),
            2,
            "at most two earlier messages"
        );
        assert!(p.user.contains("hello there"));
    }
    assert_eq!(
        code(&ask("   ").await),
        Some(ErrorCode::Validation),
        "empty draft: no call"
    );
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 2);
    let audit = h.audit_text();
    assert!(!audit.contains("hello there") && !audit.contains("make it shorter"));
}

#[tokio::test]
async fn nothing_runs_while_the_helpers_are_switched_off() {
    let h = boot().await;
    h.daemon.state.apfel.set_settings(Settings::default());
    h.allow(r"^kata@x\.org$");
    let thread = h.seed("kata@x.org", 3);
    assert_eq!(
        states(&h.suggestion(&thread).await),
        (AiState::Unavailable, AiState::Unavailable)
    );
    let r = h
        .call(
            &h.human,
            RequestBody::AiRewrite {
                thread_id: thread,
                draft: "hi".into(),
                mode: RewriteMode::Formal,
                instruction: None,
            },
        )
        .await;
    assert!(matches!(
        r,
        Response::AiRewritten {
            outcome: AiOutcome::Unavailable,
            text: None
        }
    ));
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_chat_thread_is_never_approved_even_from_a_sender_that_is_listed() {
    // Auto draft and TODO are for mail from `allowed_senders`. A WhatsApp or Messenger thread
    // from the same person queues nothing (the daemon log says `not_approved` for it).
    let h = boot().await;
    h.allow(r"kata");
    let store = &h.daemon.state.store;
    let thread = store
        .upsert_thread(
            &Source::Whatsapp,
            "wa-kata",
            "Kata Biró",
            &["Kata Biró".into()],
            200,
        )
        .unwrap();
    store
        .upsert_message(&Message {
            id: String::new(),
            thread_id: thread.id.clone(),
            source: Source::Whatsapp,
            external_id: "wa-1".into(),
            sender_person_id: None,
            sender_identifier: Some("Kata Biró".into()),
            body_text: "Szia, tudsz segiteni?".into(),
            sent_at: 200,
            is_own: false,
        })
        .unwrap();
    assert_eq!(
        states(&h.suggestion(&thread.id).await),
        (AiState::None, AiState::None)
    );
    h.tick();
    assert_eq!(h.runner.completes.load(Ordering::SeqCst), 0);
    // the same name on a mail thread is approved
    let mail = h.seed("kata@x.org", 2);
    assert_eq!(
        states(&h.suggestion(&mail).await),
        (AiState::Pending, AiState::Pending)
    );
}

#[tokio::test]
async fn the_prompt_files_are_written_and_edits_are_used_without_a_restart() {
    let h = boot().await;
    h.allow(r"^kata@x\.org$");
    let thread = h.seed("kata@x.org", 2);
    h.suggestion(&thread).await;
    h.tick();
    let dir = h.daemon.state.config.data_dir.join("prompts");
    for file in ["draft.txt", "todo.txt", "rewrite.txt", "rewrite_modes.txt"] {
        assert!(dir.join(file).exists(), "{file}");
    }
    {
        let prompts = h.runner.prompts.lock().unwrap();
        // the defaults carry the language and accent rule
        assert!(prompts
            .iter()
            .all(|(p, _)| p.system.contains("köszönöm szépen")));
        assert!(prompts.iter().all(|(p, _)| !p.system.contains("{{")));
    }

    // an edited rewrite mode line is used by the next rewrite
    fs::write(
        dir.join("rewrite_modes.txt"),
        "grammar: ONLY-FIX-COMMAS in my way.\n",
    )
    .unwrap();
    let r = h
        .call(
            &h.human,
            RequestBody::AiRewrite {
                thread_id: thread.clone(),
                draft: "koszonom szepen".into(),
                mode: RewriteMode::Grammar,
                instruction: None,
            },
        )
        .await;
    assert!(matches!(r, Response::AiRewritten { .. }), "{r:?}");
    let prompts = h.runner.prompts.lock().unwrap();
    let (p, _) = prompts.last().unwrap();
    assert!(p.system.contains("ONLY-FIX-COMMAS in my way."));
    assert!(p.system.contains("repair the missing accents"));
}
