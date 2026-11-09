//! Runs the helper: request on stdin (then closed), a hard timeout, the whole process group
//! killed on expiry, and exactly one JSON object accepted on stdout. Stderr is captured to a
//! bound and only ever reduced to an exit code and error kind.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::{RemindersRequest, RemindersResponse};

/// Longest a single helper call may run.
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// Most bytes read from the helper's stdout.
pub const MAX_STDOUT_BYTES: u64 = 4 * 1024 * 1024;
/// Most bytes kept from stderr (it is discarded on success and never logged).
pub const MAX_STDERR_BYTES: u64 = 8 * 1024;
const POLL_STEP: Duration = Duration::from_millis(20);

/// Why a helper call failed. Messages never contain titles, notes or list names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperError {
    /// The binary is missing or cannot start.
    Missing(String),
    TimedOut,
    /// Non-zero exit; only the code is kept.
    Exited(Option<i32>),
    /// Output was not exactly one valid response object.
    Malformed(String),
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(m) => write!(f, "reminders helper unavailable: {m}"),
            Self::TimedOut => write!(f, "reminders helper timed out"),
            Self::Exited(Some(c)) => write!(f, "reminders helper exited with code {c}"),
            Self::Exited(None) => write!(f, "reminders helper was terminated"),
            Self::Malformed(m) => write!(f, "reminders helper gave bad output: {m}"),
        }
    }
}

impl std::error::Error for HelperError {}

/// Anything that can answer a helper request (the real process, or a fake in tests).
pub trait Helper: Send + Sync {
    /// # Errors
    /// [`HelperError`] when the helper cannot be run or answers badly.
    fn call(&self, request: &RemindersRequest) -> Result<RemindersResponse, HelperError>;
}

/// The Swift helper binary.
#[derive(Debug, Clone)]
pub struct ProcessHelper {
    pub bin: PathBuf,
    pub timeout: Duration,
}

impl ProcessHelper {
    #[must_use]
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            timeout: HELPER_TIMEOUT,
        }
    }
}

impl Helper for ProcessHelper {
    fn call(&self, request: &RemindersRequest) -> Result<RemindersResponse, HelperError> {
        let body =
            serde_json::to_vec(request).map_err(|e| HelperError::Malformed(e.to_string()))?;
        let mut child = Command::new(&self.bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| {
                HelperError::Missing(format!("cannot start {}: {}", self.bin.display(), e.kind()))
            })?;
        let pid = child.id();
        let mut stdin = child.stdin.take();
        let writer = thread::spawn(move || {
            if let Some(mut s) = stdin.take() {
                let _ = s.write_all(&body);
                let _ = s.write_all(b"\n");
            } // dropped here: stdin is closed
        });
        let mut stdout = child.stdout.take();
        let out_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.take(MAX_STDOUT_BYTES + 1).read_to_end(&mut buf);
            }
            buf
        });
        let mut stderr = child.stderr.take();
        let err_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.take(MAX_STDERR_BYTES).read_to_end(&mut buf);
            }
            buf.len()
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() >= self.timeout => break None,
                Ok(None) => thread::sleep(POLL_STEP),
                Err(e) => return Err(HelperError::Missing(format!("cannot wait: {}", e.kind()))),
            }
        };
        let Some(status) = status else {
            kill_group(pid);
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(HelperError::TimedOut);
        };
        let _ = writer.join();
        let output = out_reader.join().unwrap_or_default();
        let _stderr_len = err_reader.join().unwrap_or(0); // captured only to drain the pipe
        if !status.success() {
            return Err(HelperError::Exited(status.code()));
        }
        parse_single_object(&output)
    }
}

/// Exactly one JSON object, nothing else but whitespace, within the size bound.
pub(crate) fn parse_single_object(output: &[u8]) -> Result<RemindersResponse, HelperError> {
    if u64::try_from(output.len()).unwrap_or(u64::MAX) > MAX_STDOUT_BYTES {
        return Err(HelperError::Malformed("output too large".into()));
    }
    let mut stream = serde_json::Deserializer::from_slice(output).into_iter::<RemindersResponse>();
    let first = stream
        .next()
        .ok_or_else(|| HelperError::Malformed("empty output".into()))?
        .map_err(|e| HelperError::Malformed(e.classify_str()))?;
    if stream.next().is_some() {
        return Err(HelperError::Malformed("more than one object".into()));
    }
    Ok(first)
}

trait ClassifyStr {
    fn classify_str(&self) -> String;
}

impl ClassifyStr for serde_json::Error {
    /// The error category only: the message can quote the offending text.
    fn classify_str(&self) -> String {
        format!("{:?}", self.classify()).to_lowercase()
    }
}

fn kill_group(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--"])
        .arg(format!("-{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("helper.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn list() -> RemindersRequest {
        RemindersRequest::ListLists
    }

    #[test]
    fn a_good_answer_is_parsed_and_the_request_arrives_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        // echo the stdin back into a file so the test can see what the helper received
        let seen = dir.path().join("seen.json");
        let bin = script(
            &dir,
            &format!(
                "cat > {}\necho '{{\"op\":\"list_lists\",\"count\":3}}'",
                seen.display()
            ),
        );
        let reply = ProcessHelper::new(bin).call(&list()).unwrap();
        assert_eq!(reply, RemindersResponse::ListLists { count: 3 });
        assert!(std::fs::read_to_string(seen)
            .unwrap()
            .contains("list_lists"));
    }

    #[test]
    fn a_hung_helper_is_killed_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-survived");
        let bin = script(
            &dir,
            &format!("(sleep 3; touch {}) &\nsleep 30", marker.display()),
        );
        let mut helper = ProcessHelper::new(bin);
        helper.timeout = Duration::from_millis(300);
        let started = Instant::now();
        assert_eq!(helper.call(&list()), Err(HelperError::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_secs(4));
        assert!(!marker.exists(), "the background child must be killed too");
    }

    #[test]
    fn malformed_output_is_refused_without_quoting_it() {
        for body in [
            "echo 'not json with secret-title'",
            "echo '{\"op\":\"list_lists\",\"count\":1}{\"op\":\"list_lists\",\"count\":2}'",
            "true",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let err = ProcessHelper::new(script(&dir, body))
                .call(&list())
                .unwrap_err();
            assert!(matches!(err, HelperError::Malformed(_)), "{body}");
            assert!(!err.to_string().contains("secret-title"));
        }
    }

    #[test]
    fn a_failing_helper_reports_only_the_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(&dir, "echo 'Buy milk' >&2\nexit 7");
        let err = ProcessHelper::new(bin).call(&list()).unwrap_err();
        assert_eq!(err, HelperError::Exited(Some(7)));
        assert!(!err.to_string().contains("milk"));
    }

    #[test]
    fn a_missing_binary_is_reported_as_missing() {
        let err = ProcessHelper::new("/nonexistent/haste-reminders-helper")
            .call(&list())
            .unwrap_err();
        assert!(matches!(err, HelperError::Missing(_)));
    }

    #[test]
    fn oversized_output_is_refused() {
        let big = vec![b' '; usize::try_from(MAX_STDOUT_BYTES).unwrap() + 1];
        assert!(matches!(
            parse_single_object(&big),
            Err(HelperError::Malformed(_))
        ));
    }
}
