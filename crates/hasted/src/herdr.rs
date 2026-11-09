//! Observing herdr: the status model, notice limiting, and the supervisor that keeps one feed
//! running per machine (the local socket plus the hub's `--machine` polls).
//!
//! Only identifiers and counts are stored, audited or announced. Pane text, titles and working
//! directories never reach this module.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use haste_core::herdr::{AgentStatus, AgentStatusRecord, MachineState, LOCAL_MACHINE};
use haste_core::time::now_secs;
use haste_herdr::backoff::Backoff;
use haste_herdr::command::Runner;
use haste_herdr::diff::{
    apply_event, apply_snapshot, labels, pane_labels, EventOutcome, Transition,
};
use haste_herdr::local::{self, LocalMsg, LocalState, HEALTHY_AFTER};
use haste_herdr::protocol::{FeedError, Snapshot, StatusEvent, HERDR_PROTOCOL};
use haste_herdr::remote::{
    discover, fetch_snapshot, Reachability, Tracker, MACHINE_LIST_REFRESH, REMOTE_BACKOFF_CAP,
    REMOTE_POLL,
};
use haste_proto::{Event, MachineView};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::state::State;

/// Notices per (machine, agent, kind): a burst of this many, then refilling slowly.
pub const NOTICE_BURST: f64 = 3.0;
/// Seconds for one notice token to refill.
pub const NOTICE_REFILL_SECS: f64 = 30.0;
/// At most one blocked-agent notice per machine in this many seconds.
pub const BLOCKED_COALESCE_SECS: f64 = 10.0;

#[derive(Clone)]
pub struct HerdrConfig {
    /// The local herdr socket.
    pub socket: PathBuf,
    /// How the `herdr` CLI is run (remote snapshots, machine list, fallback prompts).
    pub runner: Runner,
    pub observe_local: bool,
    /// Seconds between snapshots of a healthy remote machine.
    pub poll_base: Duration,
    /// How often the saved-machine list is re-read.
    pub list_refresh: Duration,
    /// Set by the supervisor while the local socket link is healthy; the notifier reads it.
    pub local_up: Arc<AtomicBool>,
}

impl HerdrConfig {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            socket: local::default_socket(),
            runner: Runner::from_env(),
            observe_local: true,
            poll_base: REMOTE_POLL,
            list_refresh: MACHINE_LIST_REFRESH,
            local_up: Arc::new(AtomicBool::new(false)),
        }
    }
}

// ---------------------------------------------------------------- notice limiting (pure)

struct Bucket {
    tokens: f64,
    last: f64,
    suppressed: u32,
}

/// Token buckets per (machine, agent, kind), plus blocked-notice coalescing per machine.
#[derive(Default)]
pub struct NoticeLimiter {
    buckets: HashMap<(String, String, String), Bucket>,
    last_blocked: HashMap<String, f64>,
}

impl NoticeLimiter {
    /// May a notice be raised now? `Some(n)` means yes, and `n` earlier notices were suppressed.
    pub fn allow(&mut self, machine: &str, agent: &str, kind: &str, now: f64) -> Option<u32> {
        let b = self
            .buckets
            .entry((machine.to_owned(), agent.to_owned(), kind.to_owned()))
            .or_insert(Bucket {
                tokens: NOTICE_BURST,
                last: now,
                suppressed: 0,
            });
        b.tokens = (b.tokens + (now - b.last).max(0.0) / NOTICE_REFILL_SECS).min(NOTICE_BURST);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Some(std::mem::take(&mut b.suppressed))
        } else {
            b.suppressed += 1;
            None
        }
    }

    /// Blocked transitions on one machine are coalesced to one notice per window.
    pub fn allow_blocked(&mut self, machine: &str, now: f64) -> bool {
        match self.last_blocked.get(machine) {
            Some(last) if now - last < BLOCKED_COALESCE_SECS => false,
            _ => {
                self.last_blocked.insert(machine.to_owned(), now);
                true
            }
        }
    }
}

// ---------------------------------------------------------------- the status model

#[derive(Default)]
struct Machine {
    state: Option<MachineState>,
    agents: Vec<AgentStatusRecord>,
    /// Every pane's workspace label, with or without a detected agent.
    panes: HashMap<String, String>,
    /// The first snapshot after start or recovery only fills the model.
    baseline_done: bool,
    last_labels: Option<BTreeSet<String>>,
}

#[derive(Default)]
pub struct HerdrState {
    machines: Mutex<BTreeMap<String, Machine>>,
    limiter: Mutex<NoticeLimiter>,
    /// Messages that could not be understood and were dropped.
    pub dropped: AtomicU64,
}

