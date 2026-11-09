//! Running the `herdr` CLI, safely. Only four command shapes can be built, every call has a
//! closed stdin and a hard timeout, and a call that hangs (for example on an SSH prompt) is killed
//! together with its process group.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use haste_core::herdr::agent_name_for;

use crate::protocol::FeedError;

/// Longest a single herdr call may run before it is killed.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// Output beyond this many bytes is discarded.
pub const MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
const POLL_STEP: Duration = Duration::from_millis(10);
const MAX_LABEL_CHARS: usize = 64;
const MAX_TEXT_CHARS: usize = 4000;

/// The complete set of herdr calls haste makes. There is deliberately no variant for adding,
/// removing, enabling, renaming or reconnecting machines, reading panes, sending keys, or
/// changing workspaces, tabs or panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HerdrCommand {
    MachineList,
    RemoteSnapshot {
        label: String,
    },
    Prompt {
        agent: String,
        text: String,
    },
    RemotePrompt {
        label: String,
        agent: String,
        text: String,
    },
}

fn check_label(label: &str) -> Result<(), FeedError> {
    let bad = label.is_empty()
        || label.chars().count() > MAX_LABEL_CHARS
        || label.starts_with('-')
        || label.chars().any(char::is_control);
    if bad {
        Err(FeedError::Malformed("invalid machine label".into()))
    } else {
        Ok(())
    }
}

fn check_prompt(agent: &str, text: &str) -> Result<(), FeedError> {
    agent_name_for(agent).map_err(|e| FeedError::Malformed(e.into()))?;
    if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS || text.starts_with('-') {
        return Err(FeedError::Malformed("invalid prompt text".into()));
    }
    Ok(())
}

impl HerdrCommand {
    /// The argument list, validated so no value can be mistaken for a flag.
    ///
    /// # Errors
    /// [`FeedError::Malformed`] for an invalid machine label, agent name or prompt text.
    pub fn args(&self) -> Result<Vec<String>, FeedError> {
        let own = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        Ok(match self {
            Self::MachineList => own(&["machine", "list", "--json"]),
            Self::RemoteSnapshot { label } => {
                check_label(label)?;
                let mut a = own(&["--machine"]);
                a.push(label.clone());
                a.extend(own(&["api", "snapshot"]));
                a
            }
            Self::Prompt { agent, text } => {
                check_prompt(agent, text)?;
                let mut a = own(&["agent", "prompt"]);
                a.extend([agent.clone(), text.clone()]);
                a
            }
            Self::RemotePrompt { label, agent, text } => {
                check_label(label)?;
                check_prompt(agent, text)?;
                let mut a = own(&["--machine"]);
                a.push(label.clone());
                a.extend(own(&["agent", "prompt"]));
                a.extend([agent.clone(), text.clone()]);
                a
            }
        })
    }
}

/// Runs herdr commands. `bin` defaults to `herdr` and can be overridden (tests use a fake).
#[derive(Debug, Clone)]
pub struct Runner {
    pub bin: PathBuf,
    pub timeout: Duration,
}

impl Default for Runner {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Runner {
    #[must_use]
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            timeout: COMMAND_TIMEOUT,
        }
    }

    /// Uses `HASTE_HERDR_BIN` when set, otherwise `herdr` from `PATH`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("HASTE_HERDR_BIN").unwrap_or_else(|| "herdr".into()))
    }

    /// Run a command and return its standard output.
    ///
    /// # Errors
    /// [`FeedError::Io`] if the process cannot start, exits unsuccessfully, or exceeds the timeout
    /// (in which case its whole process group is killed).
    pub fn run(&self, command: &HerdrCommand) -> Result<String, FeedError> {
        let args = command.args()?;
        let mut child = Command::new(&self.bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Its own process group, so a hung ssh child can be killed with it and can never
            // read the terminal in the foreground.
            .process_group(0)
            .spawn()
            .map_err(|e| FeedError::Io(format!("cannot start herdr: {e}")))?;
        let pid = child.id();
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| FeedError::Io("no stdout".into()))?;
        let reader = thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut stdout).take(MAX_OUTPUT_BYTES).read_to_end(&mut buf);
            buf
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() >= self.timeout => break None,
                Ok(None) => thread::sleep(POLL_STEP),
                Err(e) => return Err(FeedError::Io(format!("cannot wait for herdr: {e}"))),
            }
        };
        let Some(status) = status else {
            kill_group(pid);
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(FeedError::Io("herdr call timed out".into()));
        };
        let output = reader.join().unwrap_or_default();
        if !status.success() {
            return Err(FeedError::Io(format!("herdr exited with {status}")));
        }
        Ok(String::from_utf8_lossy(&output).into_owned())
    }
}

/// Kill the child's whole process group (negative pid) with the system `kill`.
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

    fn cmd(c: &HerdrCommand) -> Vec<String> {
        c.args().unwrap()
    }

    #[test]
    fn the_four_allowed_shapes() {
        assert_eq!(
            cmd(&HerdrCommand::MachineList),
            ["machine", "list", "--json"]
        );
        assert_eq!(
            cmd(&HerdrCommand::RemoteSnapshot { label: "m1".into() }),
            ["--machine", "m1", "api", "snapshot"]
        );
        assert_eq!(
            cmd(&HerdrCommand::Prompt {
                agent: "agent-a".into(),
                text: "hi".into()
            }),
            ["agent", "prompt", "agent-a", "hi"]
        );
        assert_eq!(
            cmd(&HerdrCommand::RemotePrompt {
                label: "m1".into(),
                agent: "agent-a".into(),
                text: "hi".into()
            }),
            ["--machine", "m1", "agent", "prompt", "agent-a", "hi"]
        );
    }

    #[test]
    fn values_that_look_like_flags_or_are_malformed_are_refused() {
        for label in ["", "-x", "--machine", "a\nb"] {
            assert!(
                HerdrCommand::RemoteSnapshot {
                    label: label.into()
                }
                .args()
                .is_err(),
                "{label:?}"
            );
        }
        assert!(HerdrCommand::Prompt {
            agent: "Agent A".into(),
            text: "hi".into()
        }
        .args()
        .is_err());
        assert!(HerdrCommand::Prompt {
            agent: "a".into(),
            text: "--help".into()
        }
        .args()
        .is_err());
        assert!(HerdrCommand::Prompt {
            agent: "a".into(),
            text: String::new()
        }
        .args()
        .is_err());
    }

    #[test]
    fn no_machine_management_shape_exists() {
        // Every constructible command starts with one of the four allowed prefixes.
        let all = [
            HerdrCommand::MachineList,
            HerdrCommand::RemoteSnapshot { label: "m".into() },
            HerdrCommand::Prompt {
                agent: "a".into(),
                text: "t".into(),
            },
            HerdrCommand::RemotePrompt {
                label: "m".into(),
                agent: "a".into(),
                text: "t".into(),
            },
        ];
        for c in all {
            let a = cmd(&c).join(" ");
            for forbidden in [
                "add",
                "remove",
                "enable",
                "disable",
                "rename",
                "reconnect",
                "send-keys",
                "pane",
                "read",
            ] {
                assert!(!a.split_whitespace().any(|w| w == forbidden), "{a}");
            }
        }
    }
}
