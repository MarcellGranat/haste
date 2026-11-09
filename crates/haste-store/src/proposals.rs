//! Proposal persistence and lifecycle. Human-only transitions are enforced by the daemon,
//! which never routes agent principals to approve/reject/send.

use haste_core::auth::new_id;
use haste_core::model::ObjectKind;
use haste_core::proposal::{transition, Payload, Proposal, ProposalState};
use haste_core::send::{MAX_SENDS_PER_HOUR, SEND_WINDOW_SECS};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::inbox::{apply_triage, thread_version, TriageUpdate};
use crate::{Store, StoreError, StoreResult};

/// Maximum delivery attempts per proposal result.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone)]
pub struct NewProposal {
    pub principal_id: String,
    pub idempotency_key: String,
    pub payload: Payload,
    pub thread_id: Option<String>,
    pub now: i64,
    pub ttl_secs: i64,
    /// Workspace label of the proposing agent's pane, if the daemon could resolve it.
    pub project: Option<String>,
    /// Herdr machine the proposing agent runs on.
    pub origin_machine: Option<String>,
}

/// Result of an approve attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproveOutcome {
    pub proposal: Proposal,
}

pub(crate) const COLUMNS: &str = "id, idempotency_key, principal_id, payload, source_thread_id, \
    source_thread_version, state, stale, created_at, updated_at, expires_at, result, project, origin_machine";

pub(crate) fn proposal_from_row(row: &Row<'_>) -> rusqlite::Result<Proposal> {
    let payload: String = row.get(3)?;
    let state: String = row.get(6)?;
    let to_u64 = |v: Option<i64>| v.map(|n| u64::try_from(n).unwrap_or(0));
    Ok(Proposal {
        id: row.get(0)?,
        idempotency_key: row.get(1)?,
        principal_id: row.get(2)?,
        payload: serde_json::from_str(&payload).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        source_thread_id: row.get(4)?,
        source_thread_version: to_u64(row.get(5)?),
        state: ProposalState::parse(&state).unwrap_or(ProposalState::Pending),
        stale: row.get::<_, i64>(7)? != 0,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        expires_at: row.get(10)?,
        result: row.get(11)?,
        project: row.get(12)?,
        origin_machine: row.get(13)?,
    })
}

pub(crate) fn load(conn: &Connection, id: &str) -> StoreResult<Proposal> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM proposals WHERE id = ?1"),
        [id],
        proposal_from_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("proposal {id}")))
}

pub(crate) fn set_state(
    conn: &Connection,
    p: &Proposal,
    to: ProposalState,
    result: Option<&str>,
    now: i64,
) -> StoreResult<Proposal> {
    let new_state = transition(p.state, to).map_err(|e| {
        StoreError::IllegalTransition(format!("{} -> {}", e.from.as_str(), e.to.as_str()))
    })?;
    conn.execute(
        "UPDATE proposals SET state = ?2, updated_at = ?3, result = COALESCE(?4, result) WHERE id = ?1",
        params![p.id, new_state.as_str(), now, result],
    )?;
    load(conn, &p.id)
}

/// Reflect the current thread version in `stale` for open proposals (read-only).
fn apply_live_stale(conn: &Connection, p: &mut Proposal) -> StoreResult<()> {
    if matches!(p.state, ProposalState::Pending | ProposalState::Approved) {
        if crate::reminders::reminder_stale(conn, &p.payload)?
            || crate::events::event_stale(conn, &p.payload)?
        {
            p.stale = true;
        }
        if let (Some(id), Some(recorded)) = (&p.source_thread_id, p.source_thread_version) {
            if thread_version(conn, id)?.is_some_and(|now| now != recorded) {
                p.stale = true;
            }
        }
    }
    Ok(())
}

