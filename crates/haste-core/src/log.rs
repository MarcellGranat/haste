//! Operational log: one JSON line per event, for working out what happened when something goes
//! wrong. It is separate from the hash-chained audit log (which records decisions) and follows
//! the same privacy rule, enforced by the types instead of by care:
//!
//! * every field value is a number, a `&'static str` label, an identifier restricted to
//!   `[A-Za-z0-9_.:-]`, or a short detail restricted to a small safe alphabet; there is no way to
//!   pass free text, so a message body, chat name, address, token or error string cannot be logged
//!   by accident;
//! * field names are `&'static str`.
//!
//! Until [`init`] is called every call is a no-op (tests stay silent). Files live in
//! `<base>/logs/`, directory 0700, files 0600, rotated by size.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Rotate when a file passes this size.
pub const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Files kept per process: the current one and this many older ones.
pub const LOG_KEEP_FILES: usize = 4;
/// Longest identifier or detail written.
pub const LOG_MAX_VALUE_LEN: usize = 160;
/// Directory (inside the data directory) holding the logs.
pub const LOG_DIR: &str = "logs";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// One field of an event. Built only through [`n`], [`l`], [`id`] and [`d`].
#[derive(Debug, Clone)]
pub struct Field {
    key: &'static str,
    value: Value,
}

#[derive(Debug, Clone)]
enum Value {
    Num(i64),
    Label(&'static str),
    Text(String),
}

/// A number (count, duration in ms, status code).
#[must_use]
pub fn n(key: &'static str, value: impl TryInto<i64>) -> Field {
    Field {
        key,
        value: Value::Num(value.try_into().unwrap_or(i64::MAX)),
    }
}

/// A fixed label chosen in the source code.
#[must_use]
pub fn l(key: &'static str, value: &'static str) -> Field {
    Field {
        key,
        value: Value::Label(value),
    }
}

/// An opaque identifier (a proposal or thread id, a source name). Anything outside
/// `[A-Za-z0-9_.:-]` is replaced, so a stray name or address cannot get through whole.
#[must_use]
pub fn id(key: &'static str, value: &str) -> Field {
    Field {
        key,
        value: Value::Text(clean(value, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')
        })),
    }
}

/// A short detail that already follows the counts-and-labels rule (an audit detail such as
/// `accounts=1 chats=2`). Characters outside a small alphabet are replaced.
#[must_use]
pub fn d(key: &'static str, value: &str) -> Field {
    Field {
        key,
        value: Value::Text(clean(value, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-' | '=' | ' ' | ',' | '/')
        })),
    }
}

fn clean(value: &str, keep: impl Fn(char) -> bool) -> String {
    value
        .chars()
        .take(LOG_MAX_VALUE_LEN)
        .map(|c| if keep(c) { c } else { '?' })
        .collect()
}

struct Sink {
    dir: PathBuf,
    process: &'static str,
    file: Option<File>,
    size: u64,
    min: Level,
    stderr_from: Option<Level>,
    reported: bool,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

fn path_of(dir: &Path, process: &str, n: usize) -> PathBuf {
    if n == 0 {
        dir.join(format!("{process}.log"))
    } else {
        dir.join(format!("{process}.log.{n}"))
    }
}

fn open_log(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Start logging to `<base>/logs/<process>.log`. Events below `min` are dropped; events at or
/// above `stderr_from` are also printed on stderr. Calling it twice keeps the first setup.
///
/// # Errors
/// When the directory or file cannot be created; the caller should carry on without logging.
pub fn init(
    base: &Path,
    process: &'static str,
    min: Level,
    stderr_from: Option<Level>,
) -> std::io::Result<()> {
    let dir = base.join(LOG_DIR);
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let path = path_of(&dir, process, 0);
    let file = open_log(&path)?;
    let size = file.metadata().map_or(0, |m| m.len());
    let sink = Sink {
        dir,
        process,
        file: Some(file),
        size,
        min,
        stderr_from,
        reported: false,
    };
    let _ = SINK.set(Mutex::new(sink));
    Ok(())
}

/// The level named by `HASTE_LOG` (debug, info, warn, error), else `default`.
#[must_use]
pub fn level_from_env(default: Level) -> Level {
    std::env::var("HASTE_LOG")
        .ok()
        .and_then(|v| Level::parse(&v))
        .unwrap_or(default)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"?\"".to_owned())
}

fn render(level: Level, process: &str, component: &str, event: &str, fields: &[Field]) -> String {
    let mut line = format!(
        "{{\"ts_ms\":{},\"level\":\"{}\",\"proc\":{},\"pid\":{},\"seq\":{},\"component\":{},\"event\":{}",
        now_ms(),
        level.name(),
        json_str(process),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        json_str(component),
        json_str(event),
    );
    for f in fields {
        line.push(',');
        line.push_str(&json_str(f.key));
        line.push(':');
        match &f.value {
            Value::Num(v) => line.push_str(&v.to_string()),
            Value::Label(v) => line.push_str(&json_str(v)),
            Value::Text(v) => line.push_str(&json_str(v)),
        }
    }
    line.push('}');
    line
}

impl Sink {
    fn rotate(&mut self) {
        self.file = None;
        for i in (0..LOG_KEEP_FILES).rev() {
            let from = path_of(&self.dir, self.process, i);
            let to = path_of(&self.dir, self.process, i + 1);
            if from.exists() {
                let _ = fs::rename(from, to);
            }
        }
        self.file = open_log(&path_of(&self.dir, self.process, 0)).ok();
        self.size = 0;
    }

