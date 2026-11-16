//! Apple Reminders in the store: the synced mirror, sweeps, project tags and the execution
//! steps of reminder proposals. Tags live apart from the reminder rows so a re-sync can never
//! drop them.

use std::collections::{BTreeSet, HashMap};

use haste_core::auth::new_id;
use haste_core::connector::Sweep;
use haste_core::herdr::ProjectTag;
use haste_core::model::{ObjectKind, Reminder};
use haste_core::proposal::{transition, Payload, Proposal, ProposalState};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::proposals::{load, set_state};
use crate::{Store, StoreError, StoreResult};

const COLUMNS: &str = "id, external_id, title, notes_snippet, due_at, completed, list_name, \
    remote_modified_at, missing_since, version";

/// What one upsert did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpsertCounts {
    pub created: usize,
    pub updated: usize,
}

/// Result of starting an execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStart {
    pub proposal: Proposal,
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Reminder> {
    let u = |v: i64| u64::try_from(v).unwrap_or(0);
    Ok(Reminder {
        id: row.get(0)?,
        external_id: row.get(1)?,
        title: row.get(2)?,
        notes_snippet: row.get(3)?,
        due_at: row.get(4)?,
        completed: row.get::<_, i64>(5)? != 0,
        list_name: row.get(6)?,
        remote_modified_at: row.get(7)?,
        missing_since: row.get(8)?,
        version: u(row.get(9)?),
    })
}

