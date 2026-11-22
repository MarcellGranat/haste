//! The local AI helpers: the daemon is the only caller of the `apfel` CLI (Apple's on-device
//! model). It runs one process per call, one call at a time, with a timeout, never puts message
//! or memory text on the command line, caches results by thread version and writes one log line
//! and one audit record per call, with fixed labels and numbers only.
//!
//! Three tools: `draft` and `todo` (background, only for mail from approved senders, only when the
//! machine has room) and `rewrite` (the human asked, always runs).

use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use haste_core::ai::{
    self, AiOutcome, GateDecision, GateLimits, Memory, PassedMemory, Pressure, PromptParts,
    RewriteMode, Sample, TodoSuggestion,
};
use haste_core::ignore::{IgnoreList, ALLOWED_FILE};
use haste_core::log;
use haste_core::model::{Message, Source, Thread};
use haste_core::time::now_secs;
use haste_proto::{AiMemoryView, AiState, ErrorCode, Response};
use serde::Deserialize;

use crate::prompts::PromptStore;
use crate::state::State;

// ---------------------------------------------------------------------------------------------
// CONSTANTS (tune here)

/// Calls waiting at most; a job that finds the queue full is not queued.
pub const APFEL_QUEUE_DEPTH: usize = 8;
/// A call running longer than this is killed (outcome `timeout`).
pub const APFEL_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Background jobs run only below this one-minute load per core.
pub const APFEL_MAX_LOAD_PER_CORE: f64 = 0.5;
/// Background jobs run only with at least this much free memory (MB).
pub const APFEL_MIN_FREE_MB: u64 = 4096;
/// Largest prompt (tokens, as `apfel --count-tokens` counts) a call may send. The on-device model
/// has a 4096-token window and the answer needs room.
pub const APFEL_PROMPT_TOKEN_BUDGET: usize = 2500;
/// Most tokens the model may answer with.
pub const APFEL_MAX_ANSWER_TOKENS: u32 = 600;
/// Cached results older than this are deleted (seconds).
pub const AI_RESULT_RETENTION_SECS: i64 = 14 * 86_400;
/// How often the background jobs are looked at.
pub const APFEL_TICK: Duration = Duration::from_secs(30);
/// A gated job is tried again after this long (seconds).
pub const GATED_RETRY_SECS: i64 = 300;
/// A job still waiting after this long is dropped (seconds).
pub const JOB_MAX_AGE_SECS: i64 = 86_400;
/// How long `--count-tokens` and `--model-info` may take.
pub const APFEL_QUICK_TIMEOUT: Duration = Duration::from_secs(15);
/// The settings file in the data directory.
pub const APFEL_SETTINGS_FILE: &str = "apfel.json";
/// Where `apfel` is unless the settings say otherwise.
pub const APFEL_DEFAULT_PATH: &str = "/opt/homebrew/bin/apfel";
/// Folder (in the run directory) for prompt files and the schema; mode 0700.
const APFEL_RUN_SUBDIR: &str = "apfel";
const TODO_SCHEMA_FILE: &str = "todo.schema.json";
/// Longest message body put into a prompt (characters), so one huge mail cannot dominate.
const MESSAGE_CHARS: usize = 3000;
/// The most output read back from the process.
const MAX_OUTPUT_BYTES: u64 = 64 * 1024;

// ---------------------------------------------------------------------------------------------
// Settings

/// `apfel.json`: off unless it says `"enabled": true`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub path: String,
    pub max_load_per_core: f64,
    pub min_free_mb: u64,
    pub prompt_token_budget: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            path: APFEL_DEFAULT_PATH.to_owned(),
            max_load_per_core: APFEL_MAX_LOAD_PER_CORE,
            min_free_mb: APFEL_MIN_FREE_MB,
            prompt_token_budget: APFEL_PROMPT_TOKEN_BUDGET,
        }
    }
}

impl Settings {
    /// Read the settings; a missing or broken file means the defaults (disabled).
    #[must_use]
    pub fn load(data_dir: &Path) -> Self {
        fs::read_to_string(data_dir.join(APFEL_SETTINGS_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn limits(&self) -> GateLimits {
        GateLimits {
            max_load_per_core: self.max_load_per_core,
            min_free_mb: self.min_free_mb,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Jobs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Draft,
    Todo,
    Rewrite,
}

impl Tool {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Todo => "todo",
            Self::Rewrite => "rewrite",
        }
    }
}

/// A background job: what to compute for which version of which thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub tool: Tool,
    pub thread_id: String,
    pub thread_version: u64,
    pub queued_at: i64,
    pub next_attempt_at: i64,
}

/// The waiting background jobs. In memory only: after a restart they are queued again the next
/// time the thread is opened.
#[derive(Debug, Default)]
pub struct JobQueue {
    jobs: Vec<Job>,
}

/// What [`JobQueue::enqueue`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    Added,
    /// The same job (tool, thread, version) was already waiting.
    Duplicate,
    /// The queue holds [`APFEL_QUEUE_DEPTH`] jobs.
    Full,
}

impl JobQueue {
    pub fn enqueue(
        &mut self,
        tool: Tool,
        thread_id: &str,
        thread_version: u64,
        now: i64,
    ) -> Enqueued {
        if self.jobs.iter().any(|j| {
            j.tool == tool && j.thread_id == thread_id && j.thread_version == thread_version
        }) {
            return Enqueued::Duplicate;
        }
        if self.jobs.len() >= APFEL_QUEUE_DEPTH {
            return Enqueued::Full;
        }
        self.jobs.push(Job {
            tool,
            thread_id: thread_id.to_owned(),
            thread_version,
            queued_at: now,
            next_attempt_at: now,
        });
        Enqueued::Added
    }

