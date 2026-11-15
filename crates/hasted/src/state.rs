//! Shared daemon state: store, audit writer, event bus, connectors.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use haste_core::audit::{AuditEntry, AuditWriter};
use haste_core::connector::{Connector, Hooks};
use haste_core::model::Source;
use haste_core::notes::MAX_NOTE_APPENDS_PER_DAY;
use haste_core::proposal::DEFAULT_TTL_SECS;
use haste_proto::Event;
use haste_store::Store;
use tokio::sync::{broadcast, Notify};

use crate::notifier::Notifier;

const EVENT_BUS_CAPACITY: usize = 256;
/// Remembered note-append idempotency keys (best effort, in memory).
const NOTE_KEY_MEMORY: usize = 4096;

#[derive(Clone)]
pub struct DaemonConfig {
    /// Directory holding the socket; must be mode `0700`.
    pub run_dir: PathBuf,
    /// Directory holding the database, audit file and human token.
    pub data_dir: PathBuf,
    pub notes_root: PathBuf,
    /// Settings from `notes.json` (person-note options).
    pub notes_settings: crate::notes::NotesSettings,
    pub proposal_ttl_secs: i64,
    pub expiry_tick: Duration,
    pub delivery_tick: Duration,
    /// Delay before delivery attempt `n` is `delivery_backoff * 2^n` (n counts prior attempts).
    pub delivery_backoff: Duration,
    pub notifier: Arc<dyn Notifier>,
    /// Observe herdr (status panel, project attribution, scope). `None` disables it.
    pub herdr: Option<crate::herdr::HerdrConfig>,
}

impl DaemonConfig {
    #[must_use]
    pub fn new(base: &std::path::Path, notifier: Arc<dyn Notifier>) -> Self {
        Self {
            run_dir: base.join("run"),
            data_dir: base.to_path_buf(),
            notes_root: base.join("notes"),
            notes_settings: crate::notes::NotesSettings::default(),
            proposal_ttl_secs: DEFAULT_TTL_SECS,
            expiry_tick: Duration::from_secs(60),
            delivery_tick: Duration::from_secs(5),
            delivery_backoff: Duration::from_secs(10),
            notifier,
            herdr: None,
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.run_dir.join("hasted.sock")
    }

    #[must_use]
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("haste.db")
    }

    #[must_use]
    pub fn audit_path(&self) -> PathBuf {
        self.data_dir.join("audit.jsonl")
    }

    #[must_use]
    pub fn human_token_path(&self) -> PathBuf {
        self.data_dir.join("human.token")
    }
}

/// Recently seen note idempotency keys: a set for lookup plus insertion order for eviction.
pub(crate) type NoteKeys = (HashSet<(String, String)>, Vec<(String, String)>);

pub struct State {
    pub store: Store,
    pub config: DaemonConfig,
    pub(crate) audit: Mutex<AuditWriter>,
    pub events: broadcast::Sender<Event>,
    pub(crate) connectors: RwLock<HashMap<Source, Arc<dyn Connector>>>,
    pub(crate) note_keys: Mutex<NoteKeys>,
    /// Appends made per principal per UTC day (in memory; a loop guard, reset by a restart).
    pub(crate) note_counts: Mutex<HashMap<(String, i64), u32>>,
    /// What herdr reports about agents, per machine.
    pub herdr: crate::herdr::HerdrState,
    /// The Reminders helper, once the connector is started (used to read the permission state).
    pub(crate) reminders_helper: RwLock<Option<Arc<dyn haste_connector_reminders::Helper>>>,
    /// Per-source wake-ups: releasing a source runs its first sync now instead of at the next
    /// tick.
    sync_wakers: Mutex<HashMap<Source, Arc<Notify>>>,
    /// When each source was last woken by a thread being opened.
    refreshed: Mutex<RefreshGate>,
    /// Threads whose earlier history was asked for (once each per run), and the ones waiting.
    history: Mutex<HistoryQueue>,
    history_wakers: Mutex<HashMap<Source, Arc<Notify>>>,
}

#[derive(Default)]
struct HistoryQueue {
    done: HashSet<String>,
    waiting: HashMap<Source, Vec<String>>,
}

/// Remembers when each source was last woken, so opening threads cannot hammer a provider.
#[derive(Default)]
pub(crate) struct RefreshGate(HashMap<Source, std::time::Instant>);

