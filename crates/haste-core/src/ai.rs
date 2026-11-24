//! Pure logic for the local AI helpers (the `apfel` integration): memory files and their trigger
//! rules, the resource gate for background jobs, prompt-budget trimming and the structured TODO
//! suggestion. Nothing here touches the network, a process or the daemon state; the daemon's
//! `apfel` module does the I/O and feeds values in.
//!
//! Privacy rule: warnings returned from here name a file, an id or a line number and a fixed
//! reason, never a pattern or text from a memory.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ignore::IgnoreList;

/// Directory (inside the data directory) holding the memory files.
pub const MEMORIES_DIR: &str = "memories";
/// Extension of a memory file.
pub const MEMORY_EXTENSION: &str = "yml";
/// Memory files read; further files (in name order) are skipped.
pub const MAX_MEMORY_FILES: usize = 50;
/// Largest memory file accepted.
pub const MAX_MEMORY_FILE_BYTES: u64 = 4096;
/// Memories passed to one call.
pub const MAX_MEMORIES_PER_CALL: usize = 3;
/// Combined `text` bytes of the memories passed to one call.
pub const MAX_MEMORY_TEXT_BYTES: usize = 1024;
/// Earlier messages passed for a draft or a TODO (besides the latest inbound one).
pub const DRAFT_EARLIER_MESSAGES: usize = 4;
/// Earlier messages passed for a rewrite.
pub const REWRITE_EARLIER_MESSAGES: usize = 2;
/// Length of the short thread hash written to logs.
pub const SHORT_HASH_LEN: usize = 10;

/// Longest TODO title, notes (characters).
pub const TODO_TITLE_MAX: usize = 120;
pub const TODO_NOTES_MAX: usize = 500;

/// Keys a memory file may have; anything else is ignored with a warning.
const MEMORY_KEYS: [&str; 6] = ["id", "title", "enabled", "channels", "senders", "text"];

/// A short, stable, non-reversible tag of a thread id for logs and the audit trail.
#[must_use]
pub fn short_hash(thread_id: &str) -> String {
    let digest = Sha256::digest(thread_id.as_bytes());
    let mut hex = hex::encode(digest);
    hex.truncate(SHORT_HASH_LEN);
    hex
}

/// What happened to one model call. The words are the log and audit labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiOutcome {
    Ok,
    Timeout,
    Refused,
    InvalidOutput,
    TooLong,
    Gated,
    Unavailable,
    Busy,
    Error,
}

impl AiOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
            Self::InvalidOutput => "invalid_output",
            Self::TooLong => "too_long",
            Self::Gated => "gated",
            Self::Unavailable => "unavailable",
            Self::Busy => "busy",
            Self::Error => "error",
        }
    }

    /// The outcome of an `apfel` process that exited with `code` (see `apfel --help`).
    #[must_use]
    pub fn from_exit_code(code: i32) -> Self {
        match code {
            0 => Self::Ok,
            3 => Self::Refused,
            4 => Self::TooLong,
            5 => Self::Unavailable,
            6 => Self::Busy,
            _ => Self::Error,
        }
    }

    /// Whether the result is worth caching for the thread version (a refusal is, so it is not
    /// asked again; a busy model or a runtime error is not).
    #[must_use]
    pub fn is_cacheable(self) -> bool {
        matches!(self, Self::Ok | Self::Refused)
    }
}

/// How the rewrite popup changes the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewriteMode {
    /// The human typed the instruction.
    Custom,
    Grammar,
    Formal,
    Fluent,
    Friendly,
}

/// Longest custom instruction accepted (characters).
pub const INSTRUCTION_MAX: usize = 300;
/// Longest draft a rewrite accepts (bytes).
pub const DRAFT_MAX_BYTES: usize = 8000;