impl HerdrState {
    /// Every machine with its state and, only while connected, its agents.
    #[must_use]
    pub fn views(&self) -> Vec<MachineView> {
        let guard = self.machines.lock().expect("herdr state");
        guard
            .iter()
            .map(|(name, m)| {
                let state = m.state.clone().unwrap_or(MachineState::Unreachable);
                let agents = if state == MachineState::Connected {
                    m.agents.clone()
                } else {
                    Vec::new()
                };
                MachineView {
                    machine: name.clone(),
                    state,
                    agents,
                }
            })
            .collect()
    }

    /// The workspace label of a pane on a connected machine.
    #[must_use]
    pub fn workspace_for(&self, machine: &str, pane: &str) -> Option<String> {
        let guard = self.machines.lock().expect("herdr state");
        let m = guard.get(machine)?;
        if m.state != Some(MachineState::Connected) {
            return None;
        }
        m.panes.get(pane).cloned().or_else(|| {
            m.agents
                .iter()
                .find(|a| a.pane_id == pane)
                .map(|a| a.workspace_label.clone())
        })
    }

    /// Agents currently `blocked` on connected machines.
    #[must_use]
    pub fn blocked(&self) -> Vec<AgentStatusRecord> {
        let mut out: Vec<AgentStatusRecord> = self
            .views()
            .into_iter()
            .flat_map(|v| v.agents)
            .filter(|a| a.status == AgentStatus::Blocked)
            .collect();
        out.sort_by_key(|a| a.status_since);
        out
    }

    fn blocked_count(guard: &BTreeMap<String, Machine>) -> usize {
        guard
            .values()
            .flat_map(|m| &m.agents)
            .filter(|a| a.status == AgentStatus::Blocked)
            .count()
    }
}

fn clock_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Audit and announce what a batch of transitions means. Counts and identifiers only.
fn handle_transitions(state: &State, machine: &str, transitions: &[Transition]) {
    for t in transitions {
        let r = &t.record;
        match r.status {
            AgentStatus::Blocked => {
                let blocked_now =
                    HerdrState::blocked_count(&state.herdr.machines.lock().expect("herdr state"));
                state.audit(
                    "herdr",
                    "herdr.blocked",
                    "blocked",
                    Some(machine),
                    Some(&format!(
                        "workspace={} pane={} blocked_now={blocked_now}",
                        r.workspace_id, r.pane_id
                    )),
                );
                let allowed = {
                    let mut lim = state.herdr.limiter.lock().expect("limiter");
                    let now = clock_secs();
                    if lim.allow_blocked(machine, now) {
                        lim.allow(machine, r.agent.as_deref().unwrap_or("-"), "blocked", now)
                    } else {
                        None
                    }
                };
                if let Some(suppressed) = allowed {
                    notice(
                        state,
                        machine,
                        &format!(
                            "{} in '{}' is blocked{}",
                            who(r),
                            r.workspace_label,
                            suffix(suppressed)
                        ),
                    );
                }
            }
            AgentStatus::Done => {
                let allowed = state.herdr.limiter.lock().expect("limiter").allow(
                    machine,
                    r.agent.as_deref().unwrap_or("-"),
                    "done",
                    clock_secs(),
                );
                if let Some(suppressed) = allowed {
                    notice(
                        state,
                        machine,
                        &format!(
                            "{} in '{}' is done{}",
                            who(r),
                            r.workspace_label,
                            suffix(suppressed)
                        ),
                    );
                }
            }
            _ => {}
        }
    }
}

fn who(r: &AgentStatusRecord) -> String {
    r.agent.clone().unwrap_or_else(|| "an agent".to_owned())
}

fn suffix(suppressed: u32) -> String {
    if suppressed == 0 {
        String::new()
    } else {
        format!(" (+{suppressed} suppressed)")
    }
}

fn notice(state: &State, machine: &str, text: &str) {
    state.publish(Event::Notice {
        message: format!("[herdr:{machine}] {text}"),
    });
}

/// Replace a machine's state with a snapshot. The first snapshot after start or recovery is a
/// baseline: it fills the model but announces nothing.
pub fn ingest_snapshot(state: &State, machine: &str, snapshot: &Snapshot) {
    let now = now_secs();
    let new_labels = labels(snapshot);
    let (transitions, labels_changed) = {
        let mut guard = state.herdr.machines.lock().expect("herdr state");
        let m = guard.entry(machine.to_owned()).or_default();
        let (records, transitions) =
            apply_snapshot(machine, &m.agents, snapshot, now, !m.baseline_done);
        m.agents = records;
        m.panes = pane_labels(snapshot);
        m.baseline_done = true;
        let changed = m.last_labels.as_ref() != Some(&new_labels);
        m.last_labels = Some(new_labels.clone());
        (transitions, changed)
    };
    if labels_changed {
        if let Err(e) = state.store.replace_machine_labels(machine, &new_labels) {
            eprintln!("hasted: cannot save workspace labels: {e}");
        }
    }
    handle_transitions(state, machine, &transitions);
}

