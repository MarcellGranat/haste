//! Remote machines through the hub's herdr CLI: discovery and polled snapshots.

use std::time::Duration;

use serde::Deserialize;

use crate::command::{HerdrCommand, Runner};
use crate::protocol::{parse_snapshot, FeedError, Snapshot};

/// Seconds between snapshots of a healthy remote machine.
pub const REMOTE_POLL: Duration = Duration::from_secs(15);
/// Longest wait between attempts for a failing machine.
pub const REMOTE_BACKOFF_CAP: Duration = Duration::from_secs(300);
/// Consecutive failures before a machine is reported unreachable.
pub const UNREACHABLE_AFTER: u32 = 2;
/// How often the saved-machine list is re-read.
pub const MACHINE_LIST_REFRESH: Duration = Duration::from_secs(300);

/// A saved herdr machine (`herdr machine list --json`). Other fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MachineInfo {
    pub label: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

const fn enabled_by_default() -> bool {
    true
}

/// The enabled saved machines, in list order.
///
/// # Errors
/// [`FeedError`] if the call fails or the output is not the expected list.
pub fn discover(runner: &Runner) -> Result<Vec<MachineInfo>, FeedError> {
    let text = runner.run(&HerdrCommand::MachineList)?;
    let all: Vec<MachineInfo> =
        serde_json::from_str(&text).map_err(|e| FeedError::Malformed(e.to_string()))?;
    Ok(all.into_iter().filter(|m| m.enabled).collect())
}

/// One snapshot of a remote machine.
///
/// # Errors
/// [`FeedError::Io`] on a failed or timed-out call, [`FeedError::Mismatch`] on another protocol,
/// [`FeedError::Malformed`] on an unreadable snapshot.
pub fn fetch_snapshot(runner: &Runner, label: &str) -> Result<Snapshot, FeedError> {
    let text = runner.run(&HerdrCommand::RemoteSnapshot {
        label: label.to_owned(),
    })?;
    parse_snapshot(&text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    Connected,
    Unreachable,
}

/// Counts failures and decides when a machine's reachability changes and how long to wait.
#[derive(Debug)]
pub struct Tracker {
    failures: u32,
    state: Option<Reachability>,
    base: Duration,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    #[must_use]
    pub fn new() -> Self {
        Self::with_base(REMOTE_POLL)
    }

    /// A tracker with a different poll interval (tests use a short one).
    #[must_use]
    pub fn with_base(base: Duration) -> Self {
        Self {
            failures: 0,
            state: None,
            base,
        }
    }

    /// Record a good snapshot. Returns the new state if it changed (including the first success).
    pub fn success(&mut self) -> Option<Reachability> {
        self.failures = 0;
        self.change(Reachability::Connected)
    }

    /// Record a failed call. Returns `Unreachable` once, after [`UNREACHABLE_AFTER`] in a row.
    pub fn failure(&mut self) -> Option<Reachability> {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= UNREACHABLE_AFTER {
            self.change(Reachability::Unreachable)
        } else {
            None
        }
    }

    /// How long to wait before the next attempt: the normal interval, or a growing backoff
    /// (15 s, 30 s, 60 s, ... up to 5 minutes) while failing.
    #[must_use]
    pub fn delay(&self) -> Duration {
        if self.failures == 0 {
            return self.base;
        }
        self.base
            .saturating_mul(2_u32.saturating_pow(self.failures.saturating_sub(1).min(16)))
            .min(REMOTE_BACKOFF_CAP)
    }

    fn change(&mut self, to: Reachability) -> Option<Reachability> {
        if self.state == Some(to) {
            None
        } else {
            self.state = Some(to);
            Some(to)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_failures_mark_unreachable_once_and_one_success_recovers() {
        let mut t = Tracker::new();
        assert_eq!(t.success(), Some(Reachability::Connected));
        assert_eq!(t.success(), None, "no change, nothing to report");
        assert_eq!(t.failure(), None, "one failure is tolerated");
        assert_eq!(t.failure(), Some(Reachability::Unreachable));
        assert_eq!(t.failure(), None, "reported once, not per attempt");
        assert_eq!(t.success(), Some(Reachability::Connected));
    }

    #[test]
    fn the_wait_grows_while_failing_and_is_capped() {
        let mut t = Tracker::new();
        assert_eq!(t.delay(), Duration::from_secs(15));
        let waits: Vec<u64> = (0..8)
            .map(|_| {
                t.failure();
                t.delay().as_secs()
            })
            .collect();
        assert_eq!(waits, [15, 30, 60, 120, 240, 300, 300, 300]);
        t.success();
        assert_eq!(t.delay(), Duration::from_secs(15));
    }

    #[test]
    fn only_enabled_machines_are_discovered_from_the_list_shape_herdr_prints() {
        let json = r#"[{"id":"1","label":"m1","target":"user@host","session":"s","enabled":true,"selected":false},
                       {"id":"2","label":"m2","target":"x","session":"s","enabled":false,"selected":false},
                       {"id":"3","label":"mbp","target":"y","session":"s","enabled":true,"selected":true}]"#;
        let all: Vec<MachineInfo> = serde_json::from_str(json).unwrap();
        let enabled: Vec<_> = all
            .into_iter()
            .filter(|m| m.enabled)
            .map(|m| m.label)
            .collect();
        assert_eq!(enabled, ["m1", "mbp"]);
    }
}
