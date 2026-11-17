//! Google Calendar events in the store: the synced mirror, per-calendar sweeps, project tags and
//! the finishing steps of calendar proposals. Tags live apart from the event rows, keyed by the
//! external id, so a re-sync can never drop them.

use std::collections::{BTreeSet, HashMap};

use haste_core::auth::new_id;
use haste_core::connector::CalendarSweep;
use haste_core::herdr::ProjectTag;
use haste_core::model::{Event, Identifier, IdentifierKind, LOCAL_CALENDAR};
use haste_core::proposal::{Payload, Proposal, ProposalState};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::inbox::insert_identifier;
use crate::proposals::{load, set_state};
use crate::{Store, StoreError, StoreResult};

const COLUMNS: &str = "id, external_id, calendar, title, start_at, end_at, all_day, status, \
    attendees, location, description_snippet, updated_at, missing_since, version";

/// What one upsert did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EventCounts {
    pub created: usize,
    pub updated: usize,
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    let attendees: String = row.get(8)?;
    Ok(Event {
        id: row.get(0)?,
        external_id: row.get(1)?,
        calendar: row.get(2)?,
        title: row.get(3)?,
        start_at: row.get(4)?,
        end_at: row.get(5)?,
        all_day: row.get::<_, i64>(6)? != 0,
        status: row.get(7)?,
        attendees: serde_json::from_str(&attendees).unwrap_or_default(),
        location: row.get(9)?,
        description_snippet: row.get(10)?,
        updated_at: row.get(11)?,
        missing_since: row.get(12)?,
        version: u64::try_from(row.get::<_, i64>(13)?).unwrap_or(0),
    })
}

/// True when a `CalendarUpdate` was proposed against an older version of its event.
pub(crate) fn event_stale(conn: &Connection, payload: &Payload) -> StoreResult<bool> {
    let Payload::CalendarUpdate {
        event_id,
        event_version: Some(recorded),
        ..
    } = payload
    else {
        return Ok(false);
    };
    let current: Option<i64> = conn
        .query_row(
            "SELECT version FROM events WHERE id = ?1",
            [event_id],
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
    conn.query_row("SELECT external_id FROM events WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("event {id}")))
}

/// Attendees map to people: an existing person keeps its identity, a new address gets a person
/// named after the address.
fn map_attendees(conn: &Connection, attendees: &[String]) -> StoreResult<()> {
    for email in attendees {
        let ident = Identifier {
            kind: IdentifierKind::Email,
            value: email.to_lowercase(),
        };
        let known: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM identifiers WHERE kind = ?1 AND value = ?2)",
            params![ident.kind.as_str(), ident.value],
            |r| r.get(0),
        )?;
        if !known {
            let person = new_id();
            conn.execute(
                "INSERT INTO people (id, display_name) VALUES (?1, ?2)",
                params![person, ident.value],
            )?;
            insert_identifier(conn, &person, &ident)?;
        }
    }
    Ok(())
}

fn insert_event(
    conn: &Connection,
    id: &str,
    e: &Event,
    version: i64,
    sweep: Option<&str>,
) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO events (id, external_id, calendar, title, start_at, end_at, all_day, status, \
         attendees, location, description_snippet, updated_at, missing_since, version, sweep_id) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,NULL,?13,?14)",
        params![
            id,
            e.external_id,
            e.calendar,
            e.title,
            e.start_at,
            e.end_at,
            i64::from(e.all_day),
            e.status,
            serde_json::to_string(&e.attendees)?,
            e.location,
            e.description_snippet,
            e.updated_at,
            version,
            sweep
        ],
    )?;
    Ok(())
}