    /// Whether this job is waiting.
    #[must_use]
    pub fn contains(&self, tool: Tool, thread_id: &str, thread_version: u64) -> bool {
        self.jobs.iter().any(|j| {
            j.tool == tool && j.thread_id == thread_id && j.thread_version == thread_version
        })
    }

    /// Remove and return the jobs that are due, oldest first.
    pub fn take_due(&mut self, now: i64) -> Vec<Job> {
        let (due, rest): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition(|j| j.next_attempt_at <= now);
        self.jobs = rest;
        due
    }

    /// Put a job back to be tried again later; dropped when it has waited too long.
    pub fn reschedule(&mut self, mut job: Job, now: i64) -> bool {
        if now - job.queued_at >= JOB_MAX_AGE_SECS {
            return false;
        }
        job.next_attempt_at = now + GATED_RETRY_SECS;
        self.jobs.push(job);
        true
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

// ---------------------------------------------------------------------------------------------
// Running apfel

/// What goes to the model: a system prompt and the user text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: String,
    pub user: String,
}

/// The result of one `apfel` process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallResult {
    pub outcome: AiOutcome,
    pub text: String,
}

/// The seam between the daemon and the CLI, so tests never start a real model.
pub trait Runner: Send + Sync {
    /// Whether the model can run (`apfel --model-info`).
    fn available(&self) -> bool;
    /// Tokens of `text`, or `None` when they could not be counted.
    fn count_tokens(&self, text: &str) -> Option<usize>;
    /// Run the model. `schema` is a JSON Schema file for structured output.
    fn complete(&self, prompt: &Prompt, schema: Option<&Path>, max_tokens: u32) -> CallResult;
}

/// Reads the machine's load and memory.
pub trait Sampler: Send + Sync {
    fn sample(&self) -> Sample;
}

/// Why a child process gave no exit code.
#[derive(Debug, PartialEq, Eq)]
enum RunError {
    Spawn,
    Timeout,
}

/// Run `program`, feed `stdin_text`, return the exit code and the output (at most
/// [`MAX_OUTPUT_BYTES`]). The child is killed after `timeout`.
fn run_process(
    program: &Path,
    args: &[String],
    stdin_text: &str,
    timeout: Duration,
) -> Result<(Option<i32>, String), RunError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| RunError::Spawn)?;
    let input = stdin_text.to_owned().into_bytes();
    let writer = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.take(MAX_OUTPUT_BYTES).read_to_end(&mut out);
            out
        })
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Spawn);
            }
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let out = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    match status {
        Some(status) => Ok((status.code(), String::from_utf8_lossy(&out).into_owned())),
        None => Err(RunError::Timeout),
    }
}

/// A file with a prompt in it, mode 0600 in a 0700 folder, removed when dropped (so on every
/// outcome, including a timeout or a panic).
struct TempFile(PathBuf);

