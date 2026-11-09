//! Append-only, hash-chained JSONL audit log. Records never contain secrets or bodies.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::time::now_secs;

/// Hash recorded as `prev_hash` of the very first record.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const AUDIT_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub ts: i64,
    pub prev_hash: String,
    pub principal: String,
    pub action: String,
    pub outcome: String,
    pub object: Option<String>,
    pub detail: Option<String>,
}

/// Inputs for a new record; sequence, time and chain hash are added by the writer.
#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    pub principal: &'a str,
    pub action: &'a str,
    pub outcome: &'a str,
    pub object: Option<&'a str>,
    pub detail: Option<&'a str>,
}

pub struct AuditWriter {
    file: File,
    next_seq: u64,
    prev_hash: String,
}

fn hash_line(line: &str) -> String {
    hex::encode(Sha256::digest(line.as_bytes()))
}

impl AuditWriter {
    /// Open (or create with mode `0600`) the audit file, resuming the chain.
    ///
    /// # Errors
    /// Fails on I/O errors or when the existing file's chain is already broken.
    pub fn open(path: &Path) -> io::Result<Self> {
        let (next_seq, prev_hash) = match fs::read_to_string(path) {
            Ok(text) => match verify_text(&text) {
                Ok(state) => state,
                Err(seq) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("audit chain broken at sequence {seq}"),
                    ))
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => (1, GENESIS_HASH.to_owned()),
            Err(e) => return Err(e),
        };
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(AUDIT_FILE_MODE)
            .open(path)?;
        Ok(Self {
            file,
            next_seq,
            prev_hash,
        })
    }

    /// Append one record.
    ///
    /// # Errors
    /// Fails if the write does not complete.
    pub fn append(&mut self, entry: &AuditEntry<'_>) -> io::Result<AuditRecord> {
        let record = AuditRecord {
            seq: self.next_seq,
            ts: now_secs(),
            prev_hash: self.prev_hash.clone(),
            principal: entry.principal.to_owned(),
            action: entry.action.to_owned(),
            outcome: entry.outcome.to_owned(),
            object: entry.object.map(str::to_owned),
            detail: entry.detail.map(str::to_owned),
        };
        let line = serde_json::to_string(&record).map_err(io::Error::other)?;
        let mut bytes = line.clone().into_bytes();
        bytes.push(b'\n');
        self.file.write_all(&bytes)?;
        self.file.flush()?;
        self.prev_hash = hash_line(&line);
        self.next_seq += 1;
        Ok(record)
    }
}

/// Walk the chain; returns `(next_seq, last_hash)` or the first sequence number that breaks.
fn verify_text(text: &str) -> Result<(u64, String), u64> {
    let mut expected_seq = 1_u64;
    let mut prev = GENESIS_HASH.to_owned();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<AuditRecord>(line) else {
            return Err(expected_seq);
        };
        if record.seq != expected_seq || record.prev_hash != prev {
            return Err(expected_seq);
        }
        prev = hash_line(line);
        expected_seq += 1;
    }
    Ok((expected_seq, prev))
}

/// Verify an audit file; `Err(seq)` names the first broken sequence number.
///
/// # Errors
/// `Ok(count)` on an intact chain, `Err(seq)` otherwise (also `Err(0)` if unreadable).
pub fn verify_file(path: &Path) -> Result<u64, u64> {
    let text = fs::read_to_string(path).map_err(|_| 0_u64)?;
    verify_text(&text).map(|(next, _)| next - 1)
}

/// Read the most recent `limit` records (used by the daemon for client requests).
///
/// # Errors
/// Fails on I/O errors.
pub fn read_recent(path: &Path, limit: usize) -> io::Result<Vec<AuditRecord>> {
    let text = fs::read_to_string(path)?;
    let mut records: Vec<AuditRecord> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let start = records.len().saturating_sub(limit);
    Ok(records.split_off(start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn entry(n: &str) -> AuditEntry<'_> {
        AuditEntry {
            principal: "agent-a",
            action: n,
            outcome: "auto",
            object: Some("obj"),
            detail: None,
        }
    }

    fn write_three(path: &Path) {
        let mut w = AuditWriter::open(path).unwrap();
        for a in ["read", "draft", "archive"] {
            w.append(&entry(a)).unwrap();
        }
    }

    #[test]
    fn file_mode_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        write_three(&p);
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn intact_chain_verifies_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        write_three(&p);
        assert_eq!(verify_file(&p), Ok(3));
        let mut w = AuditWriter::open(&p).unwrap();
        assert_eq!(w.append(&entry("snooze")).unwrap().seq, 4);
        assert_eq!(verify_file(&p), Ok(4));
    }

    #[test]
    fn modified_record_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        write_three(&p);
        let text = fs::read_to_string(&p)
            .unwrap()
            .replacen("draft", "dralt", 1);
        fs::write(&p, text).unwrap();
        assert_eq!(verify_file(&p), Err(3));
    }

    #[test]
    fn reordered_records_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        write_three(&p);
        let text = fs::read_to_string(&p).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.swap(0, 1);
        fs::write(&p, lines.join("\n") + "\n").unwrap();
        assert_eq!(verify_file(&p), Err(1));
    }

    #[test]
    fn deleted_record_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        write_three(&p);
        let text = fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        fs::write(&p, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert_eq!(verify_file(&p), Err(2));
    }

    #[test]
    fn records_have_no_body_field() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        write_three(&p);
        let text = fs::read_to_string(&p).unwrap();
        assert!(!text.contains("body"));
        assert!(!text.contains("token"));
    }
}
