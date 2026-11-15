//! The Logs screen's data: the daemon's own structured log (`logs/hasted.log`, one JSON object
//! per line), read from the end. It holds counts, ids and fixed labels only, never message or
//! account text, so showing it on screen is safe.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How much of the end of the log file is read (a few thousand lines).
pub const TAIL_BYTES: u64 = 512 * 1024;

/// One log line, ready to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Unix seconds.
    pub at: i64,
    pub level: String,
    /// `hasted` for the daemon; another name for the other programs writing the file.
    pub proc: String,
    pub component: String,
    pub event: String,
    /// `key=value` for everything else on the line.
    pub fields: String,
}

impl LogLine {
    /// Warnings and errors.
    #[must_use]
    pub fn is_problem(&self) -> bool {
        matches!(self.level.as_str(), "warn" | "error")
    }
}

/// Read one JSON log line; anything that is not one is skipped.
#[must_use]
pub fn parse_line(line: &str) -> Option<LogLine> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let obj = value.as_object()?;
    let text = |k: &str| {
        obj.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let at = obj.get("ts_ms").and_then(serde_json::Value::as_i64)? / 1000;
    let mut fields = Vec::new();
    for (k, v) in obj {
        if matches!(
            k.as_str(),
            "ts_ms" | "level" | "proc" | "pid" | "seq" | "component" | "event"
        ) {
            continue;
        }
        let shown = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        fields.push(format!("{k}={shown}"));
    }
    Some(LogLine {
        at,
        level: text("level"),
        proc: text("proc"),
        component: text("component"),
        event: text("event"),
        fields: fields.join(" "),
    })
}

/// The last lines of the log file, oldest first. A missing or unreadable file gives nothing.
#[must_use]
pub fn read_tail(path: &Path) -> Vec<LogLine> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map_or(0, |m| m.len());
    let start = len.saturating_sub(TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(TAIL_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if start > 0 {
        lines.next(); // the first line is probably cut in half
    }
    lines.filter_map(parse_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"ts_ms":1794762040355,"level":"warn","proc":"hasted","pid":86457,"seq":674,"component":"connector","event":"call","source":"imessage","call":"send","outcome":"ambiguous","ms":22098}"#;

    #[test]
    fn a_log_line_is_read_with_its_fields() {
        let l = parse_line(LINE).unwrap();
        assert_eq!(l.at, 1_794_762_040);
        assert_eq!((l.level.as_str(), l.proc.as_str()), ("warn", "hasted"));
        assert_eq!(
            (l.component.as_str(), l.event.as_str()),
            ("connector", "call")
        );
        assert!(l.fields.contains("source=imessage") && l.fields.contains("ms=22098"));
        assert!(!l.fields.contains("pid") && !l.fields.contains("seq"));
        assert!(l.is_problem());
        assert!(parse_line("not json").is_none());
        assert!(
            parse_line(r#"{"level":"info"}"#).is_none(),
            "no time, no line"
        );
    }

    #[test]
    fn the_tail_of_a_file_starts_on_a_whole_line() {
        let dir = std::env::temp_dir().join(format!("haste-logs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hasted.log");
        let mut text = String::new();
        for i in 0..50_000 {
            text.push_str(&LINE.replace("674", &i.to_string()));
            text.push('\n');
        }
        std::fs::write(&path, &text).unwrap();
        let lines = read_tail(&path);
        assert!(
            !lines.is_empty() && lines.len() < 50_000,
            "only the end is read"
        );
        assert!(lines.iter().all(|l| l.component == "connector"));
        assert!(read_tail(&dir.join("missing.log")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