impl RewriteMode {
    /// The instruction given to the model; `custom` is the human's own line for `Custom`.
    #[must_use]
    pub fn instruction(self, custom: &str) -> String {
        match self {
            Self::Custom => custom.trim().to_owned(),
            Self::Grammar => {
                "Fix grammar, spelling and punctuation, and restore missing Hungarian \
                accents. Keep the wording, tone and meaning."
                    .into()
            }
            Self::Formal => "Rewrite in a more formal, polite tone, and restore missing Hungarian \
                accents. Keep the meaning."
                .into(),
            Self::Fluent => "Rewrite so that it reads more fluently and naturally, and restore \
                missing Hungarian accents. Keep the meaning and tone."
                .into(),
            Self::Friendly => {
                "Rewrite in a warmer, friendlier tone, and restore missing Hungarian \
                accents. Keep the meaning."
                    .into()
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Memories

/// A memory as loaded: text that steers the model for the threads whose rule matches.
#[derive(Debug, Clone)]
pub struct Memory {
    pub id: String,
    pub title: String,
    pub enabled: bool,
    pub channels: Vec<String>,
    senders: IgnoreList,
    has_senders: bool,
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct RawMemory {
    id: Option<String>,
    title: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    senders: Vec<String>,
    text: Option<String>,
}

fn default_true() -> bool {
    true
}

/// One memory as the model gets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassedMemory {
    pub id: String,
    pub text: String,
}

impl Memory {
    /// Parse one memory file. `Err` is a fixed reason (the file is skipped); the `Vec` holds
    /// warnings that do not skip it (unknown keys, unusable sender patterns).
    ///
    /// # Errors
    /// The file is too big, not valid YAML, or has no `id` or no `text`.
    pub fn parse(source: &str) -> Result<(Self, Vec<String>), String> {
        if source.len() as u64 > MAX_MEMORY_FILE_BYTES {
            return Err("file too large".into());
        }
        let value: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(source).map_err(|_| "not valid YAML".to_owned())?;
        let mapping = value.as_mapping().ok_or("not a YAML mapping")?;
        let mut warnings = Vec::new();
        for key in mapping.keys() {
            match key.as_str() {
                Some(k) if MEMORY_KEYS.contains(&k) => {}
                Some(_) => warnings.push("unknown key ignored".to_owned()),
                None => warnings.push("non-text key ignored".to_owned()),
            }
        }
        let raw: RawMemory =
            serde_yaml_ng::from_value(value).map_err(|_| "keys have the wrong type".to_owned())?;
        let id = raw.id.map(|s| s.trim().to_owned()).unwrap_or_default();
        if id.is_empty() {
            return Err("no id".into());
        }
        let text = raw.text.unwrap_or_default();
        if text.trim().is_empty() {
            return Err("no text".into());
        }
        let mut patterns = Vec::new();
        for (i, pattern) in raw.senders.iter().enumerate() {
            let (list, errors) = IgnoreList::parse(pattern);
            if pattern.contains('\n') || !errors.is_empty() || list.is_empty() {
                warnings.push(format!("memory {id}: sender pattern {} ignored", i + 1));
            } else {
                patterns.push(pattern.clone());
            }
        }
        let memory = Self {
            title: raw.title.unwrap_or_else(|| id.clone()),
            id,
            enabled: raw.enabled,
            channels: raw
                .channels
                .into_iter()
                .map(|c| c.trim().to_ascii_lowercase())
                .collect(),
            has_senders: !patterns.is_empty(),
            senders: IgnoreList::from_patterns(&patterns),
            text,
        };
        Ok((memory, warnings))
    }

    /// Whether the memory applies to a thread on `channel` with these `senders`: enabled, and
    /// the channel is listed or a sender pattern matches. No rule means it never matches.
    #[must_use]
    pub fn matches<S: AsRef<str>>(&self, channel: &str, senders: &[S]) -> bool {
        if !self.enabled {
            return false;
        }
        let channel = channel.to_ascii_lowercase();
        self.channels.contains(&channel) || (self.has_senders && self.senders.is_muted(senders))
    }
}

/// Load the memory files in `dir` (a missing directory is an empty set). Returns the memories
/// ordered by id and one warning per skipped file or ignored key or pattern, naming the file
/// and a fixed reason only.
#[must_use]
pub fn load_memories(dir: &Path) -> (Vec<Memory>, Vec<String>) {
    let mut warnings = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return (Vec::new(), warnings);
    };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == MEMORY_EXTENSION))
        .collect();
    files.sort();
    if files.len() > MAX_MEMORY_FILES {
        warnings.push(format!(
            "more than {MAX_MEMORY_FILES} memory files; extra skipped"
        ));
        files.truncate(MAX_MEMORY_FILES);
    }
    let mut memories = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let too_big = fs::metadata(&path).map_or(true, |m| m.len() > MAX_MEMORY_FILE_BYTES);
        let parsed = if too_big {
            Err("file too large or unreadable".to_owned())
        } else {
            fs::read_to_string(&path)
                .map_err(|_| "unreadable".to_owned())
                .and_then(|s| Memory::parse(&s))
        };
        match parsed {
            Ok((memory, notes)) => {
                warnings.extend(notes.into_iter().map(|n| format!("{name}: {n}")));
                memories.push(memory);
            }
            Err(why) => warnings.push(format!("{name}: skipped, {why}")),
        }
    }
    memories.sort_by(|a, b| a.id.cmp(&b.id));
    (memories, warnings)
}