impl RefreshGate {
    /// True (and recorded) at most once per [`REFRESH_MIN_INTERVAL`] for a source.
    fn due(&mut self, source: &Source, now: std::time::Instant) -> bool {
        match self.0.get(source) {
            Some(t) if now.duration_since(*t) < REFRESH_MIN_INTERVAL => false,
            _ => {
                self.0.insert(source.clone(), now);
                true
            }
        }
    }
}

/// The shortest time between two syncs asked for by opening threads of the same source.
pub const REFRESH_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

impl State {
    pub(crate) fn new(store: Store, audit: AuditWriter, config: DaemonConfig) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUS_CAPACITY);
        Self {
            store,
            config,
            audit: Mutex::new(audit),
            events,
            connectors: RwLock::new(HashMap::new()),
            note_keys: Mutex::new((HashSet::new(), Vec::new())),
            note_counts: Mutex::new(HashMap::new()),
            herdr: crate::herdr::HerdrState::default(),
            reminders_helper: RwLock::new(None),
            sync_wakers: Mutex::new(HashMap::new()),
            refreshed: Mutex::new(RefreshGate::default()),
            history: Mutex::new(HistoryQueue::default()),
            history_wakers: Mutex::new(HashMap::new()),
        }
    }

    /// Register a connector (real ones arrive in later changes; tests use doubles).
    /// The wake-up for a source's sync loop.
    #[must_use]
    pub fn sync_waker(&self, source: &Source) -> Arc<Notify> {
        let mut wakers = self
            .sync_wakers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(wakers.entry(source.clone()).or_default())
    }

    /// May a thread open wake this source's sync now? True at most once per
    /// [`REFRESH_MIN_INTERVAL`]; the moment is recorded when it says yes.
    pub fn refresh_due(&self, source: &Source) -> bool {
        self.refreshed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .due(source, std::time::Instant::now())
    }

    /// The wake-up for a source's history reads.
    #[must_use]
    pub fn history_waker(&self, source: &Source) -> Arc<Notify> {
        let mut wakers = self
            .history_wakers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(wakers.entry(source.clone()).or_default())
    }

    /// Ask for the earlier messages of a thread: once per thread per run. Returns whether it
    /// was queued (false when it was asked for before).
    pub fn queue_history(&self, source: &Source, thread_id: &str) -> bool {
        let mut q = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !q.done.insert(thread_id.to_owned()) {
            return false;
        }
        q.waiting
            .entry(source.clone())
            .or_default()
            .push(thread_id.to_owned());
        drop(q);
        self.history_waker(source).notify_one();
        true
    }

    /// The threads waiting for a history read of this source.
    pub fn take_history(&self, source: &Source) -> Vec<String> {
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiting
            .remove(source)
            .unwrap_or_default()
    }

    /// A history read failed in a way worth trying again on the next open.
    pub fn forget_history(&self, thread_id: &str) {
        self.history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .done
            .remove(thread_id);
    }

    /// Ask a source's sync loop to run a cycle now (a permit is kept if it is busy).
    pub fn wake_sync(&self, source: &Source) {
        self.sync_waker(source).notify_one();
    }

    pub fn register_connector(&self, connector: Arc<dyn Connector>) {
        // A connector that asks for it starts engaged the first time it is ever registered. A
        // persisted choice (engaged or released) is never overridden.
        if connector.default_engaged() {
            let source = connector.source();
            let known = self
                .store
                .list_kill_switches()
                .map(|all| all.iter().any(|(s, _)| s == &source))
                .unwrap_or(true);
            if !known {
                let _ = self.store.set_kill_switch(&source, true);
                self.audit(
                    "system",
                    "connector.engage",
                    "first_install",
                    Some(&source.as_name()),
                    None,
                );
            }
        }
        if let Ok(mut map) = self.connectors.write() {
            map.insert(connector.source(), connector);
        }
    }

    /// Sources that have a connector loaded right now.
    #[must_use]
    pub fn registered_sources(&self) -> Vec<Source> {
        self.connectors
            .read()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn connector_for(&self, source: &Source) -> Option<Arc<dyn Connector>> {
        self.connectors.read().ok()?.get(source).cloned()
    }

    /// Write one audit record. Failures are reported on stderr, never silently dropped.
    pub fn audit(
        &self,
        principal: &str,
        action: &str,
        outcome: &str,
        object: Option<&str>,
        detail: Option<&str>,
    ) {
        let entry = AuditEntry {
            principal,
            action,
            outcome,
            object,
            detail,
        };
        let result = match self.audit.lock() {
            Ok(mut w) => w.append(&entry).map(|_| ()),
            Err(_) => Err(std::io::Error::other("audit lock poisoned")),
        };
        // The same record goes to the operational log (same counts-and-labels rule), with a level
        // that makes refusals and failures easy to find.
        let level = match outcome {
            "ok" | "auto" | "sent" | "released" | "engaged" | "approved" | "created" => {
                haste_core::log::Level::Info
            }
            "ambiguous" | "needs_check" | "breaker_opened" | "error" | "failed" => {
                haste_core::log::Level::Error
            }
            _ => haste_core::log::Level::Warn,
        };
        let mut fields = vec![
            haste_core::log::id("principal", principal),
            haste_core::log::id("action", action),
            haste_core::log::id("outcome", outcome),
        ];
        // note paths are left out: a file name can be a person's name
        if let Some(o) = object.filter(|o| !o.contains('/')) {
            fields.push(haste_core::log::id("object", o));
        }
        if let Some(d) = detail {
            fields.push(haste_core::log::d("detail", d));
        }
        haste_core::log::event(level, "audit", "record", &fields);
        if let Err(e) = result {
            haste_core::log::error("audit", "write_failed", &[]);
            eprintln!("hasted: audit write failed: {e}");
        }
    }

    pub fn publish(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Whether `actor` may still append today (the per-day cap is a loop guard).
    pub(crate) fn note_budget_left(&self, actor: &str, now: i64) -> bool {
        let day = now.div_euclid(86_400);
        self.note_counts.lock().is_ok_and(|c| {
            c.get(&(actor.to_owned(), day)).copied().unwrap_or(0) < MAX_NOTE_APPENDS_PER_DAY
        })
    }

    pub(crate) fn note_budget_use(&self, actor: &str, now: i64) {
        let day = now.div_euclid(86_400);
        if let Ok(mut counts) = self.note_counts.lock() {
            counts.retain(|(_, d), _| *d >= day - 1);
            *counts.entry((actor.to_owned(), day)).or_insert(0) += 1;
        }
    }

    /// Remember a note idempotency key; returns `false` if it was already seen.
    pub(crate) fn remember_note_key(&self, principal: &str, key: &str) -> bool {
        let Ok(mut guard) = self.note_keys.lock() else {
            return true;
        };
        let entry = (principal.to_owned(), key.to_owned());
        if guard.0.contains(&entry) {
            return false;
        }
        guard.0.insert(entry.clone());
        guard.1.push(entry);
        if guard.1.len() > NOTE_KEY_MEMORY {
            let old = guard.1.remove(0);
            guard.0.remove(&old);
        }
        true
    }
}

impl Hooks for State {
    fn kill_switch_engaged(&self, source: &Source) -> bool {
        // Fail closed: if the switch cannot be read the connector stays off.
        self.store.kill_switch_engaged(source).unwrap_or(true)
    }

    fn set_kill_switch(&self, source: &Source, engaged: bool) {
        if let Err(e) = self.store.set_kill_switch(source, engaged) {
            eprintln!("hasted: cannot persist kill switch: {e}");
        }
    }

    fn audit(&self, action: &str, source: &Source, outcome: &str) {
        State::audit(
            self,
            "connector",
            action,
            outcome,
            Some(&source.as_name()),
            None,
        );
    }

    fn notify(&self, message: &str) {
        self.publish(Event::Notice {
            message: message.to_owned(),
        });
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_source_is_woken_by_opening_threads_at_most_once_per_interval() {
        let mut gate = RefreshGate::default();
        let t0 = Instant::now();
        assert!(gate.due(&Source::Whatsapp, t0));
        assert!(!gate.due(&Source::Whatsapp, t0 + REFRESH_MIN_INTERVAL / 2));
        // another source has its own clock
        assert!(gate.due(&Source::Gmail, t0));
        assert!(gate.due(&Source::Whatsapp, t0 + REFRESH_MIN_INTERVAL));
    }
}
