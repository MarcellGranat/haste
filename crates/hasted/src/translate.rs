//! The local Ollama fallback for the AI helpers. Apple's on-device model (`apfel`) does not read
//! Hungarian; when it says so and `translate.json` exists, one small local model (`gemma3:4b`)
//! writes the draft, rewrite or TODO itself, in one request.
//!
//! Memory: nothing stays loaded. A [`Session`] starts `ollama serve` only if none is running
//! (one model, one request at a time, a 4096-token context), every request unloads the model at
//! once (`keep_alive` 0), and dropping the session stops the server it started.
//!
//! `translate.json` in the data folder: `{"enabled": true, "model": "gemma3:4b",
//! "min_free_mb": 8192}`. A missing file means no fallback.
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const TRANSLATE_SETTINGS_FILE: &str = "translate.json";
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(20);
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(100);
const HOST: &str = "127.0.0.1:11434";
const DEFAULT_MODEL: &str = "gemma3:4b";
const DEFAULT_MIN_FREE_MB: u64 = 8192;
const CANDIDATE_PATHS: [&str; 2] = ["/opt/homebrew/bin/ollama", "/usr/local/bin/ollama"];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct Settings {
    enabled: bool,
    model: String,
    min_free_mb: u64,
    /// The `ollama` program, when it is not in a usual place.
    program: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: DEFAULT_MODEL.to_owned(),
            min_free_mb: DEFAULT_MIN_FREE_MB,
            program: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Translator {
    program: PathBuf,
    model: String,
    min_free_mb: u64,
}

impl Translator {
    /// The translator the data folder asks for, or `None` (no file, disabled, no `ollama`).
    #[must_use]
    pub fn load(data_dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(data_dir.join(TRANSLATE_SETTINGS_FILE)).ok()?;
        let s: Settings = serde_json::from_str(&text).ok()?;
        if !s.enabled || s.model.trim().is_empty() {
            return None;
        }
        let program = s.program.or_else(|| {
            CANDIDATE_PATHS
                .iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
        })?;
        Some(Self {
            program,
            model: s.model,
            min_free_mb: s.min_free_mb,
        })
    }

    /// Enough free memory for the model next to what is running? (`None` counts as no.)
    #[must_use]
    pub fn memory_allows(&self, free_mb: Option<u64>) -> bool {
        free_mb.is_some_and(|f| f >= self.min_free_mb)
    }

    /// Get a server to talk to: the running one, or one started here and stopped at the end.
    #[must_use]
    pub fn session(&self) -> Option<Session> {
        let mut session = Session {
            program: self.program.clone(),
            model: self.model.clone(),
            child: None,
        };
        if session.server_up() {
            return Some(session);
        }
        let child = Command::new(&self.program)
            .arg("serve")
            .env("OLLAMA_MAX_LOADED_MODELS", "1")
            .env("OLLAMA_NUM_PARALLEL", "1")
            .env("OLLAMA_KEEP_ALIVE", "0s")
            .env("OLLAMA_CONTEXT_LENGTH", "4096")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // from here the session owns the child, so a failed start still stops it
        session.child = Some(child);
        let deadline = Instant::now() + SERVER_START_TIMEOUT;
        while Instant::now() < deadline {
            if session.server_up() {
                return Some(session);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        None
    }
}

/// A running Ollama, stopped again when this is dropped (if it was started for the session).
pub struct Session {
    program: PathBuf,
    model: String,
    child: Option<Child>,
}

impl Session {
    fn server_up(&self) -> bool {
        Command::new(&self.program)
            .arg("list")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// One chat request, or `None` on any failure. `schema` (a JSON Schema) forces structured
    /// output. The text goes in through stdin only, never in the arguments.
    #[must_use]
    pub fn complete(&self, system: &str, user: &str, schema: Option<&str>) -> Option<String> {
        use std::io::{Read, Write};
        let mut body = serde_json::json!({
            "model": self.model,
            "stream": false,
            "keep_alive": 0,
            "options": {"num_ctx": 4096, "num_predict": 600, "temperature": 0.4},
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
        if let Some(schema) = schema {
            body["format"] = serde_json::from_str(schema).ok()?;
        }
        let input = serde_json::to_vec(&body).ok()?;
        let mut child = Command::new("curl")
            .args(["-sS", "-f", "--max-time", "90", "-X", "POST"])
            .args(["-H", "Content-Type: application/json", "-d", "@-"])
            .arg(format!("http://{HOST}/api/chat"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take()?;
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.by_ref().take(256 * 1024).read_to_end(&mut out);
            out
        });
        let deadline = Instant::now() + COMPLETE_TIMEOUT;
        let ok = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(40)),
                Err(_) => break false,
            }
        };
        let _ = writer.join();
        let out = reader.join().ok()?;
        if !ok {
            return None;
        }
        let reply: serde_json::Value = serde_json::from_slice(&out).ok()?;
        let text = reply["message"]["content"].as_str()?.trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        } else {
            // a server that was already running stays, but this model leaves memory now
            let _ = Command::new(&self.program)
                .args(["stop", &self.model])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_disabled_file_means_no_translator() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Translator::load(dir.path()).is_none());
        std::fs::write(
            dir.path().join(TRANSLATE_SETTINGS_FILE),
            r#"{"enabled": false}"#,
        )
        .unwrap();
        assert!(Translator::load(dir.path()).is_none());
    }

    #[test]
    fn the_memory_check_needs_a_known_amount() {
        let t = Translator {
            program: PathBuf::from("ollama"),
            model: "m".into(),
            min_free_mb: 8192,
        };
        assert!(t.memory_allows(Some(9000)));
        assert!(!t.memory_allows(Some(4000)));
        assert!(!t.memory_allows(None));
    }
}