/// Recompute the stale flag from the current thread version and persist it.
pub(crate) fn refresh_stale(conn: &Connection, p: &Proposal) -> StoreResult<bool> {
    let stale = match (&p.source_thread_id, p.source_thread_version) {
        (Some(id), Some(recorded)) => thread_version(conn, id)?.is_some_and(|now| now != recorded),
        _ => false,
    } || crate::reminders::reminder_stale(conn, &p.payload)?
        || crate::events::event_stale(conn, &p.payload)?;
    if stale != p.stale {
        conn.execute(
            "UPDATE proposals SET stale = ?2 WHERE id = ?1",
            params![p.id, i64::from(stale)],
        )?;
    }
    Ok(stale)
}

impl Store {
    /// Create a proposal, or return the existing one for the same `(principal, key)`.
    /// Older `pending`/`approved` proposals with the same principal, action and thread are
    /// superseded. Returns `(proposal, created)` plus the ids superseded.
    ///
    /// # Errors
    /// Validation error for an empty idempotency key.
    pub fn create_proposal(
        &self,
        new: &NewProposal,
    ) -> StoreResult<(Proposal, bool, Vec<Proposal>)> {
        if new.idempotency_key.trim().is_empty() {
            return Err(StoreError::Validation("idempotency key is required".into()));
        }
        let new = new.clone();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let existing = tx
                .query_row(
                    &format!("SELECT {COLUMNS} FROM proposals WHERE principal_id = ?1 AND idempotency_key = ?2"),
                    params![new.principal_id, new.idempotency_key],
                    proposal_from_row,
                )
                .optional()?;
            if let Some(p) = existing {
                return Ok((p, false, Vec::new()));
            }
            let action = new.payload.action();
            let mut superseded = Vec::new();
            if let Some(thread_id) = &new.thread_id {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLUMNS} FROM proposals WHERE principal_id = ?1 AND action = ?2 \
                     AND source_thread_id = ?3 AND state IN ('pending','approved')"
                ))?;
                let older: Vec<Proposal> = stmt
                    .query_map(params![new.principal_id, action.as_str(), thread_id], proposal_from_row)?
                    .collect::<Result<_, _>>()?;
                drop(stmt);
                for old in older {
                    superseded.push(set_state(&tx, &old, ProposalState::Superseded, None, new.now)?);
                }
            }
            let version = match &new.thread_id {
                Some(id) => thread_version(&tx, id)?,
                None => None,
            };
            let id = new_id();
            tx.execute(
                "INSERT INTO proposals (id, idempotency_key, principal_id, action, payload, source_thread_id, \
                 source_thread_version, state, created_at, updated_at, expires_at, project, origin_machine) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,?8,?9,?10,?11)",
                params![
                    id,
                    new.idempotency_key,
                    new.principal_id,
                    action.as_str(),
                    serde_json::to_string(&new.payload)?,
                    new.thread_id,
                    version.map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
                    new.now,
                    new.now + new.ttl_secs,
                    new.project,
                    new.origin_machine,
                ],
            )?;
            let created = load(&tx, &id)?;
            tx.commit()?;
            Ok((created, true, superseded))
        })
    }

    /// # Errors
    /// [`StoreError::NotFound`] when absent.
    pub fn get_proposal(&self, id: &str) -> StoreResult<Proposal> {
        let id = id.to_owned();
        self.read(move |conn| {
            let mut p = load(conn, &id)?;
            apply_live_stale(conn, &mut p)?;
            Ok(p)
        })
    }

    /// Owner-scoped lookup: a foreign proposal looks exactly like a missing one.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] when absent or owned by someone else.
    pub fn get_proposal_for(&self, principal_id: &str, id: &str) -> StoreResult<Proposal> {
        let p = self.get_proposal(id)?;
        if p.principal_id == principal_id {
            Ok(p)
        } else {
            Err(StoreError::NotFound(format!("proposal {id}")))
        }
    }

    /// # Errors
    /// Database errors.
    pub fn list_proposals(&self, states: &[ProposalState]) -> StoreResult<Vec<Proposal>> {
        let states = states.to_vec();
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM proposals ORDER BY created_at, id"
            ))?;
            let all: Vec<Proposal> = stmt
                .query_map([], proposal_from_row)?
                .collect::<Result<_, _>>()?;
            let mut out = Vec::new();
            for mut p in all {
                if states.is_empty() || states.contains(&p.state) {
                    apply_live_stale(conn, &mut p)?;
                    out.push(p);
                }
            }
            Ok(out)
        })
    }

    /// Human approval. Archive/snooze payloads apply their triage change in the same
    /// transaction; drafts stay `approved` until a separate send.
    ///
    /// # Errors
    /// [`StoreError::StaleNeedsAcknowledgement`] (the stale flag is still persisted) or
    /// [`StoreError::IllegalTransition`].
    pub fn approve_proposal(
        &self,
        id: &str,
        acknowledge_stale: bool,
        now: i64,
    ) -> StoreResult<ApproveOutcome> {
        let id = id.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            transition(p.state, ProposalState::Approved).map_err(|e| {
                StoreError::IllegalTransition(format!("{} -> {}", e.from.as_str(), e.to.as_str()))
            })?;
            let stale = refresh_stale(&tx, &p)?;
            if stale && !acknowledge_stale {
                tx.commit()?;
                return Err(StoreError::StaleNeedsAcknowledgement);
            }
            if let Some(thread_id) = &p.source_thread_id {
                match &p.payload {
                    Payload::Archive => apply_triage(
                        &tx,
                        thread_id,
                        ObjectKind::Thread,
                        &TriageUpdate {
                            archived: Some(true),
                            ..Default::default()
                        },
                        now,
                    )?,
                    Payload::Snooze { until } => apply_triage(
                        &tx,
                        thread_id,
                        ObjectKind::Thread,
                        &TriageUpdate {
                            snoozed_until: Some(Some(*until)),
                            ..Default::default()
                        },
                        now,
                    )?,
                    Payload::Draft { .. }
                    | Payload::ReminderCreate { .. }
                    | Payload::ReminderComplete { .. }
                    | Payload::CalendarCreate { .. }
                    | Payload::CalendarUpdate { .. }
                    | Payload::NoteAppend { .. } => {}
                }
            }
            let updated = set_state(&tx, &p, ProposalState::Approved, None, now)?;
            tx.commit()?;
            Ok(ApproveOutcome { proposal: updated })
        })
    }

    /// Close an approved `NoteAppend` proposal after the daemon ran (or refused) its write:
    /// `Ok(bytes)` marks it `sent`, `Err(reason)` closes it as `rejected` with the refusal reason
    /// as its result, so the agent is told and nothing is written. Anything that is not an
    /// approved note proposal is an illegal transition (so a second approval cannot repeat it).
    ///
    /// # Errors
    /// [`StoreError::IllegalTransition`].
    pub fn finish_note_proposal(
        &self,
        id: &str,
        outcome: Result<u64, String>,
        now: i64,
    ) -> StoreResult<Proposal> {
        let id = id.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            if !p.payload.is_note() || p.state != ProposalState::Approved {
                return Err(StoreError::IllegalTransition(format!(
                    "{} is not an approved note proposal",
                    p.id
                )));
            }
            let (state, result) = match outcome {
                Ok(bytes) => (ProposalState::Sent, format!("written bytes={bytes}")),
                Err(reason) => (ProposalState::Rejected, reason),
            };
            tx.execute(
                "UPDATE proposals SET state = ?2, updated_at = ?3, result = ?4 WHERE id = ?1",
                params![p.id, state.as_str(), now, result],
            )?;
            let updated = load(&tx, &id)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// # Errors
    /// [`StoreError::IllegalTransition`] unless the proposal is `pending`.
    pub fn reject_proposal(
        &self,
        id: &str,
        reason: Option<&str>,
        now: i64,
    ) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let reason = reason.map(str::to_owned);
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let updated = set_state(
                &tx,
                &p,
                ProposalState::Rejected,
                Some(reason.as_deref().unwrap_or("rejected")),
                now,
            )?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// Begin a human send, in one transaction: the proposal must be an approved mail draft, the
    /// stale flag must be acknowledged when set, and fewer than [`MAX_SENDS_PER_HOUR`] sends may
    /// have started in the last [`SEND_WINDOW_SECS`] (`executing` sends count, so a crash cannot
    /// reset the limit). The resolved recipients are stored once and never rewritten, then the
    /// proposal moves to `executing`.
    ///
    /// # Errors
    /// [`StoreError::IllegalTransition`] unless an approved draft; stale errors as for approval;
    /// [`StoreError::Validation`] with `rate_limited` over the limit.
    pub fn begin_send(
        &self,
        id: &str,
        acknowledge_stale: bool,
        now: i64,
        recipients: &[String],
    ) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let recipients_json = serde_json::to_string(recipients)?;
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            if !matches!(p.payload, Payload::Draft { .. }) {
                return Err(StoreError::IllegalTransition(
                    "only mail drafts can be sent".into(),
                ));
            }
            transition(p.state, ProposalState::Executing).map_err(|e| {
                StoreError::IllegalTransition(format!("{} -> {}", e.from.as_str(), e.to.as_str()))
            })?;
            let stale = refresh_stale(&tx, &p)?;
            if stale && !acknowledge_stale {
                tx.commit()?;
                return Err(StoreError::StaleNeedsAcknowledgement);
            }
            let recent: i64 = tx.query_row(
                "SELECT COUNT(*) FROM proposals WHERE send_started_at IS NOT NULL AND send_started_at > ?1",
                [now - SEND_WINDOW_SECS],
                |r| r.get(0),
            )?;
            if recent >= i64::from(MAX_SENDS_PER_HOUR) {
                return Err(StoreError::Validation("rate_limited".into()));
            }
            tx.execute(
                "UPDATE proposals SET send_recipients = ?2, send_started_at = ?3, send_outcome = NULL WHERE id = ?1",
                params![id, recipients_json, now],
            )?;
            let started = set_state(&tx, &p, ProposalState::Executing, None, now)?;
            tx.commit()?;
            Ok(started)
        })
    }

    /// How many sends began in the last [`SEND_WINDOW_SECS`] (the number the hourly limit uses).
    ///
    /// # Errors
    /// Database errors.
    pub fn sends_in_window(&self, now: i64) -> StoreResult<u32> {
        self.read(move |conn| {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM proposals WHERE send_started_at IS NOT NULL AND send_started_at > ?1",
                [now - SEND_WINDOW_SECS],
                |r| r.get(0),
            )?;
            Ok(u32::try_from(n).unwrap_or(u32::MAX))
        })
    }

    /// The recipients frozen by [`Store::begin_send`] (empty before a send begins).
    ///
    /// # Errors
    /// [`StoreError::NotFound`] for an unknown proposal.
    pub fn send_recipients(&self, id: &str) -> StoreResult<Vec<String>> {
        let id = id.to_owned();
        self.read(move |conn| {
            let json: Option<Option<String>> = conn
                .query_row(
                    "SELECT send_recipients FROM proposals WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(json) = json else {
                return Err(StoreError::NotFound(format!("proposal {id}")));
            };
            Ok(json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default())
        })
    }

    /// The send succeeded: `executing -> sent`.
    ///
    /// # Errors
    /// [`StoreError::IllegalTransition`] unless `executing`.
    pub fn finish_send(&self, id: &str, result: &str, now: i64) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let result = result.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let updated = set_state(&tx, &p, ProposalState::Sent, Some(&result), now)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// Nothing was delivered and nothing can have been: `executing -> approved`, the reason
    /// recorded, and the attempt no longer counts toward the hourly limit.
    ///
    /// # Errors
    /// [`StoreError::IllegalTransition`] unless `executing`.
    pub fn fail_send_no_effect(&self, id: &str, reason: &str, now: i64) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let reason = reason.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let updated = set_state(&tx, &p, ProposalState::Approved, Some(&reason), now)?;
            tx.execute(
                "UPDATE proposals SET send_started_at = NULL, send_outcome = NULL WHERE id = ?1",
                [&id],
            )?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// The outcome is unknown: the state stays `executing` and a fixed label is recorded.
    ///
    /// # Errors
    /// [`StoreError::IllegalTransition`] unless `executing`.
    pub fn mark_send_needs_check(&self, id: &str, label: &str, now: i64) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let label = label.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            if p.state != ProposalState::Executing {
                return Err(StoreError::IllegalTransition(format!(
                    "{} is not executing",
                    p.state.as_str()
                )));
            }
            tx.execute(
                "UPDATE proposals SET send_outcome = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, label, now],
            )?;
            let updated = load(&tx, &id)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// The fixed label of an ambiguous send, if one was recorded.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] for an unknown proposal.
    pub fn send_outcome(&self, id: &str) -> StoreResult<Option<String>> {
        let id = id.to_owned();
        self.read(move |conn| {
            let label: Option<Option<String>> = conn
                .query_row(
                    "SELECT send_outcome FROM proposals WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            label.ok_or_else(|| StoreError::NotFound(format!("proposal {id}")))
        })
    }

    /// Attach a result note (for example a provider draft id) without changing the state.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] when absent.
    pub fn set_proposal_result(&self, id: &str, result: &str, now: i64) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let result = result.to_owned();
        self.write(move |conn| {
            let n = conn.execute(
                "UPDATE proposals SET result = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, result, now],
            )?;
            if n == 0 {
                return Err(StoreError::NotFound(format!("proposal {id}")));
            }
            load(conn, &id)
        })
    }

    /// Expire every `pending` proposal whose TTL has elapsed.
    ///
    /// # Errors
    /// Database errors.
    pub fn expire_due(&self, now: i64) -> StoreResult<Vec<Proposal>> {
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let due: Vec<Proposal> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLUMNS} FROM proposals WHERE state = 'pending' AND expires_at <= ?1"
                ))?;
                let rows = stmt.query_map([now], proposal_from_row)?;
                rows.collect::<Result<_, _>>()?
            };
            let mut out = Vec::new();
            for p in due {
                out.push(set_state(
                    &tx,
                    &p,
                    ProposalState::Expired,
                    Some("expired"),
                    now,
                )?);
            }
            tx.commit()?;
            Ok(out)
        })
    }

    /// Proposals whose result still has to be delivered to the agent.
    ///
    /// # Errors
    /// Database errors.
    pub fn pending_deliveries(&self) -> StoreResult<Vec<(Proposal, u32)>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS}, delivery_attempts FROM proposals \
                 WHERE delivered = 0 AND state IN ('sent','rejected','expired') AND delivery_attempts < ?1 \
                 ORDER BY updated_at"
            ))?;
            let rows = stmt.query_map([MAX_DELIVERY_ATTEMPTS], |r| {
                Ok((proposal_from_row(r)?, r.get::<_, u32>(14)?))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Stop trying to deliver a proposal's result (for example the agent has no valid herdr name).
    /// The result stays available through the status request.
    ///
    /// # Errors
    /// Database errors.
    pub fn give_up_delivery(&self, id: &str) -> StoreResult<()> {
        let id = id.to_owned();
        self.write(move |conn| {
            conn.execute(
                "UPDATE proposals SET delivery_attempts = ?2 WHERE id = ?1",
                params![id, MAX_DELIVERY_ATTEMPTS],
            )?;
            Ok(())
        })
    }

    /// Count one delivery attempt; returns the attempts used so far.
    ///
    /// # Errors
    /// Database errors.
    pub fn record_delivery_attempt(&self, id: &str, delivered: bool) -> StoreResult<u32> {
        let id = id.to_owned();
        self.write(move |conn| {
            conn.execute(
                "UPDATE proposals SET delivery_attempts = delivery_attempts + 1, delivered = ?2 WHERE id = ?1",
                params![id, i64::from(delivered)],
            )?;
            Ok(conn.query_row("SELECT delivery_attempts FROM proposals WHERE id = ?1", [&id], |r| r.get(0))?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_store;
    use haste_core::model::Source;
    use haste_core::proposal::DEFAULT_TTL_SECS;

    fn draft(store: &Store, key: &str, thread: Option<&str>) -> (Proposal, bool, Vec<Proposal>) {
        store
            .create_proposal(&NewProposal {
                principal_id: "agent-a".into(),
                idempotency_key: key.into(),
                payload: Payload::Draft {
                    to: Some("ana@x".into()),
                    body: "hi".into(),
                },
                thread_id: thread.map(str::to_owned),
                now: 100,
                ttl_secs: DEFAULT_TTL_SECS,
                project: None,
                origin_machine: None,
            })
            .unwrap()
    }

    fn thread(store: &Store) -> String {
        store
            .upsert_thread(&Source::Gmail, "e1", "s", &[], 1)
            .unwrap()
            .id
    }

    #[test]
    fn idempotent_retry_returns_same_proposal() {
        let (_d, s) = test_store();
        let (a, created_a, _) = draft(&s, "k1", None);
        let (b, created_b, _) = draft(&s, "k1", None);
        assert!(created_a && !created_b);
        assert_eq!(a.id, b.id);
        assert_eq!(s.list_proposals(&[]).unwrap().len(), 1);
    }

    #[test]
    fn missing_idempotency_key_is_rejected() {
        let (_d, s) = test_store();
        let r = s.create_proposal(&NewProposal {
            principal_id: "a".into(),
            idempotency_key: "  ".into(),
            payload: Payload::Archive,
            thread_id: None,
            now: 0,
            ttl_secs: 10,
            project: None,
            origin_machine: None,
        });
        assert!(matches!(r, Err(StoreError::Validation(_))));
    }

    #[test]
    fn newer_draft_supersedes_older_for_same_thread() {
        let (_d, s) = test_store();
        let t = thread(&s);
        let (first, _, _) = draft(&s, "k1", Some(&t));
        let (second, _, superseded) = draft(&s, "k2", Some(&t));
        assert_eq!(superseded.len(), 1);
        assert_eq!(
            s.get_proposal(&first.id).unwrap().state,
            ProposalState::Superseded
        );
        assert_eq!(second.state, ProposalState::Pending);
    }

    #[test]
    fn illegal_transitions_are_refused_and_state_is_unchanged() {
        let (_d, s) = test_store();
        let (p, _, _) = draft(&s, "k1", None);
        s.reject_proposal(&p.id, Some("no"), 200).unwrap();
        assert!(matches!(
            s.begin_send(&p.id, false, 250, &recipient()),
            Err(StoreError::IllegalTransition(_))
        ));
        assert!(matches!(
            s.approve_proposal(&p.id, false, 300),
            Err(StoreError::IllegalTransition(_))
        ));
        assert_eq!(
            s.get_proposal(&p.id).unwrap().state,
            ProposalState::Rejected
        );
    }

    fn recipient() -> Vec<String> {
        vec!["ana@example.org".to_owned()]
    }

    #[test]
    fn send_requires_approval_and_always_passes_through_executing() {
        let (_d, s) = test_store();
        let (p, _, _) = draft(&s, "k1", None);
        assert!(matches!(
            s.begin_send(&p.id, false, 150, &recipient()),
            Err(StoreError::IllegalTransition(_))
        ));
        s.approve_proposal(&p.id, false, 200).unwrap();
        // approved -> sent is not a transition any more
        assert!(matches!(
            s.finish_send(&p.id, "x", 250),
            Err(StoreError::IllegalTransition(_))
        ));
        let started = s.begin_send(&p.id, false, 300, &recipient()).unwrap();
        assert_eq!(started.state, ProposalState::Executing);
        assert_eq!(s.send_recipients(&p.id).unwrap(), recipient());
        let sent = s.finish_send(&p.id, "sent:smtp", 400).unwrap();
        assert_eq!(sent.state, ProposalState::Sent);
        assert!(matches!(
            s.finish_send(&p.id, "again", 500),
            Err(StoreError::IllegalTransition(_))
        ));
    }

    #[test]
    fn a_refused_send_returns_to_approved_and_stops_counting_toward_the_limit() {
        let (_d, s) = test_store();
        let (p, _, _) = draft(&s, "k1", None);
        s.approve_proposal(&p.id, false, 200).unwrap();
        s.begin_send(&p.id, false, 300, &recipient()).unwrap();
        let back = s.fail_send_no_effect(&p.id, "send refused", 310).unwrap();
        assert_eq!(back.state, ProposalState::Approved);
        assert_eq!(back.result.as_deref(), Some("send refused"));
        // it can be tried again later
        s.begin_send(&p.id, false, 320, &recipient()).unwrap();
    }

    #[test]
    fn an_ambiguous_send_stays_executing_and_the_human_decides() {
        let (_d, s) = test_store();
        let (p, _, _) = draft(&s, "k1", None);
        s.approve_proposal(&p.id, false, 200).unwrap();
        s.begin_send(&p.id, false, 300, &recipient()).unwrap();
        let marked = s
            .mark_send_needs_check(&p.id, "outcome_unknown", 310)
            .unwrap();
        assert_eq!(marked.state, ProposalState::Executing);
        assert_eq!(
            s.send_outcome(&p.id).unwrap().as_deref(),
            Some("outcome_unknown")
        );
        assert_eq!(s.executing_proposals().unwrap().len(), 1);
        // it can never be re-begun, whatever the caller wants
        assert!(matches!(
            s.begin_send(&p.id, true, 320, &recipient()),
            Err(StoreError::IllegalTransition(_))
        ));
        let resolved = s.resolve_executing(&p.id, true, 330).unwrap();
        assert_eq!(resolved.state, ProposalState::Sent);
        assert!(matches!(
            s.mark_send_needs_check(&p.id, "x", 340),
            Err(StoreError::IllegalTransition(_))
        ));
    }

    #[test]
    fn the_hourly_limit_counts_executing_sends_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        let s = Store::open(&path).unwrap();
        let now = 10_000;
        for i in 0..MAX_SENDS_PER_HOUR {
            let (p, _, _) = draft(&s, &format!("k{i}"), None);
            s.approve_proposal(&p.id, false, now).unwrap();
            s.begin_send(&p.id, false, now + i64::from(i), &recipient())
                .unwrap();
            if i % 2 == 0 {
                s.finish_send(&p.id, "ok", now).unwrap();
            } // the others stay executing: they still count
        }
        let (extra, _, _) = draft(&s, "extra", None);
        s.approve_proposal(&extra.id, false, now).unwrap();
        for store in [&s, &Store::open(&path).unwrap()] {
            assert!(matches!(
                store.begin_send(&extra.id, false, now + 100, &recipient()),
                Err(StoreError::Validation(m)) if m == "rate_limited"
            ));
        }
        // an hour later the window has passed
        s.begin_send(&extra.id, false, now + SEND_WINDOW_SECS + 100, &recipient())
            .unwrap();
    }

    #[test]
    fn only_mail_drafts_can_be_sent_and_the_recipients_are_stored_once() {
        let (_d, s) = test_store();
        let (archive, _, _) = s
            .create_proposal(&NewProposal {
                principal_id: "agent".into(),
                idempotency_key: "arch".into(),
                payload: Payload::Archive,
                thread_id: None,
                now: 100,
                ttl_secs: 1000,
                project: None,
                origin_machine: None,
            })
            .unwrap();
        s.approve_proposal(&archive.id, false, 150).unwrap();
        assert!(matches!(
            s.begin_send(&archive.id, false, 200, &recipient()),
            Err(StoreError::IllegalTransition(_))
        ));
        let (p, _, _) = draft(&s, "k1", None);
        s.approve_proposal(&p.id, false, 200).unwrap();
        s.begin_send(&p.id, false, 300, &recipient()).unwrap();
        s.fail_send_no_effect(&p.id, "x", 310).unwrap();
        let other = vec!["other@example.org".to_owned()];
        s.begin_send(&p.id, false, 320, &other).unwrap();
        assert_eq!(
            s.send_recipients(&p.id).unwrap(),
            other,
            "frozen at each begin, not rewritten during one"
        );
    }

    #[test]
    fn stale_flag_set_on_version_bump_and_needs_acknowledgement() {
        let (_d, s) = test_store();
        let t = thread(&s);
        let (p, _, _) = draft(&s, "k1", Some(&t));
        assert!(!p.stale);
        s.upsert_thread(&Source::Gmail, "e1", "changed", &[], 5)
            .unwrap();
        assert!(matches!(
            s.approve_proposal(&p.id, false, 200),
            Err(StoreError::StaleNeedsAcknowledgement)
        ));
        let flagged = s.get_proposal(&p.id).unwrap();
        assert!(flagged.stale);
        assert_eq!(flagged.state, ProposalState::Pending);
        let ok = s.approve_proposal(&p.id, true, 210).unwrap();
        assert_eq!(ok.proposal.state, ProposalState::Approved);
    }

    #[test]
    fn listing_reports_live_stale_flag() {
        let (_d, s) = test_store();
        let t = thread(&s);
        draft(&s, "k1", Some(&t));
        assert!(!s.list_proposals(&[]).unwrap()[0].stale);
        s.upsert_thread(&Source::Gmail, "e1", "changed", &[], 5)
            .unwrap();
        assert!(s.list_proposals(&[]).unwrap()[0].stale);
    }

    #[test]
    fn unchanged_thread_is_not_stale() {
        let (_d, s) = test_store();
        let t = thread(&s);
        let (p, _, _) = draft(&s, "k1", Some(&t));
        let out = s.approve_proposal(&p.id, false, 200).unwrap();
        assert!(!out.proposal.stale);
    }

    #[test]
    fn archive_applies_only_after_approval() {
        let (_d, s) = test_store();
        let t = thread(&s);
        let (p, _, _) = s
            .create_proposal(&NewProposal {
                principal_id: "agent-a".into(),
                idempotency_key: "k".into(),
                payload: Payload::Archive,
                thread_id: Some(t.clone()),
                now: 1,
                ttl_secs: 100,
                project: None,
                origin_machine: None,
            })
            .unwrap();
        assert!(!s.get_thread(&t).unwrap().unwrap().1.archived);
        s.approve_proposal(&p.id, false, 2).unwrap();
        assert!(s.get_thread(&t).unwrap().unwrap().1.archived);
    }

    #[test]
    fn expiry_after_ttl_and_delivery_attempts_are_bounded() {
        let (_d, s) = test_store();
        let (p, _, _) = draft(&s, "k1", None);
        assert!(s.expire_due(100 + DEFAULT_TTL_SECS - 1).unwrap().is_empty());
        let expired = s.expire_due(100 + DEFAULT_TTL_SECS).unwrap();
        assert_eq!(expired.len(), 1);
        for _ in 0..MAX_DELIVERY_ATTEMPTS {
            assert_eq!(s.pending_deliveries().unwrap().len(), 1);
            s.record_delivery_attempt(&p.id, false).unwrap();
        }
        assert!(s.pending_deliveries().unwrap().is_empty());
    }

    #[test]
    fn foreign_proposals_look_missing() {
        let (_d, s) = test_store();
        let (p, _, _) = draft(&s, "k1", None);
        assert!(s.get_proposal_for("agent-a", &p.id).is_ok());
        assert!(matches!(
            s.get_proposal_for("agent-b", &p.id),
            Err(StoreError::NotFound(_))
        ));
    }
}
