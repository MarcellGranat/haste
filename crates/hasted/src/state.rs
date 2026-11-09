//! Shared daemon state: store, audit writer, event bus, connectors.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use haste_core::audit::{AuditEntry, AuditWriter};
use haste_core::connector::{Connector, Hooks};
use haste_core::model::Source;
use haste_core::proposal::DEFAULT_TTL_SECS;
use haste_proto::Event;
use haste_store::Store;
use tokio::sync::broadcast;

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
    /// What herdr reports about agents, per machine.
    pub herdr: crate::herdr::HerdrState,
    /// The Reminders helper, once the connector is started (used to read the permission state).
    pub(crate) reminders_helper: RwLock<Option<Arc<dyn haste_connector_reminders::Helper>>>,
}

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
            herdr: crate::herdr::HerdrState::default(),
            reminders_helper: RwLock::new(None),
        }
    }

    /// Register a connector (real ones arrive in later changes; tests use doubles).
    pub fn register_connector(&self, connector: Arc<dyn Connector>) {
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
        if let Err(e) = result {
            eprintln!("hasted: audit write failed: {e}");
        }
    }

    pub fn publish(&self, event: Event) {
        let _ = self.events.send(event);
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