/// The memories that go to a call: those matching, ordered by id, at most
/// [`MAX_MEMORIES_PER_CALL`], with a combined text of at most [`MAX_MEMORY_TEXT_BYTES`]
/// (a text is cut at a character boundary).
#[must_use]
pub fn select_memories<S: AsRef<str>>(
    memories: &[Memory],
    channel: &str,
    senders: &[S],
) -> Vec<PassedMemory> {
    let mut matching: Vec<&Memory> = memories
        .iter()
        .filter(|m| m.matches(channel, senders))
        .collect();
    matching.sort_by(|a, b| a.id.cmp(&b.id));
    let mut remaining = MAX_MEMORY_TEXT_BYTES;
    let mut out = Vec::new();
    for memory in matching.into_iter().take(MAX_MEMORIES_PER_CALL) {
        if remaining == 0 {
            break;
        }
        let text = truncate_bytes(memory.text.trim(), remaining);
        remaining -= text.len();
        out.push(PassedMemory {
            id: memory.id.clone(),
            text,
        });
    }
    out
}

fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A fingerprint of the memories passed to a call: it changes when an id or a text does.
#[must_use]
pub fn memory_fingerprint(passed: &[PassedMemory]) -> String {
    let mut hasher = Sha256::new();
    for m in passed {
        hasher.update(m.id.len().to_le_bytes());
        hasher.update(m.id.as_bytes());
        hasher.update(m.text.len().to_le_bytes());
        hasher.update(m.text.as_bytes());
    }
    hex::encode(hasher.finalize())
}

// ---------------------------------------------------------------------------------------------
// Resource gate

/// Memory pressure as macOS reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
}

/// What was sampled from the machine; `None` is a failed read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub load1: Option<f64>,
    pub ncpu: Option<u32>,
    pub free_mb: Option<u64>,
    pub pressure: Option<Pressure>,
}

/// The thresholds of the gate (the daemon holds them as constants and in `apfel.json`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateLimits {
    pub max_load_per_core: f64,
    pub min_free_mb: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Run,
    Busy,
    /// A value could not be read; the gate fails closed.
    Unknown,
}

