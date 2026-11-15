//! Principals, token verification and per-connector kill switches.

use haste_core::auth::{issue_token, split_token, verify_secret};
use haste_core::model::Source;
use haste_core::policy::{Action, NoteMode, Principal, PrincipalKind};
use haste_core::time::now_secs;
use rusqlite::{params, OptionalExtension};

use crate::{Store, StoreError, StoreResult};

/// Id of the single human principal created at bootstrap.
pub const HUMAN_PRINCIPAL_ID: &str = "human";

/// A verified caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthInfo {
    pub principal: Principal,
    /// The subset of actions this token may even ask for (checked before the gate).
    pub allowed: Vec<Action>,
    /// How this principal's note appends are decided.
    pub note_mode: NoteMode,
}

/// Deliberately uniform: callers learn nothing about why authentication failed.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    Unauthenticated,
    Store(String),
}

impl Store {
    /// Register a principal and return its token. The token is never retrievable again.
    ///
    /// # Errors
    /// Validation error if the principal already exists.
    pub fn register_principal(
        &self,
        principal_id: &str,
        kind: PrincipalKind,
        allowed: &[Action],
    ) -> StoreResult<String> {
        let id = principal_id.to_owned();
        let allowed_json =
            serde_json::to_string(&allowed.iter().map(|a| a.as_str()).collect::<Vec<_>>())?;
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM principals WHERE id = ?1)",
                [&id],
                |r| r.get(0),
            )?;
            if exists {
                return Err(StoreError::Validation(format!("principal {id} already exists")));
            }
            let now = now_secs();
            tx.execute(
                "INSERT INTO principals (id, kind, allowed_actions, created_at) VALUES (?1,?2,?3,?4)",
                params![id, kind.as_str(), allowed_json, now],
            )?;
            let issued = issue_token();
            tx.execute(
                "INSERT INTO tokens (token_id, principal_id, salt, hash, created_at) VALUES (?1,?2,?3,?4,?5)",
                params![issued.token_id, id, issued.salt, issued.hash, now],
            )?;
            tx.commit()?;
            Ok(issued.token)
        })
    }

    /// Create the human principal if absent. Returns the token only when newly created.
    ///
    /// # Errors
    /// Database errors.
    pub fn bootstrap_human(&self) -> StoreResult<Option<String>> {
        match self.register_principal(HUMAN_PRINCIPAL_ID, PrincipalKind::Human, &Action::ALL) {
            Ok(token) => Ok(Some(token)),
            Err(StoreError::Validation(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Verify a presented token. Error values carry no token material.
    ///
    /// # Errors
    /// [`AuthError::Unauthenticated`] for missing, malformed, wrong or revoked tokens.
    pub fn authenticate(&self, token: &str) -> Result<AuthInfo, AuthError> {
        let (token_id, secret) = split_token(token).ok_or(AuthError::Unauthenticated)?;
        let token_id = token_id.to_owned();
        let secret = secret.to_owned();
        let row = self
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT t.salt, t.hash, t.revoked, p.id, p.kind, p.allowed_actions, p.note_mode \
                         FROM tokens t JOIN principals p ON p.id = t.principal_id WHERE t.token_id = ?1",
                        [&token_id],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, i64>(2)?,
                                r.get::<_, String>(3)?,
                                r.get::<_, String>(4)?,
                                r.get::<_, String>(5)?,
                                r.get::<_, String>(6)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .map_err(|e| AuthError::Store(e.to_string()))?;
        let Some((salt, hash, revoked, principal_id, kind, allowed_json, note_mode)) = row else {
            return Err(AuthError::Unauthenticated);
        };
        if revoked != 0 || !verify_secret(&salt, &hash, &secret) {
            return Err(AuthError::Unauthenticated);
        }
        let kind = PrincipalKind::parse(&kind).ok_or(AuthError::Unauthenticated)?;
        let names: Vec<String> = serde_json::from_str(&allowed_json).unwrap_or_default();
        let allowed = names.iter().filter_map(|n| Action::parse(n)).collect();
        Ok(AuthInfo {
            principal: Principal {
                id: principal_id,
                kind,
            },
            allowed,
            note_mode: NoteMode::parse(&note_mode).unwrap_or_default(),
        })
    }

    /// Set how a principal's note appends are decided. Human-only (enforced by the daemon).
    ///
    /// # Errors
    /// [`StoreError::NotFound`] for an unknown principal.
    pub fn set_note_mode(&self, principal_id: &str, mode: NoteMode) -> StoreResult<()> {
        let id = principal_id.to_owned();
        self.write(move |conn| {
            let n = conn.execute(
                "UPDATE principals SET note_mode = ?2 WHERE id = ?1",
                params![id, mode.as_str()],
            )?;
            if n == 0 {
                return Err(StoreError::NotFound(format!("principal {id}")));
            }
            Ok(())
        })
    }

    /// Revoke every token of a principal. Revocation is permanent.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if the principal has no tokens.
    pub fn revoke_tokens(&self, principal_id: &str) -> StoreResult<()> {
        let id = principal_id.to_owned();
        self.write(move |conn| {
            let n = conn.execute(
                "UPDATE tokens SET revoked = 1 WHERE principal_id = ?1",
                [&id],
            )?;
            if n == 0 {
                return Err(StoreError::NotFound(format!("principal {id}")));
            }
            Ok(())
        })
    }

    /// # Errors
    /// Database errors.
    pub fn kill_switch_engaged(&self, source: &Source) -> StoreResult<bool> {
        let name = source.as_name();
        self.read(move |conn| {
            let v: Option<i64> = conn
                .query_row(
                    "SELECT kill_switch FROM connector_state WHERE source = ?1",
                    [&name],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(v.unwrap_or(0) != 0)
        })
    }

    /// Every source with a connector_state row and whether its switch is engaged.
    ///
    /// # Errors
    /// Database errors.
    pub fn list_kill_switches(&self) -> StoreResult<Vec<(Source, bool)>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare("SELECT source, kill_switch FROM connector_state ORDER BY source")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    Source::from(r.get::<_, String>(0)?),
                    r.get::<_, i64>(1)? != 0,
                ))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// The saved sync cursor for a connector, if any.
    ///
    /// # Errors
    /// Database errors.
    pub fn connector_cursor(&self, source: &Source) -> StoreResult<Option<String>> {
        let name = source.as_name();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT cursor FROM connector_state WHERE source = ?1",
                    [&name],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
    }

    /// Forget every thread and message of a source (with their triage marks and links) and its
    /// sync cursor, so the next cycle reads from the start. Returns the number of threads.
    ///
    /// # Errors
    /// Database errors.
    pub fn purge_source(&self, source: &Source) -> StoreResult<usize> {
        let name = source.as_name();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let ids: Vec<String> = {
                let mut stmt = tx.prepare("SELECT id FROM threads WHERE source = ?1")?;
                let rows = stmt.query_map([&name], |r| r.get(0))?;
                rows.collect::<Result<_, _>>()?
            };
            for id in &ids {
                tx.execute(
                    "DELETE FROM triage WHERE object_kind = 'thread' AND object_id = ?1",
                    [id],
                )?;
                tx.execute(
                    "DELETE FROM links WHERE (from_kind = 'thread' AND from_id = ?1) \
                     OR (to_kind = 'thread' AND to_id = ?1)",
                    [id],
                )?;
                tx.execute("DELETE FROM thread_flags WHERE thread_id = ?1", [id])?;
                tx.execute("DELETE FROM messages WHERE thread_id = ?1", [id])?;
                tx.execute("DELETE FROM threads WHERE id = ?1", [id])?;
            }
            tx.execute(
                "UPDATE connector_state SET cursor = NULL WHERE source = ?1",
                [&name],
            )?;
            tx.commit()?;
            Ok(ids.len())
        })
    }

    /// Save the sync cursor without touching the kill switch.
    ///
    /// # Errors
    /// Database errors.
    pub fn set_connector_cursor(&self, source: &Source, cursor: Option<&str>) -> StoreResult<()> {
        let name = source.as_name();
        let cursor = cursor.map(str::to_owned);
        self.write(move |conn| {
            conn.execute(
                "INSERT INTO connector_state (source, cursor) VALUES (?1, ?2) \
                 ON CONFLICT(source) DO UPDATE SET cursor = excluded.cursor",
                params![name, cursor],
            )?;
            Ok(())
        })
    }

    /// # Errors
    /// Database errors.
    pub fn set_kill_switch(&self, source: &Source, engaged: bool) -> StoreResult<()> {
        let name = source.as_name();
        self.write(move |conn| {
            conn.execute(
                "INSERT INTO connector_state (source, kill_switch) VALUES (?1, ?2) \
                 ON CONFLICT(source) DO UPDATE SET kill_switch = excluded.kill_switch",
                params![name, i64::from(engaged)],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_store;

    #[test]
    fn token_round_trip_and_binding() {
        let (_d, s) = test_store();
        let token = s
            .register_principal("agent-a", PrincipalKind::Agent, &[Action::Read])
            .unwrap();
        let info = s.authenticate(&token).unwrap();
        assert_eq!(info.principal.id, "agent-a");
        assert_eq!(info.allowed, vec![Action::Read]);
    }

    #[test]
    fn missing_malformed_wrong_and_revoked_tokens_fail_uniformly() {
        let (_d, s) = test_store();
        let token = s
            .register_principal("agent-a", PrincipalKind::Agent, &[Action::Read])
            .unwrap();
        assert_eq!(s.authenticate(""), Err(AuthError::Unauthenticated));
        assert_eq!(s.authenticate("garbage"), Err(AuthError::Unauthenticated));
        let (id, _) = token.split_once('.').unwrap();
        assert_eq!(
            s.authenticate(&format!("{id}.deadbeef")),
            Err(AuthError::Unauthenticated)
        );
        s.revoke_tokens("agent-a").unwrap();
        assert_eq!(s.authenticate(&token), Err(AuthError::Unauthenticated));
    }

    #[test]
    fn only_hash_is_stored() {
        let (d, s) = test_store();
        let token = s
            .register_principal("agent-a", PrincipalKind::Agent, &[Action::Read])
            .unwrap();
        let (_, secret) = token.split_once('.').unwrap();
        let conn = rusqlite::Connection::open(d.path().join("haste.db")).unwrap();
        let rows: Vec<String> = conn
            .prepare("SELECT salt || hash FROM tokens")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(rows.iter().all(|r| !r.contains(secret)));
    }

    #[test]
    fn cursor_is_kept_when_the_kill_switch_changes() {
        let (_d, s) = test_store();
        s.set_connector_cursor(&Source::Gmail, Some("123")).unwrap();
        s.set_kill_switch(&Source::Gmail, true).unwrap();
        s.set_kill_switch(&Source::Gmail, false).unwrap();
        assert_eq!(
            s.connector_cursor(&Source::Gmail).unwrap().as_deref(),
            Some("123")
        );
        assert_eq!(s.connector_cursor(&Source::Icloud).unwrap(), None);
        assert_eq!(
            s.list_kill_switches().unwrap(),
            vec![(Source::Gmail, false)]
        );
    }

    #[test]
    fn kill_switch_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        {
            let s = Store::open(&path).unwrap();
            s.set_kill_switch(&Source::Whatsapp, true).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert!(s.kill_switch_engaged(&Source::Whatsapp).unwrap());
        assert!(!s.kill_switch_engaged(&Source::Gmail).unwrap());
    }
}
