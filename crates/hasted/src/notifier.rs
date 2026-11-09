//! Result delivery to agents through herdr `agent.prompt`.
//!
//! * a local agent is prompted over the herdr socket;
//! * an agent on a remote machine is prompted through the hub's `herdr --machine <label>`;
//! * while the local socket link is down, the plain CLI is used as a fallback.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use haste_herdr::command::{HerdrCommand, Runner};
use haste_herdr::prompt::send_prompt;

pub trait Notifier: Send + Sync {
    /// Deliver a short message to the named agent on the local machine.
    ///
    /// # Errors
    /// A human-readable reason; the daemon audits it and retries on a bounded schedule.
    fn deliver(&self, agent: &str, message: &str) -> Result<(), String>;

    /// Deliver to an agent on a remote herdr machine, through the hub.
    ///
    /// # Errors
    /// A human-readable reason, as for [`Notifier::deliver`].
    fn deliver_remote(&self, _machine: &str, _agent: &str, _message: &str) -> Result<(), String> {
        Err("remote delivery is not supported by this notifier".into())
    }
}

/// Production notifier. `local_up` is set by the herdr supervisor while the socket is healthy.
pub struct HerdrNotifier {
    pub socket: PathBuf,
    pub runner: Runner,
    pub local_up: Arc<AtomicBool>,
}

impl HerdrNotifier {
    #[must_use]
    pub fn new(socket: PathBuf, runner: Runner, local_up: Arc<AtomicBool>) -> Self {
        Self {
            socket,
            runner,
            local_up,
        }
    }
}

impl Default for HerdrNotifier {
    fn default() -> Self {
        Self::new(
            haste_herdr::local::default_socket(),
            Runner::from_env(),
            Arc::new(AtomicBool::new(false)),
        )
    }
}

impl Notifier for HerdrNotifier {
    fn deliver(&self, agent: &str, message: &str) -> Result<(), String> {
        if self.local_up.load(Ordering::SeqCst) {
            send_prompt(&self.socket, agent, message).map_err(|e| e.to_string())
        } else {
            // The socket link is down: fall back to the plain CLI for the same bounded attempts.
            self.runner
                .run(&HerdrCommand::Prompt {
                    agent: agent.to_owned(),
                    text: message.to_owned(),
                })
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }

    fn deliver_remote(&self, machine: &str, agent: &str, message: &str) -> Result<(), String> {
        self.runner
            .run(&HerdrCommand::RemotePrompt {
                label: machine.to_owned(),
                agent: agent.to_owned(),
                text: message.to_owned(),
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Discards messages.
pub struct NoopNotifier;

impl Notifier for NoopNotifier {
    fn deliver(&self, _agent: &str, _message: &str) -> Result<(), String> {
        Ok(())
    }

    fn deliver_remote(&self, _machine: &str, _agent: &str, _message: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Test double that records messages and can be told to fail.
#[derive(Default)]
pub struct RecordingNotifier {
    /// `(agent, message)` for local deliveries.
    pub sent: Mutex<Vec<(String, String)>>,
    /// `(machine, agent, message)` for remote deliveries.
    pub remote_sent: Mutex<Vec<(String, String, String)>>,
    pub fail: Mutex<bool>,
    pub attempts: Mutex<u32>,
}

impl RecordingNotifier {
    fn check(&self) -> Result<(), String> {
        if let Ok(mut n) = self.attempts.lock() {
            *n += 1;
        }
        if self.fail.lock().map(|f| *f).unwrap_or(false) {
            return Err("pane unreachable".into());
        }
        Ok(())
    }
}

impl Notifier for RecordingNotifier {
    fn deliver(&self, agent: &str, message: &str) -> Result<(), String> {
        self.check()?;
        if let Ok(mut sent) = self.sent.lock() {
            sent.push((agent.to_owned(), message.to_owned()));
        }
        Ok(())
    }

    fn deliver_remote(&self, machine: &str, agent: &str, message: &str) -> Result<(), String> {
        self.check()?;
        if let Ok(mut sent) = self.remote_sent.lock() {
            sent.push((machine.to_owned(), agent.to_owned(), message.to_owned()));
        }
        Ok(())
    }
}