/// Apply one status event. Returns `true` if a fresh snapshot is needed to place it.
pub fn ingest_event(state: &State, machine: &str, event: &StatusEvent) -> bool {
    let now = now_secs();
    let outcome = {
        let mut guard = state.herdr.machines.lock().expect("herdr state");
        let m = guard.entry(machine.to_owned()).or_default();
        apply_event(machine, &mut m.agents, event, now)
    };
    match outcome {
        EventOutcome::Changed(t) => {
            handle_transitions(state, machine, &[t]);
            false
        }
        EventOutcome::NoChange => false,
        EventOutcome::NeedsSnapshot => true,
    }
}

fn state_name(s: &MachineState) -> &'static str {
    match s {
        MachineState::Connected => "connected",
        MachineState::Unreachable => "unreachable",
        MachineState::Mismatch { .. } => "mismatch",
    }
}

/// Record a machine's new state once per change: audit it and raise one notice.
pub fn set_machine_state(state: &State, machine: &str, new: MachineState) {
    let changed = {
        let mut guard = state.herdr.machines.lock().expect("herdr state");
        let m = guard.entry(machine.to_owned()).or_default();
        if m.state.as_ref() == Some(&new) {
            false
        } else {
            if new != MachineState::Connected {
                m.baseline_done = false;
            }
            m.state = Some(new.clone());
            true
        }
    };
    if !changed {
        return;
    }
    let detail = match &new {
        MachineState::Mismatch { expected, observed } => {
            Some(format!("expected={expected} observed={observed}"))
        }
        _ => None,
    };
    state.audit(
        "herdr",
        "herdr.link",
        state_name(&new),
        Some(machine),
        detail.as_deref(),
    );
    let text = match &new {
        MachineState::Connected => "connected".to_owned(),
        MachineState::Unreachable => "unreachable".to_owned(),
        MachineState::Mismatch { expected, observed } => {
            format!("protocol mismatch: expected {expected}, observed {observed}; not observing this machine")
        }
    };
    notice(state, machine, &text);
}

/// Drop a machine herdr no longer lists (or that was disabled), with its rows and its labels.
pub fn forget_machine(state: &State, machine: &str) {
    state
        .herdr
        .machines
        .lock()
        .expect("herdr state")
        .remove(machine);
    if let Err(e) = state.store.forget_machine(machine) {
        eprintln!("hasted: cannot forget machine labels: {e}");
    }
}

// ---------------------------------------------------------------- supervisor

async fn local_task(state: Arc<State>, cfg: HerdrConfig) {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(local::run(
        cfg.socket.clone(),
        tx,
        Backoff::socket(),
        HEALTHY_AFTER,
    ));
    while let Some(msg) = rx.recv().await {
        let st = Arc::clone(&state);
        match msg {
            LocalMsg::Snapshot(snapshot) => {
                cfg.local_up.store(true, Ordering::SeqCst);
                let _ = tokio::task::spawn_blocking(move || {
                    ingest_snapshot(&st, LOCAL_MACHINE, &snapshot);
                    set_machine_state(&st, LOCAL_MACHINE, MachineState::Connected);
                })
                .await;
            }
            LocalMsg::Event(event) => {
                let _ = tokio::task::spawn_blocking(move || {
                    ingest_event(&st, LOCAL_MACHINE, &event);
                })
                .await;
            }
            LocalMsg::Dropped => {
                state.herdr.dropped.fetch_add(1, Ordering::SeqCst);
                state.audit(
                    "herdr",
                    "herdr.link",
                    "dropped_message",
                    Some(LOCAL_MACHINE),
                    None,
                );
            }
            LocalMsg::State(LocalState::Unreachable) => {
                cfg.local_up.store(false, Ordering::SeqCst);
                let _ = tokio::task::spawn_blocking(move || {
                    set_machine_state(&st, LOCAL_MACHINE, MachineState::Unreachable)
                })
                .await;
            }
            LocalMsg::State(LocalState::Mismatch { observed }) => {
                cfg.local_up.store(false, Ordering::SeqCst);
                let _ = tokio::task::spawn_blocking(move || {
                    set_machine_state(
                        &st,
                        LOCAL_MACHINE,
                        MachineState::Mismatch {
                            expected: HERDR_PROTOCOL,
                            observed,
                        },
                    );
                })
                .await;
            }
        }
    }
}