/// Whether a background job may run now. Pure: fails closed on any missing value.
#[must_use]
pub fn resource_gate(sample: &Sample, limits: &GateLimits) -> GateDecision {
    let (Some(load1), Some(ncpu), Some(free_mb), Some(pressure)) =
        (sample.load1, sample.ncpu, sample.free_mb, sample.pressure)
    else {
        return GateDecision::Unknown;
    };
    if ncpu == 0 || !load1.is_finite() {
        return GateDecision::Unknown;
    }
    if load1 / f64::from(ncpu) >= limits.max_load_per_core
        || free_mb < limits.min_free_mb
        || pressure == Pressure::Critical
    {
        return GateDecision::Busy;
    }
    GateDecision::Run
}

// ---------------------------------------------------------------------------------------------
// Prompt budget

/// The pieces of a prompt that can be dropped to fit the budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptParts {
    /// Earlier messages, oldest first.
    pub earlier: Vec<String>,
    /// The latest inbound message (or the draft for a rewrite); never dropped.
    pub latest: String,
    /// Memory texts, in id order.
    pub memories: Vec<String>,
}

/// The prompt cannot be made to fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLong;

/// Drop the oldest earlier messages one by one, then the first memory texts one by one, until
/// `count` (the token count of the assembled prompt) is at most `budget`.
///
/// # Errors
/// [`TooLong`] when the latest message alone still exceeds the budget.
pub fn trim_to_budget(
    mut parts: PromptParts,
    budget: usize,
    mut count: impl FnMut(&PromptParts) -> usize,
) -> Result<PromptParts, TooLong> {
    loop {
        if count(&parts) <= budget {
            return Ok(parts);
        }
        if !parts.earlier.is_empty() {
            parts.earlier.remove(0);
        } else if !parts.memories.is_empty() {
            parts.memories.remove(0);
        } else {
            return Err(TooLong);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// TODO suggestion

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    None,
    Low,
    Medium,
    High,
}

/// The validated answer of the TODO job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoSuggestion {
    pub has_task: bool,
    pub title: String,
    pub notes: String,
    /// `YYYY-MM-DD`, or none.
    pub due: Option<String>,
    pub priority: TodoPriority,
}

#[derive(Debug, Deserialize)]
struct RawTodo {
    has_task: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    due: Option<String>,
    priority: TodoPriority,
}

/// The JSON Schema handed to `apfel --schema`.
pub const TODO_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "has_task": {"type": "boolean"},
    "title": {"type": "string"},
    "notes": {"type": "string"},
    "due": {"type": ["string", "null"], "description": "YYYY-MM-DD or null"},
    "priority": {"type": "string", "enum": ["none", "low", "medium", "high"]}
  },
  "required": ["has_task", "title", "notes", "due", "priority"],
  "additionalProperties": false
}"#;

/// Validate the model's JSON: lengths are limits (too long is invalid), an invalid date is
/// dropped to `None`, a suggestion without a task carries no fields.
///
/// # Errors
/// The text is not JSON of the schema's shape, or a length limit is exceeded.
pub fn parse_todo(json: &str) -> Result<TodoSuggestion, &'static str> {
    let raw: RawTodo = serde_json::from_str(json.trim()).map_err(|_| "not the TODO schema")?;
    let title = raw.title.trim().to_owned();
    let notes = raw.notes.trim().to_owned();
    if title.chars().count() > TODO_TITLE_MAX || notes.chars().count() > TODO_NOTES_MAX {
        return Err("field too long");
    }
    if !raw.has_task {
        return Ok(TodoSuggestion {
            has_task: false,
            title: String::new(),
            notes: String::new(),
            due: None,
            priority: TodoPriority::None,
        });
    }
    if title.is_empty() {
        return Err("task without a title");
    }
    Ok(TodoSuggestion {
        has_task: true,
        title,
        notes,
        due: raw.due.filter(|d| is_valid_date(d)),
        priority: raw.priority,
    })
}