impl Store {
    /// Store a batch of events. A row's version only moves when a visible field changed. Rows
    /// of a calendar that has a sweep in `sweeps` are stamped with that sweep's id.
    ///
    /// # Errors
    /// Database errors.
    pub fn upsert_events(
        &self,
        events: &[Event],
        sweeps: &[CalendarSweep],
    ) -> StoreResult<EventCounts> {
        let events = events.to_vec();
        let sweeps = sweeps.to_vec();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let mut counts = EventCounts::default();
            for e in &events {
                let sweep = sweeps
                    .iter()
                    .find(|s| s.calendar == e.calendar)
                    .map(|s| s.id.as_str());
                map_attendees(&tx, &e.attendees)?;
                let existing: Option<Event> = tx
                    .query_row(
                        &format!("SELECT {COLUMNS} FROM events WHERE external_id = ?1"),
                        [&e.external_id],
                        from_row,
                    )
                    .optional()?;
                match existing {
                    None => {
                        insert_event(&tx, &new_id(), e, 1, sweep)?;
                        counts.created += 1;
                    }
                    Some(old) => {
                        let changed = old.title != e.title
                            || old.start_at != e.start_at
                            || old.end_at != e.end_at
                            || old.all_day != e.all_day
                            || old.status != e.status
                            || old.attendees != e.attendees
                            || old.location != e.location
                            || old.description_snippet != e.description_snippet;
                        tx.execute(
                            "UPDATE events SET calendar=?2, title=?3, start_at=?4, end_at=?5, all_day=?6, \
                             status=?7, attendees=?8, location=?9, description_snippet=?10, updated_at=?11, \
                             missing_since=NULL, version = version + ?12, sweep_id = COALESCE(?13, sweep_id) \
                             WHERE id = ?1",
                            params![
                                old.id,
                                e.calendar,
                                e.title,
                                e.start_at,
                                e.end_at,
                                i64::from(e.all_day),
                                e.status,
                                serde_json::to_string(&e.attendees)?,
                                e.location,
                                e.description_snippet,
                                e.updated_at,
                                i64::from(changed),
                                sweep
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

    /// Close a calendar's complete listing: its events not stamped with `sweep_id` are marked
    /// missing (never deleted). Other calendars are untouched. Returns the rows newly marked.
    ///
    /// # Errors
    /// Database errors.
    pub fn finish_calendar_sweep(
        &self,
        calendar: &str,
        sweep_id: &str,
        now: i64,
    ) -> StoreResult<usize> {
        let (calendar, id) = (calendar.to_owned(), sweep_id.to_owned());
        self.write(move |conn| {
            Ok(conn.execute(
                "UPDATE events SET missing_since = ?3 WHERE calendar = ?1 AND missing_since IS NULL \
                 AND (sweep_id IS NULL OR sweep_id != ?2)",
                params![calendar, id, now],
            )?)
        })
    }

    /// Events not cancelled or missing, soonest first, from `from` on.
    ///
    /// # Errors
    /// Database errors.
    pub fn list_events(&self, from: i64) -> StoreResult<Vec<Event>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM events WHERE missing_since IS NULL AND status != 'cancelled' \
                 AND end_at >= ?1 ORDER BY start_at, title"
            ))?;
            let rows = stmt.query_map([from], from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Add an event that lives here only: calendar `local`, never sent anywhere. Returns its id.
    ///
    /// # Errors
    /// Database errors.
    pub fn create_local_event(
        &self,
        title: &str,
        start: i64,
        end: i64,
        now: i64,
    ) -> StoreResult<String> {
        let title = title.to_owned();
        self.write(move |conn| {
            let id = new_id();
            let event = Event {
                id: id.clone(),
                external_id: format!("local:{id}"),
                calendar: LOCAL_CALENDAR.to_owned(),
                title,
                start_at: start,
                end_at: end,
                all_day: false,
                status: "confirmed".into(),
                attendees: Vec::new(),
                location: String::new(),
                description_snippet: String::new(),
                updated_at: now,
                missing_since: None,
                version: 1,
            };
            insert_event(conn, &id, &event, 1, None)?;
            Ok(id)
        })
    }

    /// Move or resize an event here only (`None` puts it back as Google has it). The provider's
    /// times are never changed.
    ///
    /// # Errors
    /// Database errors.
    pub fn set_event_override(
        &self,
        external_id: &str,
        times: Option<(i64, i64)>,
    ) -> StoreResult<()> {
        let external_id = external_id.to_owned();
        self.write(move |conn| {
            match times {
                Some((start, end)) => conn.execute(
                    "INSERT INTO event_overrides (external_id, start_at, end_at) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(external_id) DO UPDATE SET start_at = ?2, end_at = ?3",
                    rusqlite::params![external_id, start, end],
                )?,
                None => conn.execute(
                    "DELETE FROM event_overrides WHERE external_id = ?1",
                    [&external_id],
                )?,
            };
            Ok(())
        })
    }

    /// The local times of moved events, by the provider's event id: `(start, end)`.
    ///
    /// # Errors
    /// Database errors.
    pub fn event_overrides(&self) -> StoreResult<std::collections::HashMap<String, (i64, i64)>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare("SELECT external_id, start_at, end_at FROM event_overrides")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
                ))
            })?;
            Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
        })
    }

    /// # Errors
    /// Database errors.
    pub fn get_event(&self, id: &str) -> StoreResult<Option<Event>> {
        let id = id.to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM events WHERE id = ?1"),
                    [&id],
                    from_row,
                )
                .optional()?)
        })
    }

    /// Tags of every tagged event, keyed by the haste event id.
    ///
    /// # Errors
    /// Database errors.
    pub fn all_event_tags(&self) -> StoreResult<HashMap<String, Vec<String>>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT e.id, t.tag FROM event_scope_tags t \
                 JOIN events e ON e.external_id = t.external_id ORDER BY t.tag",
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

    /// Tags of one event, sorted.
    ///
    /// # Errors
    /// Database errors.
    pub fn event_tags(&self, id: &str) -> StoreResult<Vec<String>> {
        let id = id.to_owned();
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT tag FROM event_scope_tags t JOIN events e ON e.external_id = t.external_id \
                 WHERE e.id = ?1 ORDER BY tag",
            )?;
            let rows = stmt.query_map([&id], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Add a known project tag to an event. `true` if added.
    ///
    /// # Errors
    /// Validation for an invalid or unknown tag, `NotFound` for a missing event.
    pub fn add_event_tag(&self, id: &str, tag: &str) -> StoreResult<bool> {
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
                "INSERT OR IGNORE INTO event_scope_tags (external_id, tag) VALUES (?1, ?2)",
                params![ext, tag],
            )? > 0)
        })
    }

    /// Remove a tag from an event. `true` if it was present.
    ///
    /// # Errors
    /// `NotFound` for a missing event.
    pub fn remove_event_tag(&self, id: &str, tag: &str) -> StoreResult<bool> {
        let tag = tag.trim().to_owned();
        let id = id.to_owned();
        self.write(move |conn| {
            let ext = external_of(conn, &id)?;
            Ok(conn.execute(
                "DELETE FROM event_scope_tags WHERE external_id = ?1 AND tag = ?2",
                params![ext, tag],
            )? > 0)
        })
    }

    /// The create succeeded: store the event, tag it with the proposal's project when that is a
    /// known label, and mark the proposal sent with the Google event id as its result, all in
    /// one transaction. Returns the stored event id.
    ///
    /// # Errors
    /// `IllegalTransition` unless executing and a `CalendarCreate`.
    pub fn finish_event_create(
        &self,
        id: &str,
        created: &Event,
        now: i64,
    ) -> StoreResult<(Proposal, String)> {
        let id = id.to_owned();
        let created = created.clone();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let Payload::CalendarCreate { project, .. } = &p.payload else {
                return Err(StoreError::IllegalTransition(
                    "not a calendar-create proposal".into(),
                ));
            };
            if p.state != ProposalState::Executing {
                return Err(StoreError::IllegalTransition(format!(
                    "{} -> sent",
                    p.state.as_str()
                )));
            }
            map_attendees(&tx, &created.attendees)?;
            let event_id = tx
                .query_row(
                    "SELECT id FROM events WHERE external_id = ?1",
                    [&created.external_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            let event_id = match event_id {
                Some(existing) => existing,
                None => {
                    let fresh = new_id();
                    insert_event(&tx, &fresh, &created, 1, None)?;
                    fresh
                }
            };
            if let Some(tag) = project {
                if known_tags(&tx)?.contains(tag) {
                    tx.execute(
                        "INSERT OR IGNORE INTO event_scope_tags (external_id, tag) VALUES (?1, ?2)",
                        params![created.external_id, tag],
                    )?;
                }
            }
            let result = format!("event:{}", created.external_id);
            let updated = set_state(&tx, &p, ProposalState::Sent, Some(&result), now)?;
            tx.commit()?;
            Ok((updated, event_id))
        })
    }

    /// The update succeeded: refresh the mirrored row from the provider's answer (its version
    /// moves) and mark the proposal sent, in one transaction.
    ///
    /// # Errors
    /// `IllegalTransition` unless executing and a `CalendarUpdate`.
    pub fn finish_event_update(
        &self,
        id: &str,
        updated_event: &Event,
        now: i64,
    ) -> StoreResult<Proposal> {
        let id = id.to_owned();
        let updated_event = updated_event.clone();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let p = load(&tx, &id)?;
            let Payload::CalendarUpdate { event_id, .. } = &p.payload else {
                return Err(StoreError::IllegalTransition(
                    "not a calendar-update proposal".into(),
                ));
            };
            if p.state != ProposalState::Executing {
                return Err(StoreError::IllegalTransition(format!(
                    "{} -> sent",
                    p.state.as_str()
                )));
            }
            tx.execute(
                "UPDATE events SET title=?2, start_at=?3, end_at=?4, all_day=?5, location=?6, \
                 description_snippet=?7, updated_at=?8, version = version + 1 WHERE id = ?1",
                params![
                    event_id,
                    updated_event.title,
                    updated_event.start_at,
                    updated_event.end_at,
                    i64::from(updated_event.all_day),
                    updated_event.location,
                    updated_event.description_snippet,
                    updated_event.updated_at
                ],
            )?;
            let result = format!("event:{}", updated_event.external_id);
            let done = set_state(&tx, &p, ProposalState::Sent, Some(&result), now)?;
            tx.commit()?;
            Ok(done)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_store, NewProposal};
    use haste_core::proposal::EventChanges;
    use std::collections::BTreeSet;

    fn ev(cal: &str, id: &str, title: &str) -> Event {
        Event {
            id: String::new(),
            external_id: format!("{cal}/{id}"),
            calendar: cal.into(),
            title: title.into(),
            start_at: 100,
            end_at: 200,
            all_day: false,
            status: "confirmed".into(),
            attendees: vec!["Ana@Example.org".into()],
            location: "Room 1".into(),
            description_snippet: "desc".into(),
            updated_at: 5,
            missing_since: None,
            version: 0,
        }
    }

    fn sweep(cal: &str, id: &str) -> CalendarSweep {
        CalendarSweep {
            calendar: cal.into(),
            id: id.into(),
        }
    }

    fn labels(s: &Store) {
        s.replace_machine_labels("m1", &BTreeSet::from(["haste".to_owned()]))
            .unwrap();
    }

    fn propose(s: &Store, payload: Payload) -> Proposal {
        s.create_proposal(&NewProposal {
            principal_id: "agent".into(),
            idempotency_key: new_id(),
            payload,
            thread_id: None,
            now: 1,
            ttl_secs: 1000,
            project: None,
            origin_machine: None,
        })
        .unwrap()
        .0
    }

    fn create_payload() -> Payload {
        Payload::CalendarCreate {
            calendar: "primary".into(),
            title: "Sync".into(),
            start: 100,
            end: 200,
            all_day: false,
            attendees: vec![],
            description: None,
            project: Some("haste".into()),
        }
    }

    #[test]
    fn upsert_moves_the_version_only_for_visible_changes_and_maps_attendees_once() {
        let (_d, s) = test_store();
        let first = s.upsert_events(&[ev("primary", "1", "a")], &[]).unwrap();
        assert_eq!(
            first,
            EventCounts {
                created: 1,
                updated: 0
            }
        );
        let id = s.list_events(0).unwrap()[0].id.clone();
        let mut same = ev("primary", "1", "a");
        same.updated_at = 99;
        assert_eq!(s.upsert_events(&[same], &[]).unwrap().updated, 0);
        let e = s.get_event(&id).unwrap().unwrap();
        assert_eq!((e.version, e.updated_at), (1, 99));
        assert_eq!(
            s.upsert_events(&[ev("primary", "1", "b")], &[])
                .unwrap()
                .updated,
            1
        );
        assert_eq!(s.get_event(&id).unwrap().unwrap().version, 2);
        // the attendee became one person, found case-insensitively
        let found = s
            .find_person_by_identifier(&Identifier {
                kind: IdentifierKind::Email,
                value: "ana@example.org".into(),
            })
            .unwrap();
        assert!(found.is_some());
    }

    #[test]
    fn a_calendars_sweep_marks_only_that_calendars_absent_events_missing() {
        let (_d, s) = test_store();
        s.upsert_events(
            &[ev("a", "1", "x"), ev("a", "2", "y"), ev("b", "1", "z")],
            &[sweep("a", "s1"), sweep("b", "s1")],
        )
        .unwrap();
        // calendar a is relisted without event 2; calendar b is not relisted at all
        s.upsert_events(&[ev("a", "1", "x")], &[sweep("a", "s2")])
            .unwrap();
        assert_eq!(s.finish_calendar_sweep("a", "s2", 9).unwrap(), 1);
        let titles: Vec<_> = s
            .list_events(0)
            .unwrap()
            .into_iter()
            .map(|e| e.title)
            .collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"x".to_owned()) && titles.contains(&"z".to_owned()));
    }

    #[test]
    fn cancelled_events_are_hidden_and_past_events_filtered() {
        let (_d, s) = test_store();
        let mut gone = ev("a", "2", "gone");
        gone.status = "cancelled".into();
        let mut past = ev("a", "3", "past");
        past.end_at = 50;
        s.upsert_events(&[ev("a", "1", "live"), gone, past], &[])
            .unwrap();
        let titles: Vec<_> = s
            .list_events(100)
            .unwrap()
            .into_iter()
            .map(|e| e.title)
            .collect();
        assert_eq!(titles, ["live"]);
    }

    #[test]
    fn tags_need_a_known_label_and_survive_a_resync() {
        let (_d, s) = test_store();
        labels(&s);
        s.upsert_events(&[ev("a", "1", "t")], &[]).unwrap();
        let id = s.list_events(0).unwrap()[0].id.clone();
        assert!(matches!(
            s.add_event_tag(&id, "typo"),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            s.add_event_tag("nope", "haste"),
            Err(StoreError::NotFound(_))
        ));
        assert!(s.add_event_tag(&id, "haste").unwrap());
        s.upsert_events(&[ev("a", "1", "renamed")], &[sweep("a", "s")])
            .unwrap();
        assert_eq!(s.event_tags(&id).unwrap(), ["haste"]);
        assert_eq!(s.all_event_tags().unwrap().len(), 1);
        assert!(s.remove_event_tag(&id, "haste").unwrap());
        assert!(s.all_event_tags().unwrap().is_empty());
    }

    #[test]
    fn create_runs_approved_executing_sent_and_tags_the_new_event() {
        let (_d, s) = test_store();
        labels(&s);
        let p = propose(&s, create_payload());
        assert!(
            s.begin_execution(&p.id, false, 2).is_err(),
            "pending cannot execute"
        );
        s.approve_proposal(&p.id, false, 2).unwrap();
        assert_eq!(
            s.begin_execution(&p.id, false, 3).unwrap().proposal.state,
            ProposalState::Executing
        );
        let (done, eid) = s
            .finish_event_create(&p.id, &ev("primary", "new", "Sync"), 4)
            .unwrap();
        assert_eq!(done.state, ProposalState::Sent);
        assert_eq!(done.result.as_deref(), Some("event:primary/new"));
        assert_eq!(s.event_tags(&eid).unwrap(), ["haste"]);
        assert!(
            s.finish_event_create(&p.id, &ev("primary", "new", "Sync"), 5)
                .is_err(),
            "only once"
        );
    }

    #[test]
    fn an_update_goes_stale_when_the_event_changed_and_finishing_moves_the_version() {
        let (_d, s) = test_store();
        s.upsert_events(&[ev("a", "1", "old")], &[]).unwrap();
        let e = s.list_events(0).unwrap().remove(0);
        let p = propose(
            &s,
            Payload::CalendarUpdate {
                event_id: e.id.clone(),
                changes: EventChanges {
                    title: Some("new".into()),
                    ..EventChanges::default()
                },
                project: None,
                event_version: Some(e.version),
            },
        );
        s.approve_proposal(&p.id, false, 2).unwrap();
        s.upsert_events(&[ev("a", "1", "edited elsewhere")], &[])
            .unwrap();
        assert!(matches!(
            s.begin_execution(&p.id, false, 3),
            Err(StoreError::StaleNeedsAcknowledgement)
        ));
        s.begin_execution(&p.id, true, 3).unwrap();
        let mut after = ev("a", "1", "new");
        after.updated_at = 77;
        let done = s.finish_event_update(&p.id, &after, 4).unwrap();
        assert_eq!(done.state, ProposalState::Sent);
        let now = s.get_event(&e.id).unwrap().unwrap();
        assert_eq!((now.title.as_str(), now.version), ("new", 3));
    }

    #[test]
    fn a_failed_calendar_execution_returns_to_approved() {
        let (_d, s) = test_store();
        let p = propose(&s, create_payload());
        s.approve_proposal(&p.id, false, 2).unwrap();
        s.begin_execution(&p.id, false, 3).unwrap();
        let back = s.fail_execution(&p.id, "permission needed", 4).unwrap();
        assert_eq!(back.state, ProposalState::Approved);
    }

    #[test]
    fn a_local_event_is_stored_listed_and_left_alone_by_a_google_sweep() {
        let (_d, s) = test_store();
        let id = s.create_local_event("Dentist", 2_000, 5_600, 10).unwrap();
        let listed = s.list_events(0).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].calendar, LOCAL_CALENDAR);
        assert_eq!(listed[0].external_id, format!("local:{id}"));
        assert_eq!((listed[0].start_at, listed[0].end_at), (2_000, 5_600));
        // a sweep of a Google calendar does not touch it
        let sweep = CalendarSweep {
            calendar: "primary".into(),
            id: "s1".into(),
        };
        s.upsert_events(&[], &[sweep]).unwrap();
        assert_eq!(s.list_events(0).unwrap().len(), 1);
    }
}