async fn poller(state: Arc<State>, cfg: HerdrConfig, label: String) {
    let mut tracker = Tracker::with_base(cfg.poll_base);
    loop {
        let runner = cfg.runner.clone();
        let l = label.clone();
        let result = tokio::task::spawn_blocking(move || fetch_snapshot(&runner, &l)).await;
        let mut wait = tracker.delay();
        match result {
            Ok(Ok(snapshot)) => {
                tracker.success();
                let st = Arc::clone(&state);
                let l = label.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    ingest_snapshot(&st, &l, &snapshot);
                    set_machine_state(&st, &l, MachineState::Connected);
                })
                .await;
            }
            Ok(Err(FeedError::Mismatch { expected, observed })) => {
                let st = Arc::clone(&state);
                let l = label.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    set_machine_state(&st, &l, MachineState::Mismatch { expected, observed })
                })
                .await;
                wait = REMOTE_BACKOFF_CAP; // look again slowly: herdr may be upgraded or rolled back
            }
            Ok(Err(_)) | Err(_) => {
                if tracker.failure() == Some(Reachability::Unreachable) {
                    let st = Arc::clone(&state);
                    let l = label.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        set_machine_state(&st, &l, MachineState::Unreachable)
                    })
                    .await;
                }
                wait = tracker.delay();
            }
        }
        sleep(wait).await;
    }
}

async fn remote_manager(state: Arc<State>, cfg: HerdrConfig) {
    let mut pollers: HashMap<String, JoinHandle<()>> = HashMap::new();
    loop {
        let runner = cfg.runner.clone();
        if let Ok(Ok(machines)) = tokio::task::spawn_blocking(move || discover(&runner)).await {
            let wanted: BTreeSet<String> = machines
                .into_iter()
                .map(|m| m.label)
                .filter(|l| l != LOCAL_MACHINE)
                .collect();
            for label in &wanted {
                pollers.entry(label.clone()).or_insert_with(|| {
                    tokio::spawn(poller(Arc::clone(&state), cfg.clone(), label.clone()))
                });
            }
            let gone: Vec<String> = pollers
                .keys()
                .filter(|k| !wanted.contains(*k))
                .cloned()
                .collect();
            for label in gone {
                if let Some(handle) = pollers.remove(&label) {
                    handle.abort();
                }
                let st = Arc::clone(&state);
                let _ = tokio::task::spawn_blocking(move || forget_machine(&st, &label)).await;
            }
        }
        sleep(cfg.list_refresh).await;
    }
}

/// Start the feeds. The returned handles are aborted when the daemon shuts down.
#[must_use]
pub fn start(state: &Arc<State>, cfg: &HerdrConfig) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();
    if cfg.observe_local {
        handles.push(tokio::spawn(local_task(Arc::clone(state), cfg.clone())));
    }
    handles.push(tokio::spawn(remote_manager(Arc::clone(state), cfg.clone())));
    handles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chatty_agent_gets_three_notices_then_a_suppressed_count() {
        let mut l = NoticeLimiter::default();
        let mut shown = 0;
        let mut last_suppressed = 0;
        for i in 0..50 {
            if let Some(s) = l.allow("m1", "claude", "done", f64::from(i)) {
                shown += 1;
                last_suppressed = s;
            }
        }
        // 50 changes over 50 seconds: the burst of 3 plus about 1 refill per 30 s
        assert!((3..=5).contains(&shown), "{shown}");
        assert!(
            last_suppressed > 0,
            "the suppressed ones are counted and reported with the next notice"
        );
    }

    #[test]
    fn buckets_are_per_machine_agent_and_kind() {
        let mut l = NoticeLimiter::default();
        for _ in 0..3 {
            assert!(l.allow("m1", "a", "done", 0.0).is_some());
        }
        assert!(l.allow("m1", "a", "done", 0.0).is_none());
        assert!(l.allow("m1", "b", "done", 0.0).is_some(), "another agent");
        assert!(l.allow("m2", "a", "done", 0.0).is_some(), "another machine");
        assert!(l.allow("m1", "a", "blocked", 0.0).is_some(), "another kind");
    }

    #[test]
    fn the_bucket_refills_over_time() {
        let mut l = NoticeLimiter::default();
        for _ in 0..3 {
            l.allow("m", "a", "k", 0.0);
        }
        assert!(l.allow("m", "a", "k", 1.0).is_none());
        assert!(
            l.allow("m", "a", "k", 31.0).is_some(),
            "30 s later one token is back"
        );
    }

    #[test]
    fn blocked_notices_are_coalesced_to_one_per_machine_per_ten_seconds() {
        let mut l = NoticeLimiter::default();
        assert!(l.allow_blocked("m1", 100.0));
        assert!(!l.allow_blocked("m1", 103.0));
        assert!(!l.allow_blocked("m1", 109.9));
        assert!(l.allow_blocked("m1", 110.0));
        assert!(
            l.allow_blocked("m2", 103.0),
            "another machine has its own window"
        );
    }
}
