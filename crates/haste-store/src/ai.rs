//! Cache of the local AI helpers' results (`ai_results`): one row per kind, thread version and
//! memory set. The payload is the suggestion shown to the human (a draft text or the TODO JSON);
//! the table holds no prompt.

use rusqlite::{params, OptionalExtension};

use crate::{Store, StoreResult};

/// One cached result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiResultRow {
    pub payload: String,
    /// The outcome label (`ok`, `refused`).
    pub outcome: String,
    pub created_at: i64,
}

/// What a result is cached under.
#[derive(Debug, Clone, Copy)]
pub struct AiKey<'a> {
    /// `draft` or `todo`.
    pub kind: &'a str,
    pub thread_id: &'a str,
    pub thread_version: u64,
    /// The fingerprint of the memories that went into the call.
    pub memory_fingerprint: &'a str,
}

impl Store {
    /// The cached result for a key, if any.
    ///
    /// # Errors
    /// Database errors.
    pub fn ai_result_get(&self, key: &AiKey<'_>) -> StoreResult<Option<AiResultRow>> {
        let (kind, thread_id, fp) = (
            key.kind.to_owned(),
            key.thread_id.to_owned(),
            key.memory_fingerprint.to_owned(),
        );
        let version = i64::try_from(key.thread_version).unwrap_or(i64::MAX);
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT payload, outcome, created_at FROM ai_results \
                     WHERE kind = ?1 AND thread_id = ?2 AND thread_version = ?3 AND memory_fingerprint = ?4",
                    params![kind, thread_id, version, fp],
                    |r| {
                        Ok(AiResultRow {
                            payload: r.get(0)?,
                            outcome: r.get(1)?,
                            created_at: r.get(2)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    /// Store a result; a result for the same key is replaced.
    ///
    /// # Errors
    /// Database errors.
    pub fn ai_result_put(
        &self,
        key: &AiKey<'_>,
        payload: &str,
        outcome: &str,
        now: i64,
    ) -> StoreResult<()> {
        let (kind, thread_id, fp) = (
            key.kind.to_owned(),
            key.thread_id.to_owned(),
            key.memory_fingerprint.to_owned(),
        );
        let (payload, outcome) = (payload.to_owned(), outcome.to_owned());
        let version = i64::try_from(key.thread_version).unwrap_or(i64::MAX);
        self.write(move |conn| {
            conn.execute(
                "INSERT INTO ai_results \
                 (kind, thread_id, thread_version, memory_fingerprint, payload, outcome, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(kind, thread_id, thread_version, memory_fingerprint) DO UPDATE SET \
                 payload = excluded.payload, outcome = excluded.outcome, created_at = excluded.created_at",
                params![kind, thread_id, version, fp, payload, outcome, now],
            )?;
            Ok(())
        })
    }

    /// Delete the results created before `cutoff`. Returns how many went.
    ///
    /// # Errors
    /// Database errors.
    pub fn ai_results_delete_older_than(&self, cutoff: i64) -> StoreResult<usize> {
        self.write(move |conn| {
            Ok(conn.execute("DELETE FROM ai_results WHERE created_at < ?1", [cutoff])?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::AiKey;
    use crate::test_store;

    fn key<'a>(kind: &'a str, thread: &'a str, version: u64, fp: &'a str) -> AiKey<'a> {
        AiKey {
            kind,
            thread_id: thread,
            thread_version: version,
            memory_fingerprint: fp,
        }
    }

    #[test]
    fn a_result_is_found_by_its_whole_key_only() {
        let (_d, s) = test_store();
        assert!(s
            .ai_result_get(&key("draft", "t1", 1, "fp"))
            .unwrap()
            .is_none());
        s.ai_result_put(&key("draft", "t1", 1, "fp"), "Hello", "ok", 100)
            .unwrap();
        let row = s
            .ai_result_get(&key("draft", "t1", 1, "fp"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.payload.as_str(), row.outcome.as_str(), row.created_at),
            ("Hello", "ok", 100)
        );
        assert!(s
            .ai_result_get(&key("todo", "t1", 1, "fp"))
            .unwrap()
            .is_none());
        assert!(
            s.ai_result_get(&key("draft", "t1", 2, "fp"))
                .unwrap()
                .is_none(),
            "new thread version"
        );
        assert!(
            s.ai_result_get(&key("draft", "t1", 1, "other"))
                .unwrap()
                .is_none(),
            "memory edited"
        );
    }

    #[test]
    fn putting_the_same_key_again_replaces_the_result() {
        let (_d, s) = test_store();
        s.ai_result_put(&key("draft", "t1", 1, "fp"), "one", "ok", 100)
            .unwrap();
        s.ai_result_put(&key("draft", "t1", 1, "fp"), "", "refused", 200)
            .unwrap();
        let row = s
            .ai_result_get(&key("draft", "t1", 1, "fp"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.payload.as_str(), row.outcome.as_str(), row.created_at),
            ("", "refused", 200)
        );
    }

    #[test]
    fn old_results_are_deleted() {
        let (_d, s) = test_store();
        s.ai_result_put(&key("draft", "t1", 1, "fp"), "old", "ok", 100)
            .unwrap();
        s.ai_result_put(&key("draft", "t2", 1, "fp"), "new", "ok", 900)
            .unwrap();
        assert_eq!(s.ai_results_delete_older_than(500).unwrap(), 1);
        assert!(s
            .ai_result_get(&key("draft", "t1", 1, "fp"))
            .unwrap()
            .is_none());
        assert!(s
            .ai_result_get(&key("draft", "t2", 1, "fp"))
            .unwrap()
            .is_some());
    }
}