impl TempFile {
    fn create(dir: &Path, contents: &str) -> std::io::Result<Self> {
        ensure_private_dir(dir)?;
        let name = format!(
            "prompt-{}-{}",
            std::process::id(),
            haste_core::auth::new_id()
        );
        let path = dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(contents.as_bytes())?;
        Ok(Self(path))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// The real thing: the `apfel` binary.
pub struct ProcessRunner {
    path: PathBuf,
    dir: PathBuf,
}

impl ProcessRunner {
    #[must_use]
    pub fn new(path: &str, run_dir: &Path) -> Self {
        Self {
            path: PathBuf::from(path),
            dir: run_dir.join(APFEL_RUN_SUBDIR),
        }
    }
}

impl Runner for ProcessRunner {
    fn available(&self) -> bool {
        run_process(
            &self.path,
            &["--model-info".to_owned()],
            "",
            APFEL_QUICK_TIMEOUT,
        )
        .is_ok_and(|(code, out)| code == Some(0) && out.contains("available:  yes"))
    }

    fn count_tokens(&self, text: &str) -> Option<usize> {
        let args = ["--count-tokens".to_owned(), "-q".to_owned()];
        let (code, out) = run_process(&self.path, &args, text, APFEL_QUICK_TIMEOUT).ok()?;
        if code != Some(0) {
            return None;
        }
        // "123/3584 tokens (fits)"
        out.lines()
            .rev()
            .find_map(|l| l.trim().split('/').next()?.trim().parse::<usize>().ok())
    }

    fn complete(&self, prompt: &Prompt, schema: Option<&Path>, max_tokens: u32) -> CallResult {
        let fail = |outcome| CallResult {
            outcome,
            text: String::new(),
        };
        // The system prompt goes through a private file, the user text through stdin: nothing
        // from a message or a memory is ever an argument of the process.
        let Ok(system_file) = TempFile::create(&self.dir, &prompt.system) else {
            return fail(AiOutcome::Error);
        };
        let mut args = vec![
            "-q".to_owned(),
            "--system-file".to_owned(),
            system_file.0.to_string_lossy().into_owned(),
            "--max-tokens".to_owned(),
            max_tokens.to_string(),
        ];
        if let Some(schema) = schema {
            args.push("--schema".to_owned());
            args.push(schema.to_string_lossy().into_owned());
        }
        match run_process(&self.path, &args, &prompt.user, APFEL_CALL_TIMEOUT) {
            Ok((Some(0), text)) => CallResult {
                outcome: AiOutcome::Ok,
                text,
            },
            Ok((Some(code), _)) => fail(AiOutcome::from_exit_code(code)),
            Ok((None, _)) => fail(AiOutcome::Error),
            Err(RunError::Timeout) => fail(AiOutcome::Timeout),
            Err(RunError::Spawn) => fail(AiOutcome::Unavailable),
        }
    }
}

/// Reads the load and memory with `sysctl` and `memory_pressure`.
pub struct SystemSampler;

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let (code, out) = run_process(Path::new(program), &args, "", Duration::from_secs(5)).ok()?;
    (code == Some(0)).then_some(out)
}

impl Sampler for SystemSampler {
    fn sample(&self) -> Sample {
        let load1 = command_output("/usr/sbin/sysctl", &["-n", "vm.loadavg"])
            .and_then(|s| parse_loadavg(&s));
        let ncpu = command_output("/usr/sbin/sysctl", &["-n", "hw.ncpu"])
            .and_then(|s| s.trim().parse::<u32>().ok());
        let memsize = command_output("/usr/sbin/sysctl", &["-n", "hw.memsize"])
            .and_then(|s| s.trim().parse::<u64>().ok());
        let free_pct = command_output("/usr/bin/memory_pressure", &["-Q"])
            .and_then(|s| parse_free_percentage(&s));
        let free_mb = match (memsize, free_pct) {
            (Some(bytes), Some(pct)) => Some(bytes / (1024 * 1024) * u64::from(pct) / 100),
            _ => None,
        };
        let pressure = command_output(
            "/usr/sbin/sysctl",
            &["-n", "kern.memorystatus_vm_pressure_level"],
        )
        .and_then(|s| parse_pressure(&s));
        Sample {
            load1,
            ncpu,
            free_mb,
            pressure,
        }
    }
}

/// `{ 6.39 5.56 5.11 }` to the first number.
fn parse_loadavg(text: &str) -> Option<f64> {
    text.trim()
        .trim_start_matches('{')
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// `System-wide memory free percentage: 67%`.
fn parse_free_percentage(text: &str) -> Option<u8> {
    text.lines()
        .find_map(|l| l.split("free percentage:").nth(1))?
        .trim()
        .trim_end_matches('%')
        .parse()
        .ok()
}

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning, 4 critical.
fn parse_pressure(text: &str) -> Option<Pressure> {
    match text.trim().parse::<u8>().ok()? {
        1 => Some(Pressure::Normal),
        2 => Some(Pressure::Warn),
        4 => Some(Pressure::Critical),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The service held in the daemon state

pub struct Apfel {
    data_dir: PathBuf,
    run_dir: PathBuf,
    settings: RwLock<(Settings, Option<SystemTime>)>,
    runner: RwLock<Arc<dyn Runner>>,
    sampler: RwLock<Arc<dyn Sampler>>,
    queue: Mutex<JobQueue>,
    /// One model call at a time.
    call_lock: Mutex<()>,
    /// Rewrites waiting for the model; background jobs yield while it is above zero.
    interactive_waiting: AtomicUsize,
    /// `apfel` cannot run; set once, cleared when the settings file changes.
    unavailable: AtomicBool,
    checked: AtomicBool,
    last_memory_warnings: Mutex<Vec<String>>,
    prompts: PromptStore,
}

impl Apfel {
    #[must_use]
    pub fn new(data_dir: &Path, run_dir: &Path) -> Self {
        let settings = Settings::load(data_dir);
        let runner: Arc<dyn Runner> = Arc::new(ProcessRunner::new(&settings.path, run_dir));
        let stamp = settings_stamp(data_dir);
        Self {
            data_dir: data_dir.to_path_buf(),
            run_dir: run_dir.to_path_buf(),
            settings: RwLock::new((settings, stamp)),
            runner: RwLock::new(runner),
            sampler: RwLock::new(Arc::new(SystemSampler)),
            queue: Mutex::new(JobQueue::default()),
            call_lock: Mutex::new(()),
            interactive_waiting: AtomicUsize::new(0),
            unavailable: AtomicBool::new(false),
            checked: AtomicBool::new(false),
            last_memory_warnings: Mutex::new(Vec::new()),
            prompts: PromptStore::new(data_dir),
        }
    }

    /// Replace the process runner (tests use a fake).
    pub fn set_runner(&self, runner: Arc<dyn Runner>) {
        if let Ok(mut slot) = self.runner.write() {
            *slot = runner;
        }
    }

    /// Replace the machine sampler (tests inject values).
    pub fn set_sampler(&self, sampler: Arc<dyn Sampler>) {
        if let Ok(mut slot) = self.sampler.write() {
            *slot = sampler;
        }
    }

    /// Replace the settings (tests).
    pub fn set_settings(&self, settings: Settings) {
        if let Ok(mut slot) = self.settings.write() {
            slot.0 = settings;
        }
    }

    fn settings(&self) -> Settings {
        self.settings
            .read()
            .map(|s| s.0.clone())
            .unwrap_or_default()
    }

    fn runner(&self) -> Arc<dyn Runner> {
        self.runner.read().map_or_else(
            |_| Arc::new(ProcessRunner::new(APFEL_DEFAULT_PATH, &self.run_dir)) as Arc<dyn Runner>,
            |r| Arc::clone(&r),
        )
    }

    fn sampler(&self) -> Arc<dyn Sampler> {
        self.sampler.read().map_or_else(
            |_| Arc::new(SystemSampler) as Arc<dyn Sampler>,
            |s| Arc::clone(&s),
        )
    }

    /// Whether the helpers are switched on and `apfel` can run.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.settings().enabled && !self.unavailable.load(Ordering::SeqCst)
    }

    /// Re-read the settings when the file changed (this also clears `unavailable`).
    fn reload_settings(&self) {
        let stamp = settings_stamp(&self.data_dir);
        let changed = self.settings.read().is_ok_and(|s| s.1 != stamp);
        if !changed {
            return;
        }
        let settings = Settings::load(&self.data_dir);
        if let Ok(mut slot) = self.runner.write() {
            *slot = Arc::new(ProcessRunner::new(&settings.path, &self.run_dir));
        }
        if let Ok(mut slot) = self.settings.write() {
            *slot = (settings, stamp);
        }
        self.unavailable.store(false, Ordering::SeqCst);
        self.checked.store(false, Ordering::SeqCst);
    }

    /// Check once that the model can run (and write the TODO schema); sets `unavailable`.
    fn ensure_ready(&self) -> bool {
        if !self.settings().enabled {
            return false;
        }
        if !self.checked.swap(true, Ordering::SeqCst) {
            let dir = self.run_dir.join(APFEL_RUN_SUBDIR);
            self.prompts.ensure_defaults();
            let schema_ok = ensure_private_dir(&dir).is_ok()
                && write_private(&dir.join(TODO_SCHEMA_FILE), ai::TODO_SCHEMA).is_ok();
            if !schema_ok || !self.runner().available() {
                self.unavailable.store(true, Ordering::SeqCst);
                log::warn("apfel", "unavailable", &[]);
            }
        }
        !self.unavailable.load(Ordering::SeqCst)
    }

    fn schema_path(&self) -> PathBuf {
        self.run_dir.join(APFEL_RUN_SUBDIR).join(TODO_SCHEMA_FILE)
    }

    fn queue_depth(&self) -> usize {
        self.queue.lock().map_or(0, |q| q.len())
    }
}

fn settings_stamp(data_dir: &Path) -> Option<SystemTime> {
    fs::metadata(data_dir.join(APFEL_SETTINGS_FILE))
        .and_then(|m| m.modified())
        .ok()
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// The thread as the model sees it

struct Ctx {
    thread: Thread,
    messages: Vec<Message>,
    channel: String,
    senders: Vec<String>,
    /// Mail from a sender on `allowed_senders`.
    approved: bool,
    /// Index of the latest message from the other side.
    latest_inbound: Option<usize>,
}

impl Ctx {
    /// The human wrote after the latest message from the other side: nothing left to answer.
    fn already_answered(&self) -> bool {
        self.latest_inbound
            .is_some_and(|i| self.messages.iter().skip(i + 1).any(|m| m.is_own))
    }
}

fn is_mail(source: &Source) -> bool {
    matches!(source, Source::Gmail | Source::Icloud)
}

fn load_ctx(state: &State, thread_id: &str) -> Option<Ctx> {
    let (thread, _) = state.store.get_thread(thread_id).ok().flatten()?;
    let messages = state.store.messages_for_thread(thread_id).ok()?;
    let own = state
        .connector_for(&thread.source)
        .and_then(|c| c.own_address())
        .map(|a| a.to_lowercase());
    let from_me = |m: &Message| {
        m.is_own
            || own.as_deref().is_some_and(|o| {
                m.sender_identifier
                    .as_deref()
                    .is_some_and(|s| s.to_lowercase() == o)
            })
    };
    let messages: Vec<Message> = messages
        .into_iter()
        .map(|mut m| {
            m.is_own = from_me(&m);
            m
        })
        .collect();
    let latest_inbound = messages.iter().rposition(|m| !m.is_own);
    let mut senders: Vec<String> = thread.participants.clone();
    for m in &messages {
        if let Some(s) = m.sender_identifier.as_deref() {
            if !senders.iter().any(|x| x == s) {
                senders.push(s.to_owned());
            }
        }
    }
    let approved = is_mail(&thread.source) && {
        let text = fs::read_to_string(state.config.data_dir.join(ALLOWED_FILE)).unwrap_or_default();
        let (allowed, _) = IgnoreList::parse(&text);
        // the same subject the TUI matches: the newest sender who is not the human
        latest_inbound
            .and_then(|i| messages[i].sender_identifier.as_deref())
            .is_some_and(|s| allowed.is_muted(&[s]))
    };
    Some(Ctx {
        channel: thread.source.as_name(),
        thread,
        messages,
        senders,
        approved,
        latest_inbound,
    })
}

fn clip(text: &str) -> String {
    text.trim().chars().take(MESSAGE_CHARS).collect()
}

fn label(m: &Message) -> String {
    format!(
        "{}: {}",
        if m.is_own { "Me" } else { "Them" },
        clip(&m.body_text)
    )
}

/// The memories that apply to the thread (and the warnings of loading them, logged once).
fn memories_for(state: &State, ctx: &Ctx) -> Vec<PassedMemory> {
    let (memories, warnings): (Vec<Memory>, Vec<String>) =
        ai::load_memories(&state.config.data_dir.join(ai::MEMORIES_DIR));
    if let Ok(mut last) = state.apfel.last_memory_warnings.lock() {
        if *last != warnings {
            for w in &warnings {
                // file names and fixed reasons only
                log::event(
                    log::Level::Warn,
                    "apfel",
                    "memory_warning",
                    &[log::d("detail", w)],
                );
            }
            *last = warnings;
        }
    }
    ai::select_memories(&memories, &ctx.channel, &ctx.senders)
}

fn today(now: i64) -> String {
    let (y, m, d, _, _) = haste_core::time::to_civil(now + haste_core::time::local_offset_secs());
    format!("{y:04}-{m:02}-{d:02}")
}

fn memory_block(memories: &[String]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut s = String::from("\nNotes about this correspondence (follow them):\n");
    for m in memories {
        s.push_str("- ");
        s.push_str(&m.replace('\n', " "));
        s.push('\n');
    }
    s
}

fn transcript(parts: &PromptParts, latest_title: &str) -> String {
    let mut s = String::new();
    if !parts.earlier.is_empty() {
        s.push_str("Earlier messages:\n");
        for m in &parts.earlier {
            s.push_str(m);
            s.push('\n');
        }
        s.push('\n');
    }
    s.push_str(latest_title);
    s.push_str(":\n");
    s.push_str(&parts.latest);
    s
}

fn draft_prompt(parts: &PromptParts, prompts: &PromptStore) -> Prompt {
    Prompt {
        system: prompts.draft(&memory_block(&parts.memories)),
        user: transcript(parts, "Latest message (reply to this)"),
    }
}

fn todo_prompt(parts: &PromptParts, now: i64, prompts: &PromptStore) -> Prompt {
    Prompt {
        system: prompts.todo(&memory_block(&parts.memories), &today(now)),
        user: transcript(parts, "Latest message"),
    }
}

fn rewrite_prompt(parts: &PromptParts, instruction: &str, prompts: &PromptStore) -> Prompt {
    Prompt {
        system: prompts.rewrite(&memory_block(&parts.memories), instruction),
        user: transcript(parts, "Draft to edit"),
    }
}

fn prompt_text(p: &Prompt) -> String {
    format!("{}\n\n{}", p.system, p.user)
}

/// The prompt that fits the budget, or `TooLong`. Counting failure is an error, not "fits".
fn fit(
    runner: &dyn Runner,
    parts: PromptParts,
    budget: usize,
    build: &dyn Fn(&PromptParts) -> Prompt,
) -> Result<Prompt, AiOutcome> {
    let mut counting_failed = false;
    let trimmed = ai::trim_to_budget(parts, budget, |p| {
        runner
            .count_tokens(&prompt_text(&build(p)))
            .unwrap_or_else(|| {
                counting_failed = true;
                0
            })
    });
    if counting_failed {
        return Err(AiOutcome::Error);
    }
    trimmed.map(|p| build(&p)).map_err(|_| AiOutcome::TooLong)
}

// ---------------------------------------------------------------------------------------------
// One call: run, log, audit

struct CallRecord {
    tool: Tool,
    thread_id: String,
    outcome: AiOutcome,
    duration_ms: i64,
    queue_depth: usize,
    sample: Sample,
}

/// The fields of the operational log line: labels and numbers and the hashed thread only.
fn call_fields(r: &CallRecord) -> Vec<log::Field> {
    let load_x100 = r.sample.load1.map_or(-1, |l| (l * 100.0).round() as i64);
    let free_mb = r
        .sample
        .free_mb
        .map_or(-1, |m| i64::try_from(m).unwrap_or(i64::MAX));
    vec![
        log::l("tool", r.tool.label()),
        log::id("thread", &ai::short_hash(&r.thread_id)),
        log::n("duration_ms", r.duration_ms),
        log::l("outcome", r.outcome.as_str()),
        log::n("queue_depth", r.queue_depth),
        log::n("load1_x100", load_x100),
        log::n("free_mb", free_mb),
    ]
}

fn record_call(state: &State, r: &CallRecord) {
    let level = if r.outcome == AiOutcome::Ok {
        log::Level::Info
    } else {
        log::Level::Warn
    };
    log::event(level, "apfel", "call", &call_fields(r));
    state.audit(
        "human",
        "ai.call",
        r.outcome.as_str(),
        Some(&ai::short_hash(&r.thread_id)),
        Some(&format!("tool={}", r.tool.label())),
    );
}

fn answered(thread: &Ctx, tool: Tool) -> bool {
    tool == Tool::Draft && thread.already_answered()
}

/// Run one background job now (the gate was already passed). Returns without storing anything
/// when the thread went away or the result is already cached.
fn run_job(state: &State, job: &Job, sample: Sample) {
    let apfel = &state.apfel;
    let Some(ctx) = load_ctx(state, &job.thread_id) else {
        return;
    };
    // a new message arrived since the job was queued: the next open queues it again
    if ctx.thread.version != job.thread_version || !ctx.approved || answered(&ctx, job.tool) {
        return;
    }
    let Some(latest) = ctx.latest_inbound else {
        return;
    };
    let memories = memories_for(state, &ctx);
    let fingerprint = ai::memory_fingerprint(&memories);
    let key = haste_store::AiKey {
        kind: job.tool.label(),
        thread_id: &job.thread_id,
        thread_version: job.thread_version,
        memory_fingerprint: &fingerprint,
    };
    if state.store.ai_result_get(&key).ok().flatten().is_some() {
        return;
    }
    let first = latest.saturating_sub(ai::DRAFT_EARLIER_MESSAGES);
    let parts = PromptParts {
        earlier: ctx.messages[first..latest].iter().map(label).collect(),
        latest: clip(&ctx.messages[latest].body_text),
        memories: memories.iter().map(|m| m.text.clone()).collect(),
    };
    let Ok(_guard) = apfel.call_lock.lock() else {
        return;
    };
    let timer = log::Timer::start();
    let runner = apfel.runner();
    let settings = apfel.settings();
    let now = now_secs();
    let prompts = &apfel.prompts;
    let build: Box<dyn Fn(&PromptParts) -> Prompt + '_> = match job.tool {
        Tool::Todo => Box::new(move |p| todo_prompt(p, now, prompts)),
        _ => Box::new(move |p| draft_prompt(p, prompts)),
    };
    let result = match fit(
        runner.as_ref(),
        parts,
        settings.prompt_token_budget,
        build.as_ref(),
    ) {
        Ok(prompt) => {
            let schema = (job.tool == Tool::Todo).then(|| apfel.schema_path());
            runner.complete(&prompt, schema.as_deref(), APFEL_MAX_ANSWER_TOKENS)
        }
        Err(outcome) => CallResult {
            outcome,
            text: String::new(),
        },
    };
    let (outcome, payload) = finish(job.tool, result);
    if outcome.is_cacheable() {
        let _ = state
            .store
            .ai_result_put(&key, &payload, outcome.as_str(), now);
    }
    record_call(
        state,
        &CallRecord {
            tool: job.tool,
            thread_id: job.thread_id.clone(),
            outcome,
            duration_ms: timer.ms(),
            queue_depth: apfel.queue_depth(),
            sample,
        },
    );
    if outcome == AiOutcome::Ok {
        state.publish(haste_proto::Event::ThreadChanged {
            thread_id: job.thread_id.clone(),
        });
    }
}

/// Validate the model's answer into the payload that is stored: the draft text, or the TODO JSON.
fn finish(tool: Tool, result: CallResult) -> (AiOutcome, String) {
    if result.outcome != AiOutcome::Ok {
        return (result.outcome, String::new());
    }
    match tool {
        Tool::Todo => match ai::parse_todo(&result.text) {
            Ok(todo) => (
                AiOutcome::Ok,
                serde_json::to_string(&todo).unwrap_or_default(),
            ),
            Err(_) => (AiOutcome::InvalidOutput, String::new()),
        },
        Tool::Draft | Tool::Rewrite => {
            let text = result.text.trim().to_owned();
            if text.is_empty() {
                (AiOutcome::InvalidOutput, String::new())
            } else {
                (AiOutcome::Ok, text)
            }
        }
    }
}

/// One pass over the waiting background jobs: reload the settings, take the due jobs and run
/// those the gate lets through; the others wait another [`GATED_RETRY_SECS`].
pub fn tick(state: &State) {
    let apfel = &state.apfel;
    apfel.reload_settings();
    if !apfel.ensure_ready() {
        // nothing runs; jobs queued before the helpers went away stay until they expire
        return;
    }
    let now = now_secs();
    let due = apfel
        .queue
        .lock()
        .map(|mut q| q.take_due(now))
        .unwrap_or_default();
    for job in due {
        let sample = apfel.sampler().sample();
        let decision = ai::resource_gate(&sample, &apfel.settings().limits());
        let yield_to_human = apfel.interactive_waiting.load(Ordering::SeqCst) > 0;
        if decision != GateDecision::Run || yield_to_human {
            if let Ok(mut q) = apfel.queue.lock() {
                q.reschedule(job.clone(), now);
            }
            record_gated(state, &job, sample);
            continue;
        }
        run_job(state, &job, sample);
    }
}

fn record_gated(state: &State, job: &Job, sample: Sample) {
    let r = CallRecord {
        tool: job.tool,
        thread_id: job.thread_id.clone(),
        outcome: AiOutcome::Gated,
        duration_ms: 0,
        queue_depth: state.apfel.queue_depth(),
        sample,
    };
    // a gated attempt is logged but is not a call, so it has no audit record
    log::event(log::Level::Info, "apfel", "call", &call_fields(&r));
}

/// Run [`tick`] forever.
pub async fn tick_loop(state: Arc<State>) {
    let mut ticker = tokio::time::interval(APFEL_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let st = Arc::clone(&state);
        let _ = tokio::task::spawn_blocking(move || tick(&st)).await;
    }
}

/// Delete cached results past their retention (called by the expiry tick).
pub fn expire(state: &State, now: i64) {
    let _ = state
        .store
        .ai_results_delete_older_than(now - AI_RESULT_RETENTION_SECS);
}

// ---------------------------------------------------------------------------------------------
// Requests

fn err(code: ErrorCode, msg: &str) -> Response {
    Response::error(code, msg)
}

/// `AiSuggestionGet`: what is stored for the thread, queueing the jobs that are missing.
/// One log line per decision of [`suggestion_get`]: fixed words, the source name and the hashed
/// thread, so the log shows why a thread got (or did not get) a suggestion.
fn log_suggest(decision: &'static str, tool: &'static str, thread_id: &str, channel: &str) {
    log::event(
        log::Level::Info,
        "apfel",
        "suggest",
        &[
            log::l("decision", decision),
            log::l("tool", tool),
            log::id("thread", &ai::short_hash(thread_id)),
            log::d("channel", channel),
        ],
    );
}

pub fn suggestion_get(state: &State, thread_id: &str) -> Response {
    let apfel = &state.apfel;
    apfel.reload_settings();
    let unavailable = Response::AiSuggestion {
        draft_state: AiState::Unavailable,
        draft: None,
        todo_state: AiState::Unavailable,
        todo: None,
    };
    if !apfel.usable() {
        log_suggest("off", "all", thread_id, "");
        return unavailable;
    }
    let Some(ctx) = load_ctx(state, thread_id) else {
        log_suggest("not_found", "all", thread_id, "");
        return err(ErrorCode::NotFound, "thread not found");
    };
    if !ctx.approved {
        // approved means: mail, and the newest sender who is not the human is on allowed_senders
        log_suggest("not_approved", "all", thread_id, &ctx.channel);
        return Response::AiSuggestion {
            draft_state: AiState::None,
            draft: None,
            todo_state: AiState::None,
            todo: None,
        };
    }
    let memories = memories_for(state, &ctx);
    let fingerprint = ai::memory_fingerprint(&memories);
    let now = now_secs();
    let lookup = |tool: Tool| -> (AiState, Option<String>) {
        if answered(&ctx, tool) {
            log_suggest("answered", tool.label(), thread_id, &ctx.channel);
            return (AiState::None, None);
        }
        let key = haste_store::AiKey {
            kind: tool.label(),
            thread_id,
            thread_version: ctx.thread.version,
            memory_fingerprint: &fingerprint,
        };
        let stored = state.store.ai_result_get(&key).ok().flatten();
        match stored {
            Some(row) if row.outcome == AiOutcome::Ok.as_str() => {
                log_suggest("cached", tool.label(), thread_id, &ctx.channel);
                (AiState::Ready, Some(row.payload))
            }
            // refused: asked once for this version, not again
            Some(_) => {
                log_suggest("cached", tool.label(), thread_id, &ctx.channel);
                (AiState::None, None)
            }
            None => {
                let queued = apfel
                    .queue
                    .lock()
                    .map(|mut q| q.enqueue(tool, thread_id, ctx.thread.version, now))
                    .unwrap_or(Enqueued::Full);
                match queued {
                    // a job already waiting is asked about again every few seconds: not logged
                    Enqueued::Added => log_suggest("queued", tool.label(), thread_id, &ctx.channel),
                    Enqueued::Full => log_suggest("full", tool.label(), thread_id, &ctx.channel),
                    Enqueued::Duplicate => {}
                }
                if queued == Enqueued::Full {
                    // not queued: logged as a gated attempt, shown as nothing yet
                    record_gated(
                        state,
                        &Job {
                            tool,
                            thread_id: thread_id.to_owned(),
                            thread_version: ctx.thread.version,
                            queued_at: now,
                            next_attempt_at: now,
                        },
                        Sample {
                            load1: None,
                            ncpu: None,
                            free_mb: None,
                            pressure: None,
                        },
                    );
                    return (AiState::None, None);
                }
                (AiState::Pending, None)
            }
        }
    };
    let (draft_state, draft) = lookup(Tool::Draft);
    let (todo_state, todo_json) = lookup(Tool::Todo);
    let todo: Option<TodoSuggestion> = todo_json.and_then(|j| serde_json::from_str(&j).ok());
    Response::AiSuggestion {
        draft_state,
        draft,
        todo_state,
        todo,
    }
}

/// `AiMemoriesList`: ids and titles of the memories that apply to the thread, nothing else.
pub fn memories_list(state: &State, thread_id: &str) -> Response {
    let Some(ctx) = load_ctx(state, thread_id) else {
        return err(ErrorCode::NotFound, "thread not found");
    };
    let (memories, _) = ai::load_memories(&state.config.data_dir.join(ai::MEMORIES_DIR));
    let titles: HashMap<&str, &str> = memories
        .iter()
        .map(|m| (m.id.as_str(), m.title.as_str()))
        .collect();
    let passed = ai::select_memories(&memories, &ctx.channel, &ctx.senders);
    Response::AiMemories {
        memories: passed
            .iter()
            .map(|m| AiMemoryView {
                id: m.id.clone(),
                title: titles
                    .get(m.id.as_str())
                    .map_or_else(String::new, |t| (*t).to_owned()),
            })
            .collect(),
    }
}

/// `AiRewrite`: the human asked, so it runs now whatever the load and is not cached.
pub fn rewrite(
    state: &State,
    thread_id: &str,
    draft: &str,
    mode: RewriteMode,
    instruction: Option<&str>,
) -> Response {
    let apfel = &state.apfel;
    apfel.reload_settings();
    let result = |outcome, text| Response::AiRewritten { outcome, text };
    if draft.trim().is_empty() {
        return err(ErrorCode::Validation, "the draft is empty");
    }
    if draft.len() > ai::DRAFT_MAX_BYTES {
        return err(ErrorCode::Validation, "the draft is too long");
    }
    let custom = instruction.unwrap_or("");
    if custom.chars().count() > ai::INSTRUCTION_MAX {
        return err(ErrorCode::Validation, "the instruction is too long");
    }
    let instruction = apfel.prompts.instruction(mode, custom);
    if instruction.is_empty() {
        return err(ErrorCode::Validation, "the instruction is empty");
    }
    let Some(ctx) = load_ctx(state, thread_id) else {
        return err(ErrorCode::NotFound, "thread not found");
    };
    if !apfel.usable() || !apfel.ensure_ready() {
        return result(AiOutcome::Unavailable, None);
    }
    let memories = memories_for(state, &ctx);
    let first = ctx
        .messages
        .len()
        .saturating_sub(ai::REWRITE_EARLIER_MESSAGES);
    let parts = PromptParts {
        earlier: ctx.messages[first..].iter().map(label).collect(),
        latest: draft.trim().to_owned(),
        memories: memories.iter().map(|m| m.text.clone()).collect(),
    };
    // in front of the background jobs: they yield while a rewrite waits
    apfel.interactive_waiting.fetch_add(1, Ordering::SeqCst);
    let guard = apfel.call_lock.lock();
    apfel.interactive_waiting.fetch_sub(1, Ordering::SeqCst);
    let Ok(_guard) = guard else {
        return result(AiOutcome::Error, None);
    };
    let timer = log::Timer::start();
    let runner = apfel.runner();
    let settings = apfel.settings();
    let sample = apfel.sampler().sample();
    let build = |p: &PromptParts| rewrite_prompt(p, &instruction, &apfel.prompts);
    let call = match fit(runner.as_ref(), parts, settings.prompt_token_budget, &build) {
        Ok(prompt) => runner.complete(&prompt, None, APFEL_MAX_ANSWER_TOKENS),
        Err(outcome) => CallResult {
            outcome,
            text: String::new(),
        },
    };
    let (outcome, text) = finish(Tool::Rewrite, call);
    record_call(
        state,
        &CallRecord {
            tool: Tool::Rewrite,
            thread_id: thread_id.to_owned(),
            outcome,
            duration_ms: timer.ms(),
            queue_depth: apfel.queue_depth(),
            sample,
        },
    );
    result(outcome, (outcome == AiOutcome::Ok).then_some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_dedupes_by_tool_thread_and_version() {
        let mut q = JobQueue::default();
        assert_eq!(q.enqueue(Tool::Draft, "t1", 1, 100), Enqueued::Added);
        assert_eq!(q.enqueue(Tool::Draft, "t1", 1, 101), Enqueued::Duplicate);
        assert_eq!(
            q.enqueue(Tool::Todo, "t1", 1, 101),
            Enqueued::Added,
            "another tool"
        );
        assert_eq!(
            q.enqueue(Tool::Draft, "t1", 2, 102),
            Enqueued::Added,
            "new version"
        );
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn the_queue_holds_at_most_eight_jobs() {
        let mut q = JobQueue::default();
        for i in 0..APFEL_QUEUE_DEPTH {
            assert_eq!(
                q.enqueue(Tool::Draft, &format!("t{i}"), 1, 0),
                Enqueued::Added
            );
        }
        assert_eq!(q.enqueue(Tool::Draft, "extra", 1, 0), Enqueued::Full);
    }

    #[test]
    fn due_jobs_are_taken_and_gated_ones_wait_then_expire() {
        let mut q = JobQueue::default();
        q.enqueue(Tool::Draft, "a", 1, 1000);
        let mut due = q.take_due(1000);
        assert_eq!(due.len(), 1);
        assert!(q.is_empty());
        let job = due.remove(0);
        assert!(q.reschedule(job.clone(), 1000));
        assert!(
            q.take_due(1000 + GATED_RETRY_SECS - 1).is_empty(),
            "not yet"
        );
        assert_eq!(q.take_due(1000 + GATED_RETRY_SECS).len(), 1);
        // dropped after a day
        assert!(!q.reschedule(job, 1000 + JOB_MAX_AGE_SECS));
        assert!(q.is_empty());
    }

    #[test]
    fn machine_readings_are_parsed() {
        assert_eq!(parse_loadavg("{ 6.39 5.56 5.11 }\n"), Some(6.39));
        assert_eq!(parse_loadavg("nonsense"), None);
        assert_eq!(
            parse_free_percentage(
                "The system has 1 (2).\nSystem-wide memory free percentage: 67%\n"
            ),
            Some(67)
        );
        assert_eq!(parse_pressure("1\n"), Some(Pressure::Normal));
        assert_eq!(parse_pressure("4\n"), Some(Pressure::Critical));
        assert_eq!(parse_pressure("3\n"), None);
    }

    #[test]
    fn settings_default_to_off_and_survive_a_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!Settings::load(dir.path()).enabled);
        fs::write(dir.path().join(APFEL_SETTINGS_FILE), "{ not json").unwrap();
        assert!(!Settings::load(dir.path()).enabled);
        fs::write(
            dir.path().join(APFEL_SETTINGS_FILE),
            r#"{"enabled": true, "min_free_mb": 1000}"#,
        )
        .unwrap();
        let s = Settings::load(dir.path());
        assert!(s.enabled);
        assert_eq!(s.min_free_mb, 1000);
        assert_eq!(s.prompt_token_budget, APFEL_PROMPT_TOKEN_BUDGET);
    }

    #[test]
    fn a_temp_prompt_file_is_private_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("apfel");
        let path = {
            let f = TempFile::create(&sub, "secret prompt").unwrap();
            let mode = fs::metadata(&f.0).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            assert_eq!(
                fs::metadata(&sub).unwrap().permissions().mode() & 0o777,
                0o700
            );
            f.0.clone()
        };
        assert!(!path.exists());
    }

    #[test]
    fn a_process_that_runs_too_long_is_killed() {
        let started = Instant::now();
        let r = run_process(
            Path::new("/bin/sleep"),
            &["5".to_owned()],
            "",
            Duration::from_millis(200),
        );
        assert_eq!(r, Err(RunError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(3));
        let r = run_process(
            Path::new("/bin/cat"),
            &[],
            "from stdin",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(r, (Some(0), "from stdin".to_owned()));
        assert_eq!(
            run_process(
                Path::new("/nonexistent/apfel"),
                &[],
                "",
                Duration::from_secs(1)
            ),
            Err(RunError::Spawn)
        );
    }

    #[test]
    fn the_todo_answer_is_validated_and_a_draft_must_not_be_empty() {
        let ok = CallResult {
            outcome: AiOutcome::Ok,
            text: r#"{"has_task":true,"title":"Send the form","notes":"","due":"nonsense","priority":"high"}"#.into(),
        };
        let (outcome, payload) = finish(Tool::Todo, ok);
        assert_eq!(outcome, AiOutcome::Ok);
        let todo: TodoSuggestion = serde_json::from_str(&payload).unwrap();
        assert_eq!(todo.due, None);
        let bad = CallResult {
            outcome: AiOutcome::Ok,
            text: "Sure! Here you go".into(),
        };
        assert_eq!(finish(Tool::Todo, bad).0, AiOutcome::InvalidOutput);
        let empty = CallResult {
            outcome: AiOutcome::Ok,
            text: "  ".into(),
        };
        assert_eq!(finish(Tool::Draft, empty).0, AiOutcome::InvalidOutput);
        let refused = CallResult {
            outcome: AiOutcome::Refused,
            text: String::new(),
        };
        assert_eq!(
            finish(Tool::Draft, refused),
            (AiOutcome::Refused, String::new())
        );
    }

    #[test]
    fn the_log_line_has_labels_numbers_and_a_hashed_thread_only() {
        let rec = CallRecord {
            tool: Tool::Draft,
            thread_id: "01THREADIDENTIFIER".into(),
            outcome: AiOutcome::Ok,
            duration_ms: 1234,
            queue_depth: 2,
            sample: Sample {
                load1: Some(2.5),
                ncpu: Some(12),
                free_mb: Some(9000),
                pressure: Some(Pressure::Normal),
            },
        };
        let dump = format!("{:?}", call_fields(&rec));
        assert!(!dump.contains("01THREADIDENTIFIER"));
        assert!(dump.contains(&ai::short_hash("01THREADIDENTIFIER")));
        assert!(dump.contains("250") && dump.contains("9000") && dump.contains("1234"));
    }
}