/// True when a `ReminderComplete` proposal was made against an older version of its reminder.
pub(crate) fn reminder_stale(conn: &Connection, payload: &Payload) -> StoreResult<bool> {
    let Payload::ReminderComplete {
        reminder_id,
        reminder_version: Some(recorded),
        ..
    } = payload
    else {
        return Ok(false);
    };
    let current: Option<i64> = conn
        .query_row(
            "SELECT version FROM reminders WHERE id = ?1",
            [reminder_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(current.is_some_and(|v| u64::try_from(v).unwrap_or(0) != *recorded))
}

fn known_tags(conn: &Connection) -> StoreResult<BTreeSet<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT label FROM machine_labels")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn external_of(conn: &Connection, id: &str) -> StoreResult<String> {
    conn.query_row(
        "SELECT external_id FROM reminders WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("reminder {id}")))
}

impl Store {
    /// Store a batch of reminders. A row's version only moves when a visible field changed.
    /// When a sweep is given, every stored row is stamped with its id.
    ///
    /// # Errors
    /// Database errors.
    pub fn upsert_reminders(
        &self,
        reminders: &[Reminder],
        sweep: Option<&Sweep>,
    ) -> StoreResult<UpsertCounts> {
        let reminders = reminders.to_vec();
        let sweep_id = sweep.map(|s| s.id.clone());
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let mut counts = UpsertCounts::default();
            for r in &reminders {
                let existing: Option<Reminder> = tx
                    .query_row(
                        &format!("SELECT {COLUMNS} FROM reminders WHERE external_id = ?1"),
                        [&r.external_id],
                        from_row,
                    )
                    .optional()?;
                match existing {
                    None => {
                        tx.execute(
                            "INSERT INTO reminders (id, external_id, title, notes_snippet, due_at, completed, \
                             list_name, remote_modified_at, missing_since, version, sweep_id) \
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,1,?9)",
                            params![
                                new_id(),
                                r.external_id,
                                r.title,
                                r.notes_snippet,
                                r.due_at,
                                i64::from(r.completed),
                                r.list_name,
                                r.remote_modified_at,
                                sweep_id
                            ],
                        )?;
                        counts.created += 1;
                    }
                    Some(old) => {
                        let changed = old.title != r.title
                            || old.notes_snippet != r.notes_snippet
                            || old.due_at != r.due_at
                            || old.completed != r.completed
                            || old.list_name != r.list_name;
                        tx.execute(
                            "UPDATE reminders SET title=?2, notes_snippet=?3, due_at=?4, completed=?5, \
                             list_name=?6, remote_modified_at=?7, missing_since=NULL, \
                             version = version + ?8, sweep_id = COALESCE(?9, sweep_id) WHERE id = ?1",
                            params![
                                old.id,
                                r.title,
                                r.notes_snippet,
                                r.due_at,
                                i64::from(r.completed),
                                r.list_name,
                                r.remote_modified_at,
                                i64::from(changed),
                                sweep_id
                            ],
                        )?;
                        if changed {
                            counts.updated += 1;
                        }
                    }
                }
            }
            tx.commit()?;
            Ok(counts)
        })
    }

    /// Close a sweep: rows not stamped with `sweep_id` are marked missing (never deleted).
    /// Returns how many rows were newly marked.
    ///
    /// # Errors
    /// Database errors.
    pub fn finish_sweep(&self, sweep_id: &str, now: i64) -> StoreResult<usize> {
        let id = sweep_id.to_owned();
        self.write(move |conn| {
            Ok(conn.execute(
                "UPDATE reminders SET missing_since = ?2 \
                 WHERE missing_since IS NULL AND (sweep_id IS NULL OR sweep_id != ?1)",
                params![id, now],
            )?)
        })
    }

    /// # Errors
    /// Database errors.
    pub fn list_reminders(&self, include_completed: bool) -> StoreResult<Vec<Reminder>> {
        self.read(|conn| {
            let sql = format!(
                "SELECT {COLUMNS} FROM reminders WHERE missing_since IS NULL {} \
                 ORDER BY due_at IS NULL, due_at, title",
                if include_completed {
                    ""
                } else {
                    "AND completed = 0"
                }
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// # Errors
    /// Database errors.
    pub fn get_reminder(&self, id: &str) -> StoreResult<Option<Reminder>> {
        let id = id.to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM reminders WHERE id = ?1"),
                    [&id],
                    from_row,
                )
                .optional()?)
        })
    }

    /// `(total, open, missing)` over every stored reminder.
    ///
    /// # Errors
    /// Database errors.
    pub fn reminder_counts(&self) -> StoreResult<(u64, u64, u64)> {
        self.read(|conn| {
            let one = |sql: &str| -> rusqlite::Result<u64> {
                conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                    .map(|n| u64::try_from(n).unwrap_or(0))
            };
            Ok((
                one("SELECT COUNT(*) FROM reminders")?,
                one(
                    "SELECT COUNT(*) FROM reminders WHERE completed = 0 AND missing_since IS NULL",
                )?,
                one("SELECT COUNT(*) FROM reminders WHERE missing_since IS NOT NULL")?,
            ))
        })
    }

    /// Tags of one reminder, sorted.
    ///
    /// # Errors
    /// Database errors.
    pub fn reminder_tags(&self, id: &str) -> StoreResult<Vec<String>> {
        let id = id.to_owned();
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT tag FROM reminder_scope_tags t JOIN reminders r ON r.external_id = t.external_id \
                 WHERE r.id = ?1 ORDER BY tag",
            )?;
            let rows = stmt.query_map([&id], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Tags of every tagged reminder, keyed by the haste reminder id.
    ///
    /// # Errors
    /// Database errors.
    pub fn all_reminder_tags(&self) -> StoreResult<HashMap<String, Vec<String>>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT r.id, t.tag FROM reminder_scope_tags t \
                 JOIN reminders r ON r.external_id = t.external_id ORDER BY t.tag",
            )?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out: HashMap<String, Vec<String>> = HashMap::new();
            for row in rows {
                let (id, tag) = row?;
                out.entry(id).or_default().push(tag);
            }
            Ok(out)
        })
    }

    /// Add a known project tag to a reminder. `true` if added.
    ///
    /// # Errors
    /// Validation for an invalid or unknown tag, `NotFound` for a missing reminder.
    pub fn add_reminder_tag(&self, id: &str, tag: &str) -> StoreResult<bool> {
        let tag = ProjectTag::new(tag)
            .map_err(|e| StoreError::Validation(e.0.into()))?
            .as_str()
            .to_owned();
        let id = id.to_owned();
        self.write(move |conn| {
            if !known_tags(conn)?.contains(&tag) {
                return Err(StoreError::Validation(format!(
                    "'{tag}' is not a known project (workspace) label"
                )));
            }
            let ext = external_of(conn, &id)?;
            Ok(conn.execute(
                "INSERT OR IGNORE INTO reminder_scope_tags (external_id, tag) VALUES (?1, ?2)",
                params![ext, tag],
            )? > 0)
        })
    }

    /// Remove a tag from a reminder. `true` if it was present.
    ///
    /// # Errors
    /// `NotFound` for a missing reminder.
    pub fn remove_reminder_tag(&self, id: &str, tag: &str) -> StoreResult<bool> {
        let tag = tag.trim().to_owned();
        let id = id.to_owned();
        self.write(move |conn| {
            let ext = external_of(conn, &id)?;
            Ok(conn.execute(
                "DELETE FROM reminder_scope_tags WHERE external_id = ?1 AND tag = ?2",
                params![ext, tag],
            )? > 0)
        })
    }

    /// Human starts executing an approved reminder proposal: `approved -> executing`.
    /// A stale proposal needs the acknowledgement. Nothing else is touched.
    ///
    /// # Errors
    /// `IllegalTransition` unless approved and a reminder payload; stale needs acknowledgement.
    pub fn begin_execution(
        &self,
        id: &str,
        acknowledge_stale: bool,
        now: i64,
    ) -> StoreResult<ExecutionStart> {
        let id = id.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            if !p.payload.is_reminder() && !p.payload.is_calendar() {
                return Err(StoreError::IllegalTransition(
                    "only reminder and calendar proposals are executed".into(),
                ));
            }
            transition(p.state, ProposalState::Executing).map_err(|e| {
                StoreError::IllegalTransition(format!("{} -> {}", e.from.as_str(), e.to.as_str()))
            })?;
            let stale = crate::proposals::refresh_stale(&tx, &p)?;
            if stale && !acknowledge_stale {
                tx.commit()?;
                return Err(StoreError::StaleNeedsAcknowledgement);
            }
            let proposal = set_state(&tx, &p, ProposalState::Executing, None, now)?;
            tx.commit()?;
            Ok(ExecutionStart { proposal })
        })
    }

    /// The write failed before any external effect: `executing -> approved`, reason recorded.
    ///
    /// # Errors
    /// `IllegalTransition` unless executing.
    pub fn fail_execution(&self, id: &str, reason: &str, now: i64) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let reason = reason.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let updated = set_state(&tx, &p, ProposalState::Approved, Some(&reason), now)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// The create succeeded: store the reminder, tag it with the proposal's project when that
    /// is a known label, link it to the source thread (`about`), and mark the proposal sent,
    /// all in one transaction. Returns the stored reminder id.
    ///
    /// # Errors
    /// `IllegalTransition` unless executing.
    pub fn finish_create(
        &self,
        id: &str,
        created: &Reminder,
        now: i64,
    ) -> StoreResult<(Proposal, String)> {
        let id = id.to_owned();
        let created = created.clone();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let Payload::ReminderCreate {
                project, thread_id, ..
            } = &p.payload
            else {
                return Err(StoreError::IllegalTransition(
                    "not a reminder-create proposal".into(),
                ));
            };
            if p.state != ProposalState::Executing {
                return Err(StoreError::IllegalTransition(format!(
                    "{} -> sent",
                    p.state.as_str()
                )));
            }
            let reminder_id = tx
                .query_row(
                    "SELECT id FROM reminders WHERE external_id = ?1",
                    [&created.external_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or_else(new_id);
            tx.execute(
                "INSERT INTO reminders (id, external_id, title, notes_snippet, due_at, completed, \
                 list_name, remote_modified_at, missing_since, version, sweep_id) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,1,NULL) \
                 ON CONFLICT(external_id) DO NOTHING",
                params![
                    reminder_id,
                    created.external_id,
                    created.title,
                    created.notes_snippet,
                    created.due_at,
                    i64::from(created.completed),
                    created.list_name,
                    created.remote_modified_at
                ],
            )?;
            if let Some(tag) = project {
                if known_tags(&tx)?.contains(tag) {
                    tx.execute(
                        "INSERT OR IGNORE INTO reminder_scope_tags (external_id, tag) VALUES (?1, ?2)",
                        params![created.external_id, tag],
                    )?;
                }
            }
            if let Some(thread) = thread_id {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?1)",
                    [thread],
                    |r| r.get(0),
                )?;
                if exists {
                    tx.execute(
                        "INSERT OR IGNORE INTO links (from_kind, from_id, to_kind, to_id, link_type) \
                         VALUES (?1,?2,?3,?4,'about')",
                        params![
                            ObjectKind::Reminder.as_str(),
                            reminder_id,
                            ObjectKind::Thread.as_str(),
                            thread
                        ],
                    )?;
                }
            }
            let updated = set_state(&tx, &p, ProposalState::Sent, Some("reminder created"), now)?;
            tx.commit()?;
            Ok((updated, reminder_id))
        })
    }

    /// Remember how reminders repeat (`None` forgets it).
    ///
    /// # Errors
    /// Database errors.
    pub fn set_reminder_repeats(
        &self,
        repeats: &[(String, Option<haste_core::model::RepeatRule>)],
    ) -> StoreResult<()> {
        let repeats = repeats.to_vec();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            for (external, rule) in &repeats {
                match rule {
                    Some(r) => tx.execute(
                        "INSERT INTO reminder_repeats (external_id, freq, every) VALUES (?1, ?2, ?3) \
                         ON CONFLICT(external_id) DO UPDATE SET freq = ?2, every = ?3",
                        params![external, r.freq.as_str(), r.every],
                    )?,
                    None => tx.execute(
                        "DELETE FROM reminder_repeats WHERE external_id = ?1",
                        [external],
                    )?,
                };
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Repeat rules by the Reminders item's external id.
    ///
    /// # Errors
    /// Database errors.
    pub fn reminder_repeats(
        &self,
    ) -> StoreResult<std::collections::HashMap<String, haste_core::model::RepeatRule>> {
        self.read(|conn| {
            let mut stmt = conn.prepare("SELECT external_id, freq, every FROM reminder_repeats")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })?;
            let mut out = std::collections::HashMap::new();
            for row in rows {
                let (external, freq, every) = row?;
                if let Some(freq) = haste_core::model::RepeatFreq::parse(&freq) {
                    out.insert(external, haste_core::model::RepeatRule { freq, every });
                }
            }
            Ok(out)
        })
    }

    /// Store a reminder the human just created, with its project tags, the thread it is about
    /// and how it repeats, so it shows at once; the next sync fills in the rest. Returns its id.
    ///
    /// # Errors
    /// Database errors.
    pub fn record_created_reminder(
        &self,
        created: &Reminder,
        tags: &[String],
        thread_id: Option<&str>,
        repeat: Option<haste_core::model::RepeatRule>,
    ) -> StoreResult<String> {
        let created = created.clone();
        let tags = tags.to_vec();
        let thread_id = thread_id.map(str::to_owned);
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let reminder_id = tx
                .query_row(
                    "SELECT id FROM reminders WHERE external_id = ?1",
                    [&created.external_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or_else(new_id);
            tx.execute(
                "INSERT INTO reminders (id, external_id, title, notes_snippet, due_at, completed, \
                 list_name, remote_modified_at, missing_since, version, sweep_id) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,1,NULL) \
                 ON CONFLICT(external_id) DO NOTHING",
                params![
                    reminder_id,
                    created.external_id,
                    created.title,
                    created.notes_snippet,
                    created.due_at,
                    i64::from(created.completed),
                    created.list_name,
                    created.remote_modified_at
                ],
            )?;
            let known = known_tags(&tx)?;
            for tag in tags.iter().filter(|t| known.contains(*t)) {
                tx.execute(
                    "INSERT OR IGNORE INTO reminder_scope_tags (external_id, tag) VALUES (?1, ?2)",
                    params![created.external_id, tag],
                )?;
            }
            if let Some(thread) = &thread_id {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?1)",
                    [thread],
                    |r| r.get(0),
                )?;
                if exists {
                    tx.execute(
                        "INSERT OR IGNORE INTO links (from_kind, from_id, to_kind, to_id, link_type) \
                         VALUES (?1,?2,?3,?4,'about')",
                        params![
                            ObjectKind::Reminder.as_str(),
                            reminder_id,
                            ObjectKind::Thread.as_str(),
                            thread
                        ],
                    )?;
                }
            }
            if let Some(r) = repeat {
                tx.execute(
                    "INSERT OR REPLACE INTO reminder_repeats (external_id, freq, every) VALUES (?1,?2,?3)",
                    params![created.external_id, r.freq.as_str(), r.every],
                )?;
            }
            tx.commit()?;
            Ok(reminder_id)
        })
    }

    /// The human's edit went through at the Reminders app: show it here at once (the next sync
    /// finds the same values and changes nothing). The notes are only ever appended, and only
    /// their short preview is kept here, so they are left to the sync.
    ///
    /// # Errors
    /// Database errors.
    pub fn apply_reminder_edit(&self, id: &str, title: &str, due: Option<i64>) -> StoreResult<()> {
        let (id, title) = (id.to_owned(), title.to_owned());
        self.write(move |conn| {
            conn.execute(
                "UPDATE reminders SET title = ?2, due_at = ?3 WHERE id = ?1",
                params![id, title, due],
            )?;
            Ok(())
        })
    }

    /// How long a task takes on the calendar, here only (`None` goes back to the default).
    ///
    /// # Errors
    /// Database errors.
    pub fn set_reminder_duration(
        &self,
        external_id: &str,
        minutes: Option<u32>,
    ) -> StoreResult<()> {
        let external_id = external_id.to_owned();
        self.write(move |conn| {
            match minutes {
                Some(m) => conn.execute(
                    "INSERT INTO reminder_durations (external_id, minutes) VALUES (?1, ?2) \
                     ON CONFLICT(external_id) DO UPDATE SET minutes = ?2",
                    params![external_id, m],
                )?,
                None => conn.execute(
                    "DELETE FROM reminder_durations WHERE external_id = ?1",
                    [&external_id],
                )?,
            };
            Ok(())
        })
    }

    /// The set durations (minutes) by the Reminders item's external id.
    ///
    /// # Errors
    /// Database errors.
    pub fn reminder_durations(&self) -> StoreResult<std::collections::HashMap<String, u32>> {
        self.read(|conn| {
            let mut stmt = conn.prepare("SELECT external_id, minutes FROM reminder_durations")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
            Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
        })
    }

    /// How important a task is, here only (`None` goes back to middle).
    ///
    /// # Errors
    /// Database errors.
    pub fn set_reminder_priority(&self, external_id: &str, rank: Option<u8>) -> StoreResult<()> {
        let external_id = external_id.to_owned();
        self.write(move |conn| {
            match rank {
                Some(r) => conn.execute(
                    "INSERT INTO reminder_priorities (external_id, priority) VALUES (?1, ?2) \
                     ON CONFLICT(external_id) DO UPDATE SET priority = ?2",
                    params![external_id, r],
                )?,
                None => conn.execute(
                    "DELETE FROM reminder_priorities WHERE external_id = ?1",
                    [&external_id],
                )?,
            };
            Ok(())
        })
    }

    /// The set priorities (ranks) by the Reminders item's external id.
    ///
    /// # Errors
    /// Database errors.
    pub fn reminder_priorities(&self) -> StoreResult<std::collections::HashMap<String, u8>> {
        self.read(|conn| {
            let mut stmt = conn.prepare("SELECT external_id, priority FROM reminder_priorities")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u8>(1)?)))?;
            Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
        })
    }

    /// The complete succeeded: mark the mirrored row done (version moves) and the proposal sent.
    ///
    /// # Errors
    /// `IllegalTransition` unless executing.
    pub fn finish_complete(&self, id: &str, now: i64) -> StoreResult<Proposal> {
        let id = id.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let Payload::ReminderComplete { reminder_id, .. } = &p.payload else {
                return Err(StoreError::IllegalTransition(
                    "not a reminder-complete proposal".into(),
                ));
            };
            if p.state != ProposalState::Executing {
                return Err(StoreError::IllegalTransition(format!(
                    "{} -> sent",
                    p.state.as_str()
                )));
            }
            tx.execute(
                "UPDATE reminders SET completed = 1, version = version + 1 \
                 WHERE id = ?1 AND completed = 0",
                [reminder_id],
            )?;
            let updated = set_state(
                &tx,
                &p,
                ProposalState::Sent,
                Some("reminder completed"),
                now,
            )?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// Proposals left `executing` (for example by a crash), oldest first.
    ///
    /// # Errors
    /// Database errors.
    pub fn executing_proposals(&self) -> StoreResult<Vec<Proposal>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {} FROM proposals WHERE state = 'executing' ORDER BY updated_at",
                crate::proposals::COLUMNS
            ))?;
            let rows = stmt.query_map([], crate::proposals::proposal_from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The human decides what happened to an interrupted execution: the effect is visible in
    /// Reminders (`done`, becomes sent) or not (back to approved so it can be run again).
    ///
    /// # Errors
    /// `IllegalTransition` unless executing.
    pub fn resolve_executing(&self, id: &str, done: bool, now: i64) -> StoreResult<Proposal> {
        let id = id.to_owned();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let (to, result) = if done {
                (ProposalState::Sent, "confirmed done by the human")
            } else {
                (ProposalState::Approved, "returned to approved by the human")
            };
            let updated = set_state(&tx, &p, to, Some(result), now)?;
            tx.commit()?;
            Ok(updated)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_store, NewProposal};
    use haste_core::model::Source;
    use std::collections::BTreeSet;

    fn rem(ext: &str, title: &str) -> Reminder {
        Reminder {
            id: String::new(),
            external_id: ext.into(),
            title: title.into(),
            notes_snippet: String::new(),
            due_at: None,
            completed: false,
            list_name: "Inbox".into(),
            remote_modified_at: 10,
            missing_since: None,
            version: 0,
        }
    }

    fn sweep(id: &str) -> Sweep {
        Sweep {
            id: id.into(),
            complete: true,
        }
    }

    fn labels(s: &Store) {
        s.replace_machine_labels("m1", &BTreeSet::from(["haste".to_owned()]))
            .unwrap();
    }

    fn propose(s: &Store, payload: Payload, thread: Option<String>) -> Proposal {
        s.create_proposal(&NewProposal {
            principal_id: "agent".into(),
            idempotency_key: new_id(),
            payload,
            thread_id: thread,
            now: 1,
            ttl_secs: 1000,
            project: None,
            origin_machine: None,
        })
        .unwrap()
        .0
    }

    fn create_payload(thread: Option<String>) -> Payload {
        Payload::ReminderCreate {
            title: "call".into(),
            due: None,
            list: None,
            project: Some("haste".into()),
            thread_id: thread,
        }
    }

    #[test]
    fn upsert_moves_the_version_only_when_something_visible_changed() {
        let (_d, s) = test_store();
        let first = s.upsert_reminders(&[rem("e1", "a")], None).unwrap();
        assert_eq!(
            first,
            UpsertCounts {
                created: 1,
                updated: 0
            }
        );
        let id = s.list_reminders(true).unwrap()[0].id.clone();
        assert_eq!(s.get_reminder(&id).unwrap().unwrap().version, 1);
        // same content, newer remote timestamp: no version move
        let mut same = rem("e1", "a");
        same.remote_modified_at = 99;
        assert_eq!(s.upsert_reminders(&[same], None).unwrap().updated, 0);
        let r = s.get_reminder(&id).unwrap().unwrap();
        assert_eq!((r.version, r.remote_modified_at), (1, 99));
        assert_eq!(
            s.upsert_reminders(&[rem("e1", "b")], None).unwrap().updated,
            1
        );
        assert_eq!(s.get_reminder(&id).unwrap().unwrap().version, 2);
    }

    #[test]
    fn a_complete_sweep_marks_absent_rows_missing_and_a_return_clears_it() {
        let (_d, s) = test_store();
        s.upsert_reminders(&[rem("e1", "a"), rem("e2", "b")], Some(&sweep("s1")))
            .unwrap();
        assert_eq!(s.finish_sweep("s1", 5).unwrap(), 0);
        s.upsert_reminders(&[rem("e1", "a")], Some(&sweep("s2")))
            .unwrap();
        assert_eq!(s.finish_sweep("s2", 6).unwrap(), 1);
        let listed = s.list_reminders(true).unwrap();
        assert_eq!(listed.len(), 1, "missing rows are hidden, not deleted");
        s.upsert_reminders(&[rem("e2", "b")], Some(&sweep("s3")))
            .unwrap();
        s.finish_sweep("s3", 7).unwrap();
        let ids: Vec<_> = s
            .list_reminders(true)
            .unwrap()
            .into_iter()
            .map(|r| r.external_id)
            .collect();
        assert_eq!(ids, ["e2"]);
    }

    #[test]
    fn completed_reminders_are_hidden_unless_asked_for() {
        let (_d, s) = test_store();
        let mut done = rem("e2", "done");
        done.completed = true;
        s.upsert_reminders(&[rem("e1", "open"), done], None)
            .unwrap();
        assert_eq!(s.list_reminders(false).unwrap().len(), 1);
        assert_eq!(s.list_reminders(true).unwrap().len(), 2);
    }

    #[test]
    fn tags_need_a_known_label_and_survive_a_resync() {
        let (_d, s) = test_store();
        labels(&s);
        s.upsert_reminders(&[rem("e1", "a")], None).unwrap();
        let id = s.list_reminders(true).unwrap()[0].id.clone();
        assert!(matches!(
            s.add_reminder_tag(&id, "typo"),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            s.add_reminder_tag("nope", "haste"),
            Err(StoreError::NotFound(_))
        ));
        assert!(s.add_reminder_tag(&id, "haste").unwrap());
        assert!(!s.add_reminder_tag(&id, "haste").unwrap());
        s.upsert_reminders(&[rem("e1", "renamed")], Some(&sweep("s")))
            .unwrap();
        assert_eq!(s.reminder_tags(&id).unwrap(), ["haste"]);
        assert_eq!(s.all_reminder_tags().unwrap().len(), 1);
        assert!(s.remove_reminder_tag(&id, "haste").unwrap());
        assert!(s.all_reminder_tags().unwrap().is_empty());
    }

    #[test]
    fn create_runs_approved_executing_sent_and_links_the_thread() {
        let (_d, s) = test_store();
        labels(&s);
        let t = s
            .upsert_thread(&Source::Gmail, "g1", "mail", &[], 1)
            .unwrap()
            .id;
        let p = propose(&s, create_payload(Some(t.clone())), Some(t.clone()));
        assert!(
            s.begin_execution(&p.id, false, 2).is_err(),
            "pending cannot execute"
        );
        s.approve_proposal(&p.id, false, 2).unwrap();
        let started = s.begin_execution(&p.id, false, 3).unwrap();
        assert_eq!(started.proposal.state, ProposalState::Executing);
        let (done, rid) = s.finish_create(&p.id, &rem("ek", "call"), 4).unwrap();
        assert_eq!(done.state, ProposalState::Sent);
        assert_eq!(s.reminder_tags(&rid).unwrap(), ["haste"]);
        assert_eq!(s.count_links().unwrap(), 1);
        assert!(
            s.finish_create(&p.id, &rem("ek", "call"), 5).is_err(),
            "only once"
        );
    }

    #[test]
    fn a_failed_execution_returns_to_approved_with_the_reason() {
        let (_d, s) = test_store();
        let p = propose(&s, create_payload(None), None);
        s.approve_proposal(&p.id, false, 2).unwrap();
        s.begin_execution(&p.id, false, 3).unwrap();
        let back = s.fail_execution(&p.id, "permission needed", 4).unwrap();
        assert_eq!(back.state, ProposalState::Approved);
        assert_eq!(back.result.as_deref(), Some("permission needed"));
    }

    #[test]
    fn only_reminder_proposals_can_be_executed() {
        let (_d, s) = test_store();
        let p = propose(&s, Payload::Archive, None);
        s.approve_proposal(&p.id, false, 2).unwrap();
        assert!(matches!(
            s.begin_execution(&p.id, false, 3),
            Err(StoreError::IllegalTransition(_))
        ));
    }

    #[test]
    fn a_complete_proposal_goes_stale_when_the_reminder_changed() {
        let (_d, s) = test_store();
        s.upsert_reminders(&[rem("e1", "a")], None).unwrap();
        let r = s.list_reminders(true).unwrap().remove(0);
        let p = propose(
            &s,
            Payload::ReminderComplete {
                reminder_id: r.id.clone(),
                project: None,
                reminder_version: Some(r.version),
            },
            None,
        );
        s.approve_proposal(&p.id, false, 2).unwrap();
        s.upsert_reminders(&[rem("e1", "edited elsewhere")], None)
            .unwrap();
        assert!(matches!(
            s.begin_execution(&p.id, false, 3),
            Err(StoreError::StaleNeedsAcknowledgement)
        ));
        s.begin_execution(&p.id, true, 3).unwrap();
        let done = s.finish_complete(&p.id, 4).unwrap();
        assert_eq!(done.state, ProposalState::Sent);
        assert!(s.get_reminder(&r.id).unwrap().unwrap().completed);
    }

    #[test]
    fn an_interrupted_execution_is_listed_and_resolved_by_the_human() {
        let (_d, s) = test_store();
        let p = propose(&s, create_payload(None), None);
        s.approve_proposal(&p.id, false, 2).unwrap();
        s.begin_execution(&p.id, false, 3).unwrap();
        assert_eq!(s.executing_proposals().unwrap().len(), 1);
        assert_eq!(
            s.resolve_executing(&p.id, false, 4).unwrap().state,
            ProposalState::Approved
        );
        s.begin_execution(&p.id, false, 5).unwrap();
        assert_eq!(
            s.resolve_executing(&p.id, true, 6).unwrap().state,
            ProposalState::Sent
        );
        assert!(s.executing_proposals().unwrap().is_empty());
    }
}
