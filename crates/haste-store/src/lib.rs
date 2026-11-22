//! SQLite persistence for haste: one writer thread, a small pool of read-only connections.

mod ai;
mod auth;
mod events;
mod herdr;
mod inbox;
mod proposals;
mod reminders;
mod schema;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Mutex;
use std::thread;

use rusqlite::{Connection, OpenFlags};

pub use ai::{AiKey, AiResultRow};
pub use auth::{AuthError, AuthInfo, HUMAN_PRINCIPAL_ID};
pub use events::EventCounts;
pub use inbox::{ThreadFilter, TriageUpdate};
pub use proposals::{ApproveOutcome, NewProposal, MAX_DELIVERY_ATTEMPTS};
pub use reminders::{ExecutionStart, UpsertCounts};
pub use schema::SCHEMA_VERSION;

const WRITER_QUEUE_DEPTH: usize = 256;
const MAX_POOLED_READERS: usize = 4;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Validation(String),
    NotFound(String),
    IllegalTransition(String),
    /// Approval or send needs the human to acknowledge that the thread changed.
    StaleNeedsAcknowledgement,
    SchemaTooNew {
        found: u32,
        supported: u32,
    },
    Json(serde_json::Error),
    WriterGone,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "database error: {e}"),
            Self::Validation(m) => write!(f, "validation error: {m}"),
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::IllegalTransition(m) => write!(f, "illegal transition: {m}"),
            Self::StaleNeedsAcknowledgement => {
                write!(
                    f,
                    "thread changed since the proposal; acknowledgement required"
                )
            }
            Self::SchemaTooNew { found, supported } => write!(
                f,
                "database schema version {found} is newer than supported version {supported}"
            ),
            Self::Json(e) => write!(f, "json error: {e}"),
            Self::WriterGone => write!(f, "store writer is not running"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

pub struct Store {
    path: PathBuf,
    writer: SyncSender<Job>,
    readers: Mutex<Vec<Connection>>,
}

impl Store {
    /// Open the database, enable WAL, apply migrations, and start the writer thread.
    ///
    /// # Errors
    /// Fails on I/O errors or when the stored schema is newer than this binary supports.
    pub fn open(path: &Path) -> StoreResult<Self> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&mut conn)?;
        let (tx, rx) = sync_channel::<Job>(WRITER_QUEUE_DEPTH);
        thread::Builder::new()
            .name("haste-store-writer".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(|e| StoreError::Validation(format!("cannot start writer: {e}")))?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: tx,
            readers: Mutex::new(Vec::new()),
        })
    }

    /// Run `f` on the single writer connection and wait for its result.
    ///
    /// # Errors
    /// Propagates `f`'s error, or [`StoreError::WriterGone`] if the writer stopped.
    pub fn write<R, F>(&self, f: F) -> StoreResult<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> StoreResult<R> + Send + 'static,
    {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let job: Job = Box::new(move |conn| {
            let _ = reply_tx.send(f(conn));
        });
        self.writer.send(job).map_err(|_| StoreError::WriterGone)?;
        reply_rx.recv().map_err(|_| StoreError::WriterGone)?
    }

    /// Run `f` on a pooled read-only connection.
    ///
    /// # Errors
    /// Propagates `f`'s error or connection errors.
    pub fn read<R, F>(&self, f: F) -> StoreResult<R>
    where
        F: FnOnce(&Connection) -> StoreResult<R>,
    {
        let pooled = self
            .readers
            .lock()
            .map_err(|_| StoreError::WriterGone)?
            .pop();
        let conn = match pooled {
            Some(c) => c,
            None => Connection::open_with_flags(
                &self.path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?,
        };
        let result = f(&conn);
        if let Ok(mut pool) = self.readers.lock() {
            if pool.len() < MAX_POOLED_READERS {
                pool.push(conn);
            }
        }
        result
    }
}

#[cfg(test)]
pub(crate) fn test_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("haste.db")).expect("open store");
    (dir, store)
}
