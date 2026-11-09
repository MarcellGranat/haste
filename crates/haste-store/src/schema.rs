//! Forward-only migrations tracked by `PRAGMA user_version`.

use rusqlite::Connection;

use crate::{StoreError, StoreResult};

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    "
    CREATE TABLE threads (
        id TEXT PRIMARY KEY,
        source TEXT NOT NULL,
        external_id TEXT NOT NULL,
        subject TEXT NOT NULL,
        participants TEXT NOT NULL,
        last_activity_at INTEGER NOT NULL,
        version INTEGER NOT NULL,
        UNIQUE (source, external_id)
    );
    CREATE TABLE messages (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL REFERENCES threads(id),
        source TEXT NOT NULL,
        external_id TEXT NOT NULL,
        sender_person_id TEXT REFERENCES people(id),
        sender_identifier TEXT,
        body_text TEXT NOT NULL,
        sent_at INTEGER NOT NULL,
        UNIQUE (source, external_id)
    );
    CREATE TABLE people (
        id TEXT PRIMARY KEY,
        display_name TEXT NOT NULL
    );
    CREATE TABLE identifiers (
        kind TEXT NOT NULL,
        value TEXT NOT NULL,
        person_id TEXT NOT NULL REFERENCES people(id),
        PRIMARY KEY (kind, value)
    );
    CREATE TABLE triage (
        object_id TEXT NOT NULL,
        object_kind TEXT NOT NULL,
        priority INTEGER NOT NULL DEFAULT 0,
        snoozed_until INTEGER,
        set_aside INTEGER NOT NULL DEFAULT 0,
        archived INTEGER NOT NULL DEFAULT 0,
        updated_at INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (object_id, object_kind)
    );
    CREATE TABLE links (
        from_kind TEXT NOT NULL,
        from_id TEXT NOT NULL,
        to_kind TEXT NOT NULL,
        to_id TEXT NOT NULL,
        link_type TEXT NOT NULL CHECK (link_type IN ('replies_to','about','references','mentions')),
        PRIMARY KEY (from_kind, from_id, to_kind, to_id, link_type)
    );
    CREATE TABLE principals (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        allowed_actions TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE tokens (
        token_id TEXT PRIMARY KEY,
        principal_id TEXT NOT NULL REFERENCES principals(id),
        salt TEXT NOT NULL,
        hash TEXT NOT NULL,
        revoked INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    );
    CREATE TABLE proposals (
        id TEXT PRIMARY KEY,
        idempotency_key TEXT NOT NULL,
        principal_id TEXT NOT NULL,
        action TEXT NOT NULL,
        payload TEXT NOT NULL,
        source_thread_id TEXT,
        source_thread_version INTEGER,
        state TEXT NOT NULL,
        stale INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        result TEXT,
        delivered INTEGER NOT NULL DEFAULT 0,
        delivery_attempts INTEGER NOT NULL DEFAULT 0,
        UNIQUE (principal_id, idempotency_key)
    );
    CREATE TABLE connector_state (
        source TEXT PRIMARY KEY,
        kill_switch INTEGER NOT NULL DEFAULT 0,
        cursor TEXT
    );
    ",
    // 2: herdr project tags and proposal attribution (nullable, no backfill)
    "
    ALTER TABLE threads ADD COLUMN scope_tags TEXT;
    ALTER TABLE proposals ADD COLUMN project TEXT;
    ALTER TABLE proposals ADD COLUMN origin_machine TEXT;
    CREATE TABLE machine_labels (
        machine TEXT NOT NULL,
        label TEXT NOT NULL,
        PRIMARY KEY (machine, label)
    );
    ",
    // 3: Apple Reminders. Tags live in their own table, keyed by the external id, so a re-sync
    // that rewrites a reminder row can never drop them.
    "
    CREATE TABLE reminders (
        id TEXT PRIMARY KEY,
        external_id TEXT NOT NULL UNIQUE,
        title TEXT NOT NULL,
        notes_snippet TEXT NOT NULL DEFAULT '',
        due_at INTEGER,
        completed INTEGER NOT NULL DEFAULT 0,
        list_name TEXT NOT NULL DEFAULT '',
        remote_modified_at INTEGER NOT NULL DEFAULT 0,
        missing_since INTEGER,
        version INTEGER NOT NULL DEFAULT 1,
        sweep_id TEXT
    );
    CREATE TABLE reminder_scope_tags (
        external_id TEXT NOT NULL,
        tag TEXT NOT NULL,
        PRIMARY KEY (external_id, tag)
    );
    ",
    // 4: Google Calendar events. Same shape as reminders: tags apart, keyed by external id.
    "
    CREATE TABLE events (
        id TEXT PRIMARY KEY,
        external_id TEXT NOT NULL UNIQUE,
        calendar TEXT NOT NULL,
        title TEXT NOT NULL,
        start_at INTEGER NOT NULL,
        end_at INTEGER NOT NULL,
        all_day INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'confirmed',
        attendees TEXT NOT NULL DEFAULT '[]',
        location TEXT NOT NULL DEFAULT '',
        description_snippet TEXT NOT NULL DEFAULT '',
        updated_at INTEGER NOT NULL DEFAULT 0,
        missing_since INTEGER,
        version INTEGER NOT NULL DEFAULT 1,
        sweep_id TEXT
    );
    CREATE INDEX events_start ON events (start_at);
    CREATE TABLE event_scope_tags (
        external_id TEXT NOT NULL,
        tag TEXT NOT NULL,
        PRIMARY KEY (external_id, tag)
    );
    ",
    // 5: how a principal's note appends are decided (`auto` by default, or `approval`).
    "ALTER TABLE principals ADD COLUMN note_mode TEXT NOT NULL DEFAULT 'auto';",
];

/// Highest schema version this binary understands.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub fn migrate(conn: &mut Connection) -> StoreResult<()> {
    let found: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        let err = crate::Store::open(&path).err().expect("must refuse");
        assert!(matches!(err, StoreError::SchemaTooNew { .. }));
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        drop(crate::Store::open(&path).unwrap());
        assert!(crate::Store::open(&path).is_ok());
    }
}