    fn write(&mut self, line: &str) {
        if self.size > LOG_MAX_BYTES {
            self.rotate();
        }
        let ok = self.file.as_mut().is_some_and(|f| {
            f.write_all(line.as_bytes())
                .and_then(|()| f.write_all(b"\n"))
                .is_ok()
        });
        if ok {
            self.size += line.len() as u64 + 1;
        } else if !self.reported {
            self.reported = true;
            eprintln!("haste: cannot write the log file (logging continues on stderr only)");
        }
    }
}

/// Record one event. `component` and `event` are fixed names (`"beeper"`, `"sync_done"`).
pub fn event(level: Level, component: &'static str, event: &'static str, fields: &[Field]) {
    let Some(sink) = SINK.get() else {
        return;
    };
    let Ok(mut sink) = sink.lock() else {
        return;
    };
    if level < sink.min {
        return;
    }
    let line = render(level, sink.process, component, event, fields);
    if sink.stderr_from.is_some_and(|m| level >= m) {
        eprintln!("{line}");
    }
    sink.write(&line);
}

pub fn debug(component: &'static str, name: &'static str, fields: &[Field]) {
    event(Level::Debug, component, name, fields);
}

pub fn info(component: &'static str, name: &'static str, fields: &[Field]) {
    event(Level::Info, component, name, fields);
}

pub fn warn(component: &'static str, name: &'static str, fields: &[Field]) {
    event(Level::Warn, component, name, fields);
}

pub fn error(component: &'static str, name: &'static str, fields: &[Field]) {
    event(Level::Error, component, name, fields);
}

/// Log panics (location only; the panic message can carry data, so it is not written) and keep
/// the default hook's behavior.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (line, col) = info.location().map_or((0, 0), |l| (l.line(), l.column()));
        let file = info
            .location()
            .map_or("unknown", |l| l.file())
            .rsplit('/')
            .next()
            .unwrap_or("unknown")
            .to_owned();
        event(
            Level::Error,
            "process",
            "panic",
            &[id("file", &file), n("line", line), n("col", col)],
        );
        previous(info);
    }));
}

/// Measures how long a step took.
#[derive(Debug, Clone, Copy)]
pub struct Timer(Instant);

impl Timer {
    #[must_use]
    pub fn start() -> Self {
        Self(Instant::now())
    }

    #[must_use]
    pub fn ms(self) -> i64 {
        i64::try_from(self.0.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// What `haste logs` shows: lines of the current and rotated files of `process`, oldest first,
/// at or above `min`, optionally from one `component`, newer than `since_ms` (unix ms).
#[must_use]
pub fn read_lines(
    base: &Path,
    process: &str,
    min: Level,
    component: Option<&str>,
    since_ms: Option<u128>,
) -> Vec<String> {
    let dir = base.join(LOG_DIR);
    let mut out = Vec::new();
    for i in (0..=LOG_KEEP_FILES).rev() {
        let Ok(text) = fs::read_to_string(path_of(&dir, process, i)) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let level = v
                .get("level")
                .and_then(|x| x.as_str())
                .and_then(Level::parse)
                .unwrap_or(Level::Info);
            let ts = v
                .get("ts_ms")
                .and_then(serde_json::Value::as_u64)
                .map_or(0, u128::from);
            let comp_ok =
                component.is_none_or(|c| v.get("component").and_then(|x| x.as_str()) == Some(c));
            if level >= min && comp_ok && since_ms.is_none_or(|s| ts >= s) {
                out.push(line.to_owned());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_details_cannot_carry_free_text() {
        let f = id("x", "Marcell Granát <a@b.c>");
        let Value::Text(t) = f.value else { panic!() };
        assert!(!t.contains(' ') && !t.contains('@') && !t.contains('<'));
        let f = d("x", "chats=2 bad\"quote\n");
        let Value::Text(t) = f.value else { panic!() };
        assert_eq!(t, "chats=2 bad?quote?");
    }

    #[test]
    fn a_line_is_valid_json() {
        let line = render(
            Level::Info,
            "t",
            "c",
            "e",
            &[n("a", 3_u32), l("b", "x"), id("c", "01ABC")],
        );
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["a"], 3);
        assert_eq!(v["c"], "01ABC");
    }
}