/// `YYYY-MM-DD` with a real calendar date.
#[must_use]
pub fn is_valid_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|p| p.parse::<u32>().ok());
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    if !s
        .bytes()
        .enumerate()
        .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
    {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KATA: &str = "id: kata-biro-tone\ntitle: Kata is a client\nenabled: true\nsenders: ['kata\\s+biro']\ntext: |\n  Be formal.\n";

    fn memory(src: &str) -> Memory {
        Memory::parse(src).unwrap().0
    }

    #[test]
    fn valid_memory_parses() {
        let (m, warnings) = Memory::parse(KATA).unwrap();
        assert_eq!(m.id, "kata-biro-tone");
        assert!(m.enabled);
        assert!(warnings.is_empty());
    }

    #[test]
    fn malformed_oversize_and_incomplete_files_are_refused() {
        assert_eq!(Memory::parse("id: [").unwrap_err(), "not valid YAML");
        assert_eq!(Memory::parse("- a\n- b").unwrap_err(), "not a YAML mapping");
        assert_eq!(Memory::parse("id: a\n").unwrap_err(), "no text");
        assert_eq!(Memory::parse("text: hi\n").unwrap_err(), "no id");
        let big = format!("id: a\ntext: {}\n", "x".repeat(5000));
        assert_eq!(Memory::parse(&big).unwrap_err(), "file too large");
    }

    #[test]
    fn unknown_keys_and_bad_patterns_warn_without_the_content() {
        let src = "id: a\ntext: hi\nmood: happy\nchannels: [gmail]\nsenders: ['(unclosed']\n";
        let (m, warnings) = Memory::parse(src).unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|w| !w.contains("unclosed")));
        assert!(warnings.iter().any(|w| w.contains("pattern 1")));
        assert!(m.matches("gmail", &["x"]));
        assert!(!m.matches("whatsapp", &["unclosed"]));
    }

    #[test]
    fn channel_or_sender_matches() {
        let by_sender = memory(KATA);
        assert!(by_sender.matches("gmail", &["Kata  Biro <k@x.hu>"]));
        assert!(!by_sender.matches("gmail", &["someone else"]));
        let by_channel = memory("id: wa\ntext: casual\nchannels: [WhatsApp]\n");
        assert!(by_channel.matches("whatsapp", &["anyone"]));
        assert!(!by_channel.matches("gmail", &["anyone"]));
        // either rule is enough
        let both = memory("id: b\ntext: t\nchannels: [gmail]\nsenders: ['kata']\n");
        assert!(both.matches("gmail", &["zed"]));
        assert!(both.matches("imessage", &["Kata"]));
    }

    #[test]
    fn disabled_and_ruleless_memories_never_match() {
        let off = memory("id: a\ntext: t\nenabled: false\nchannels: [gmail]\n");
        assert!(!off.matches("gmail", &["x"]));
        let none = memory("id: a\ntext: t\n");
        assert!(!none.matches("gmail", &["x"]));
    }

    #[test]
    fn at_most_three_memories_and_one_kilobyte() {
        let memories: Vec<Memory> = ["d", "a", "c", "b"]
            .iter()
            .map(|id| memory(&format!("id: {id}\ntext: t-{id}\nchannels: [gmail]\n")))
            .collect();
        let passed = select_memories(&memories, "gmail", &["x"]);
        let ids: Vec<_> = passed.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);

        let long = "é".repeat(600); // 1200 bytes
        let big = memory(&format!("id: a\ntext: {long}\nchannels: [gmail]\n"));
        let next = memory("id: b\ntext: more\nchannels: [gmail]\n");
        let passed = select_memories(&[big, next], "gmail", &["x"]);
        let total: usize = passed.iter().map(|m| m.text.len()).sum();
        assert!(total <= MAX_MEMORY_TEXT_BYTES);
        assert_eq!(passed.len(), 1, "the cap is used up by the first");
    }

    #[test]
    fn fingerprint_changes_with_an_edit() {
        let a = vec![PassedMemory {
            id: "a".into(),
            text: "one".into(),
        }];
        let mut b = a.clone();
        b[0].text = "two".into();
        assert_ne!(memory_fingerprint(&a), memory_fingerprint(&b));
        assert_eq!(memory_fingerprint(&a), memory_fingerprint(&a.clone()));
        // ids and texts cannot run together
        let c = vec![PassedMemory {
            id: "ab".into(),
            text: "c".into(),
        }];
        let d = vec![PassedMemory {
            id: "a".into(),
            text: "bc".into(),
        }];
        assert_ne!(memory_fingerprint(&c), memory_fingerprint(&d));
    }

    #[test]
    fn loading_a_directory_orders_skips_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.yml"), "id: b\ntext: t\n").unwrap();
        std::fs::write(dir.path().join("a.yml"), "id: a\ntext: t\n").unwrap();
        std::fs::write(dir.path().join("bad.yml"), "id: [").unwrap();
        std::fs::write(dir.path().join("note.txt"), "ignored").unwrap();
        let (memories, warnings) = load_memories(dir.path());
        let ids: Vec<_> = memories.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(warnings, ["bad.yml: skipped, not valid YAML"]);
        let (none, _) = load_memories(&dir.path().join("missing"));
        assert!(none.is_empty());
    }

    #[test]
    fn more_than_fifty_files_are_cut() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..52 {
            std::fs::write(
                dir.path().join(format!("m{i:02}.yml")),
                format!("id: m{i:02}\ntext: t\n"),
            )
            .unwrap();
        }
        let (memories, warnings) = load_memories(dir.path());
        assert_eq!(memories.len(), MAX_MEMORY_FILES);
        assert_eq!(warnings.len(), 1);
    }

    const LIMITS: GateLimits = GateLimits {
        max_load_per_core: 0.5,
        min_free_mb: 4096,
    };

    fn sample(load1: f64, free_mb: u64, pressure: Pressure) -> Sample {
        Sample {
            load1: Some(load1),
            ncpu: Some(10),
            free_mb: Some(free_mb),
            pressure: Some(pressure),
        }
    }

    #[test]
    fn gate_runs_when_free_and_stops_when_busy() {
        assert_eq!(
            resource_gate(&sample(2.0, 8000, Pressure::Normal), &LIMITS),
            GateDecision::Run
        );
        assert_eq!(
            resource_gate(&sample(8.0, 8000, Pressure::Normal), &LIMITS),
            GateDecision::Busy
        );
        assert_eq!(
            resource_gate(&sample(2.0, 1000, Pressure::Normal), &LIMITS),
            GateDecision::Busy
        );
        assert_eq!(
            resource_gate(&sample(2.0, 8000, Pressure::Critical), &LIMITS),
            GateDecision::Busy
        );
        assert_eq!(
            resource_gate(&sample(2.0, 8000, Pressure::Warn), &LIMITS),
            GateDecision::Run
        );
    }

    #[test]
    fn gate_fails_closed_on_a_failed_sample() {
        let mut s = sample(2.0, 8000, Pressure::Normal);
        s.load1 = None;
        assert_eq!(resource_gate(&s, &LIMITS), GateDecision::Unknown);
        let mut s = sample(2.0, 8000, Pressure::Normal);
        s.pressure = None;
        assert_eq!(resource_gate(&s, &LIMITS), GateDecision::Unknown);
        let mut s = sample(2.0, 8000, Pressure::Normal);
        s.ncpu = Some(0);
        assert_eq!(resource_gate(&s, &LIMITS), GateDecision::Unknown);
    }

    fn parts() -> PromptParts {
        PromptParts {
            earlier: vec!["old".into(), "newer".into()],
            latest: "latest".into(),
            memories: vec!["m1".into(), "m2".into()],
        }
    }

    fn size(p: &PromptParts) -> usize {
        p.earlier.len() * 10 + p.memories.len() * 10 + 10
    }

    #[test]
    fn trimming_drops_oldest_messages_first() {
        let out = trim_to_budget(parts(), 40, size).unwrap();
        assert_eq!(out.earlier, ["newer"].map(String::from));
        assert_eq!(out.memories.len(), 2);
    }

    #[test]
    fn trimming_then_drops_memories_in_order() {
        let out = trim_to_budget(parts(), 20, size).unwrap();
        assert!(out.earlier.is_empty());
        assert_eq!(out.memories, ["m2"].map(String::from));
    }

    #[test]
    fn a_latest_message_that_does_not_fit_is_too_long() {
        assert_eq!(trim_to_budget(parts(), 5, size), Err(TooLong));
        // already fitting is returned untouched
        assert_eq!(trim_to_budget(parts(), 1000, size).unwrap(), parts());
    }

    #[test]
    fn todo_is_validated() {
        let ok = r#"{"has_task":true,"title":" Send the form ","notes":"By Friday","due":"2026-11-27","priority":"high"}"#;
        let t = parse_todo(ok).unwrap();
        assert_eq!(t.title, "Send the form");
        assert_eq!(t.due.as_deref(), Some("2026-11-27"));
        assert_eq!(t.priority, TodoPriority::High);

        let bad_date =
            r#"{"has_task":true,"title":"x","notes":"","due":"2026-02-30","priority":"low"}"#;
        let t = parse_todo(bad_date).unwrap();
        assert_eq!(t.due, None);
        assert_eq!(t.title, "x");

        let none = r#"{"has_task":false,"title":"junk","notes":"junk","due":"2026-11-27","priority":"high"}"#;
        let t = parse_todo(none).unwrap();
        assert!(!t.has_task && t.title.is_empty() && t.due.is_none());
    }

    #[test]
    fn todo_limits_and_shape_are_enforced() {
        let long = format!(
            r#"{{"has_task":true,"title":"{}","notes":"","due":null,"priority":"low"}}"#,
            "x".repeat(121)
        );
        assert_eq!(parse_todo(&long), Err("field too long"));
        assert!(parse_todo("sure, here is a task").is_err());
        let bad_priority =
            r#"{"has_task":true,"title":"x","notes":"","due":null,"priority":"urgent"}"#;
        assert!(parse_todo(bad_priority).is_err());
        let untitled = r#"{"has_task":true,"title":"  ","notes":"","due":null,"priority":"low"}"#;
        assert_eq!(parse_todo(untitled), Err("task without a title"));
    }

    #[test]
    fn dates_are_real_calendar_dates() {
        assert!(is_valid_date("2028-02-29"));
        assert!(!is_valid_date("2027-02-29"));
        assert!(!is_valid_date("2026-13-01"));
        assert!(!is_valid_date("2026-1-01"));
        assert!(!is_valid_date("2026-11-2x"));
        assert!(!is_valid_date("+026-11-01"));
    }

    #[test]
    fn exit_codes_map_to_outcomes() {
        assert_eq!(AiOutcome::from_exit_code(0), AiOutcome::Ok);
        assert_eq!(AiOutcome::from_exit_code(3), AiOutcome::Refused);
        assert_eq!(AiOutcome::from_exit_code(4), AiOutcome::TooLong);
        assert_eq!(AiOutcome::from_exit_code(5), AiOutcome::Unavailable);
        assert_eq!(AiOutcome::from_exit_code(6), AiOutcome::Busy);
        assert_eq!(AiOutcome::from_exit_code(1), AiOutcome::Error);
        assert!(AiOutcome::Refused.is_cacheable() && !AiOutcome::Busy.is_cacheable());
    }

    #[test]
    fn short_hash_is_stable_and_short() {
        let h = short_hash("01ABCDEF");
        assert_eq!(h.len(), SHORT_HASH_LEN);
        assert_eq!(h, short_hash("01ABCDEF"));
        assert_ne!(h, short_hash("01ABCDEG"));
    }
}
