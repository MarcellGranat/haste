//! Threads, messages, people, triage and links.

use haste_core::auth::new_id;
use haste_core::model::{
    Identifier, Link, LinkType, Message, ObjectKind, Person, Source, Thread, Triage,
};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::{Store, StoreError, StoreResult};

#[derive(Debug, Clone, Default)]
pub struct ThreadFilter {
    /// Empty means every source.
    pub sources: Vec<Source>,
    pub include_archived: bool,
    pub include_snoozed: bool,
    /// Current time, used to decide whether a snooze is still active.
    pub now: i64,
}

/// Partial triage change; `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct TriageUpdate {
    pub priority: Option<i32>,
    /// `Some(None)` clears the snooze.
    pub snoozed_until: Option<Option<i64>>,
    pub set_aside: Option<bool>,
    pub archived: Option<bool>,
}

const THREAD_COLUMNS: &str = "t.id, t.source, t.external_id, t.subject, t.participants, \
    t.last_activity_at, t.version";

pub(crate) fn thread_from_row(row: &Row<'_>) -> rusqlite::Result<Thread> {
    let participants: String = row.get(4)?;
    Ok(Thread {
        id: row.get(0)?,
        source: Source::from(row.get::<_, String>(1)?),
        external_id: row.get(2)?,
        subject: row.get(3)?,
        participants: serde_json::from_str(&participants).unwrap_or_default(),
        last_activity_at: row.get(5)?,
        version: row.get::<_, i64>(6)?.try_into().unwrap_or(0),
    })
}

fn triage_from_row(row: &Row<'_>, offset: usize, id: &str) -> rusqlite::Result<Triage> {
    Ok(Triage {
        object_id: id.to_owned(),
        object_kind: ObjectKind::Thread,
        priority: row.get(offset)?,
        snoozed_until: row.get(offset + 1)?,
        set_aside: row.get::<_, i64>(offset + 2)? != 0,
        archived: row.get::<_, i64>(offset + 3)? != 0,
        updated_at: row.get(offset + 4)?,
    })
}

pub(crate) fn thread_version(conn: &Connection, id: &str) -> StoreResult<Option<u64>> {
    let v: Option<i64> = conn
        .query_row("SELECT version FROM threads WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(v.map(|v| v.try_into().unwrap_or(0)))
}

pub(crate) fn apply_triage(
    conn: &Connection,
    object_id: &str,
    kind: ObjectKind,
    update: &TriageUpdate,
    now: i64,
) -> StoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO triage (object_id, object_kind, updated_at) VALUES (?1, ?2, ?3)",
        params![object_id, kind.as_str(), now],
    )?;
    if let Some(p) = update.priority {
        conn.execute(
            "UPDATE triage SET priority = ?3, updated_at = ?4 WHERE object_id = ?1 AND object_kind = ?2",
            params![object_id, kind.as_str(), p, now],
        )?;
    }
    if let Some(s) = update.snoozed_until {
        conn.execute(
            "UPDATE triage SET snoozed_until = ?3, updated_at = ?4 WHERE object_id = ?1 AND object_kind = ?2",
            params![object_id, kind.as_str(), s, now],
        )?;
    }
    if let Some(a) = update.set_aside {
        conn.execute(
            "UPDATE triage SET set_aside = ?3, updated_at = ?4 WHERE object_id = ?1 AND object_kind = ?2",
            params![object_id, kind.as_str(), i64::from(a), now],
        )?;
    }
    if let Some(a) = update.archived {
        conn.execute(
            "UPDATE triage SET archived = ?3, updated_at = ?4 WHERE object_id = ?1 AND object_kind = ?2",
            params![object_id, kind.as_str(), i64::from(a), now],
        )?;
    }
    Ok(())
}

impl Store {
    /// Insert or update a thread by `(source, external_id)`. Triage is never touched.
    ///
    /// # Errors
    /// Database errors.
    pub fn upsert_thread(
        &self,
        source: &Source,
        external_id: &str,
        subject: &str,
        participants: &[String],
        last_activity_at: i64,
    ) -> StoreResult<Thread> {
        let source = source.clone();
        let external_id = external_id.to_owned();
        let subject = subject.to_owned();
        let participants = participants.to_vec();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let participants_json = serde_json::to_string(&participants)?;
            let existing = tx
                .query_row(
                    &format!("SELECT {THREAD_COLUMNS} FROM threads t WHERE t.source = ?1 AND t.external_id = ?2"),
                    params![source.as_name(), external_id],
                    thread_from_row,
                )
                .optional()?;
            let thread = if let Some(mut t) = existing {
                let changed = t.subject != subject
                    || t.participants != participants
                    || t.last_activity_at != last_activity_at;
                if changed {
                    t.subject = subject;
                    t.participants = participants;
                    t.last_activity_at = last_activity_at;
                    t.version += 1;
                    tx.execute(
                        "UPDATE threads SET subject = ?2, participants = ?3, last_activity_at = ?4, version = ?5 WHERE id = ?1",
                        params![t.id, t.subject, participants_json, t.last_activity_at, i64::try_from(t.version).unwrap_or(i64::MAX)],
                    )?;
                }
                t
            } else {
                let t = Thread {
                    id: new_id(),
                    source,
                    external_id,
                    subject,
                    participants,
                    last_activity_at,
                    version: 1,
                };
                tx.execute(
                    "INSERT INTO threads (id, source, external_id, subject, participants, last_activity_at, version) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![t.id, t.source.as_name(), t.external_id, t.subject, participants_json, t.last_activity_at, 1_i64],
                )?;
                t
            };
            tx.execute(
                "INSERT OR IGNORE INTO triage (object_id, object_kind, updated_at) VALUES (?1, 'thread', ?2)",
                params![thread.id, thread.last_activity_at],
            )?;
            tx.commit()?;
            Ok(thread)
        })
    }

    /// Insert or update a message by `(source, external_id)`; a new message bumps the thread version.
    ///
    /// # Errors
    /// Database errors, or validation if the thread does not exist.
    pub fn upsert_message(&self, message: &Message) -> StoreResult<Message> {
        let mut message = message.clone();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            if thread_version(&tx, &message.thread_id)?.is_none() {
                return Err(StoreError::Validation("message thread does not exist".into()));
            }
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM messages WHERE source = ?1 AND external_id = ?2",
                    params![message.source.as_name(), message.external_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                message.id = id;
                tx.execute(
                    "UPDATE messages SET sender_person_id = ?2, sender_identifier = ?3, body_text = ?4, sent_at = ?5 WHERE id = ?1",
                    params![message.id, message.sender_person_id, message.sender_identifier, message.body_text, message.sent_at],
                )?;
            } else {
                if message.id.is_empty() {
                    message.id = new_id();
                }
                tx.execute(
                    "INSERT INTO messages (id, thread_id, source, external_id, sender_person_id, sender_identifier, body_text, sent_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![message.id, message.thread_id, message.source.as_name(), message.external_id, message.sender_person_id, message.sender_identifier, message.body_text, message.sent_at],
                )?;
                tx.execute(
                    "UPDATE threads SET version = version + 1, last_activity_at = MAX(last_activity_at, ?2) WHERE id = ?1",
                    params![message.thread_id, message.sent_at],
                )?;
            }
            tx.commit()?;
            Ok(message)
        })
    }

    /// # Errors
    /// Database errors.
    pub fn get_thread(&self, id: &str) -> StoreResult<Option<(Thread, Triage)>> {
        let id = id.to_owned();
        self.read(move |conn| {
            let row = conn
                .query_row(
                    &format!(
                        "SELECT {THREAD_COLUMNS}, COALESCE(g.priority,0), g.snoozed_until, COALESCE(g.set_aside,0), COALESCE(g.archived,0), COALESCE(g.updated_at,0) \
                         FROM threads t LEFT JOIN triage g ON g.object_id = t.id AND g.object_kind = 'thread' WHERE t.id = ?1"
                    ),
                    [&id],
                    |r| {
                        let t = thread_from_row(r)?;
                        let g = triage_from_row(r, 7, &t.id)?;
                        Ok((t, g))
                    },
                )
                .optional()?;
            Ok(row)
        })
    }

    /// Look a thread up by its provider id.
    ///
    /// # Errors
    /// Database errors.
    pub fn find_thread(&self, source: &Source, external_id: &str) -> StoreResult<Option<Thread>> {
        let name = source.as_name();
        let external_id = external_id.to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {THREAD_COLUMNS} FROM threads t WHERE t.source = ?1 AND t.external_id = ?2"),
                    params![name, external_id],
                    thread_from_row,
                )
                .optional()?)
        })
    }

    /// # Errors
    /// Database errors.
    pub fn messages_for_thread(&self, thread_id: &str) -> StoreResult<Vec<Message>> {
        let thread_id = thread_id.to_owned();
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, thread_id, source, external_id, sender_person_id, sender_identifier, body_text, sent_at \
                 FROM messages WHERE thread_id = ?1 ORDER BY sent_at, id",
            )?;
            let rows = stmt.query_map([&thread_id], |r| {
                Ok(Message {
                    id: r.get(0)?,
                    thread_id: r.get(1)?,
                    source: Source::from(r.get::<_, String>(2)?),
                    external_id: r.get(3)?,
                    sender_person_id: r.get(4)?,
                    sender_identifier: r.get(5)?,
                    body_text: r.get(6)?,
                    sent_at: r.get(7)?,
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Unified listing across sources, newest first. Archived and snoozed are hidden by default.
    ///
    /// # Errors
    /// Database errors.
    pub fn list_threads(&self, filter: &ThreadFilter) -> StoreResult<Vec<(Thread, Triage)>> {
        let filter = filter.clone();
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {THREAD_COLUMNS}, COALESCE(g.priority,0), g.snoozed_until, COALESCE(g.set_aside,0), COALESCE(g.archived,0), COALESCE(g.updated_at,0) \
                 FROM threads t LEFT JOIN triage g ON g.object_id = t.id AND g.object_kind = 'thread' \
                 ORDER BY COALESCE(g.priority,0) DESC, t.last_activity_at DESC"
            ))?;
            let rows = stmt.query_map([], |r| {
                let t = thread_from_row(r)?;
                let g = triage_from_row(r, 7, &t.id)?;
                Ok((t, g))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (t, g) = row?;
                if !filter.sources.is_empty() && !filter.sources.contains(&t.source) {
                    continue;
                }
                if g.archived && !filter.include_archived {
                    continue;
                }
                let snoozed = g.snoozed_until.is_some_and(|until| until > filter.now);
                if snoozed && !filter.include_snoozed {
                    continue;
                }
                out.push((t, g));
            }
            Ok(out)
        })
    }

    /// Apply a partial triage update to an object.
    ///
    /// # Errors
    /// Database errors.
    pub fn set_triage(
        &self,
        object_id: &str,
        kind: ObjectKind,
        update: &TriageUpdate,
        now: i64,
    ) -> StoreResult<()> {
        let object_id = object_id.to_owned();
        let update = update.clone();
        self.write(move |conn| apply_triage(conn, &object_id, kind, &update, now))
    }

    /// # Errors
    /// Validation error if an identifier already belongs to another person.
    pub fn create_person(
        &self,
        display_name: &str,
        identifiers: &[Identifier],
    ) -> StoreResult<Person> {
        let name = display_name.to_owned();
        let identifiers = identifiers.to_vec();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            let id = new_id();
            tx.execute(
                "INSERT INTO people (id, display_name) VALUES (?1, ?2)",
                params![id, name],
            )?;
            for ident in &identifiers {
                insert_identifier(&tx, &id, ident)?;
            }
            tx.commit()?;
            Ok(Person {
                id,
                display_name: name,
                identifiers,
            })
        })
    }

    /// # Errors
    /// Validation error if the identifier belongs to another person.
    pub fn attach_identifier(&self, person_id: &str, identifier: &Identifier) -> StoreResult<()> {
        let person_id = person_id.to_owned();
        let identifier = identifier.clone();
        self.write(move |conn| insert_identifier(conn, &person_id, &identifier))
    }

    /// # Errors
    /// Database errors.
    pub fn find_person_by_identifier(
        &self,
        identifier: &Identifier,
    ) -> StoreResult<Option<String>> {
        let identifier = identifier.clone();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT person_id FROM identifiers WHERE kind = ?1 AND value = ?2",
                    params![identifier.kind.as_str(), identifier.value],
                    |r| r.get(0),
                )
                .optional()?)
        })
    }

    /// Insert a link after validating the vocabulary and that both ends exist.
    ///
    /// # Errors
    /// [`StoreError::Validation`] for an unknown link type or a missing object.
    pub fn add_link_checked(
        &self,
        from: (ObjectKind, &str),
        to: (ObjectKind, &str),
        link_type: &str,
    ) -> StoreResult<Link> {
        let link_type = LinkType::parse(link_type)
            .ok_or_else(|| StoreError::Validation(format!("unknown link type '{link_type}'")))?;
        let link = Link {
            from_kind: from.0,
            from_id: from.1.to_owned(),
            to_kind: to.0,
            to_id: to.1.to_owned(),
            link_type,
        };
        let stored = link.clone();
        self.write(move |conn| {
            for (kind, id) in [(stored.from_kind, &stored.from_id), (stored.to_kind, &stored.to_id)] {
                let table = match kind {
                    ObjectKind::Thread => "threads",
                    ObjectKind::Message => "messages",
                    ObjectKind::Person => "people",
                    ObjectKind::Proposal => "proposals",
                    ObjectKind::Reminder => "reminders",
                    ObjectKind::Event => "events",
                };
                let exists: bool = conn.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"),
                    [id],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(StoreError::Validation(format!("{} {id} does not exist", kind.as_str())));
                }
            }
            conn.execute(
                "INSERT OR IGNORE INTO links (from_kind, from_id, to_kind, to_id, link_type) VALUES (?1,?2,?3,?4,?5)",
                params![stored.from_kind.as_str(), stored.from_id, stored.to_kind.as_str(), stored.to_id, stored.link_type.as_str()],
            )?;
            Ok(())
        })?;
        Ok(link)
    }

    /// # Errors
    /// Database errors.
    pub fn count_links(&self) -> StoreResult<u64> {
        self.read(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))?;
            Ok(n.try_into().unwrap_or(0))
        })
    }
}

pub(crate) fn insert_identifier(
    conn: &Connection,
    person_id: &str,
    ident: &Identifier,
) -> StoreResult<()> {
    let owner: Option<String> = conn
        .query_row(
            "SELECT person_id FROM identifiers WHERE kind = ?1 AND value = ?2",
            params![ident.kind.as_str(), ident.value],
            |r| r.get(0),
        )
        .optional()?;
    match owner {
        Some(o) if o == person_id => Ok(()),
        Some(_) => Err(StoreError::Validation(format!(
            "identifier {} already belongs to another person",
            ident.value
        ))),
        None => {
            conn.execute(
                "INSERT INTO identifiers (kind, value, person_id) VALUES (?1, ?2, ?3)",
                params![ident.kind.as_str(), ident.value, person_id],
            )?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_store;
    use haste_core::model::IdentifierKind;

    fn thread(store: &Store, ext: &str, source: Source, subject: &str, at: i64) -> Thread {
        store
            .upsert_thread(&source, ext, subject, &["a@x".into()], at)
            .unwrap()
    }

    #[test]
    fn duplicate_external_id_updates_in_place() {
        let (_d, s) = test_store();
        let a = thread(&s, "e1", Source::Gmail, "hi", 10);
        let b = thread(&s, "e1", Source::Gmail, "hi again", 11);
        assert_eq!(a.id, b.id);
        assert_eq!(b.version, a.version + 1);
        let all = s.list_threads(&ThreadFilter::default()).unwrap();
        assert_eq!(all.len(), 1);
        // unchanged re-sync does not bump the version
        let c = thread(&s, "e1", Source::Gmail, "hi again", 11);
        assert_eq!(c.version, b.version);
    }

    #[test]
    fn triage_survives_resync() {
        let (_d, s) = test_store();
        let t = thread(&s, "e1", Source::Gmail, "hi", 10);
        s.set_triage(
            &t.id,
            ObjectKind::Thread,
            &TriageUpdate {
                set_aside: Some(true),
                priority: Some(2),
                ..Default::default()
            },
            20,
        )
        .unwrap();
        thread(&s, "e1", Source::Gmail, "changed", 30);
        let (_, g) = s.get_thread(&t.id).unwrap().unwrap();
        assert!(g.set_aside);
        assert_eq!(g.priority, 2);
    }

    #[test]
    fn filters_hide_archived_and_snoozed_and_select_sources() {
        let (_d, s) = test_store();
        let a = thread(&s, "e1", Source::Gmail, "a", 10);
        let b = thread(&s, "e2", Source::Icloud, "b", 11);
        let c = thread(&s, "e3", Source::Gmail, "c", 12);
        s.set_triage(
            &a.id,
            ObjectKind::Thread,
            &TriageUpdate {
                archived: Some(true),
                ..Default::default()
            },
            1,
        )
        .unwrap();
        s.set_triage(
            &c.id,
            ObjectKind::Thread,
            &TriageUpdate {
                snoozed_until: Some(Some(100)),
                ..Default::default()
            },
            1,
        )
        .unwrap();
        let default = s
            .list_threads(&ThreadFilter {
                now: 50,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(default.len(), 1);
        assert_eq!(default[0].0.id, b.id);
        let gmail = s
            .list_threads(&ThreadFilter {
                sources: vec![Source::Gmail],
                include_archived: true,
                include_snoozed: true,
                now: 50,
            })
            .unwrap();
        assert_eq!(gmail.len(), 2);
        // after the snooze elapses the thread is visible again
        let later = s
            .list_threads(&ThreadFilter {
                now: 200,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(later.len(), 2);
    }

    #[test]
    fn identifier_cannot_belong_to_two_people() {
        let (_d, s) = test_store();
        let ident = Identifier {
            kind: IdentifierKind::Email,
            value: "ana@x.org".into(),
        };
        s.create_person("Ana", std::slice::from_ref(&ident))
            .unwrap();
        let other = s.create_person("Not Ana", &[]).unwrap();
        assert!(matches!(
            s.attach_identifier(&other.id, &ident),
            Err(StoreError::Validation(_))
        ));
        assert!(s.find_person_by_identifier(&ident).unwrap().is_some());
    }

    #[test]
    fn links_validate_type_and_targets() {
        let (_d, s) = test_store();
        let t = thread(&s, "e1", Source::Gmail, "a", 1);
        let p = s.create_person("Ana", &[]).unwrap();
        assert!(s
            .add_link_checked(
                (ObjectKind::Thread, &t.id),
                (ObjectKind::Person, &p.id),
                "about"
            )
            .is_ok());
        assert!(matches!(
            s.add_link_checked(
                (ObjectKind::Thread, &t.id),
                (ObjectKind::Person, &p.id),
                "friend_of"
            ),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            s.add_link_checked(
                (ObjectKind::Thread, &t.id),
                (ObjectKind::Person, "nope"),
                "about"
            ),
            Err(StoreError::Validation(_))
        ));
        assert_eq!(s.count_links().unwrap(), 1);
    }

    #[test]
    fn new_message_bumps_thread_version_and_unknown_sender_is_kept() {
        let (_d, s) = test_store();
        let t = thread(&s, "e1", Source::Gmail, "a", 1);
        let m = Message {
            id: String::new(),
            thread_id: t.id.clone(),
            source: Source::Gmail,
            external_id: "m1".into(),
            sender_person_id: None,
            sender_identifier: Some("who@x".into()),
            body_text: "hello".into(),
            sent_at: 5,
        };
        let stored = s.upsert_message(&m).unwrap();
        s.upsert_message(&m).unwrap();
        let (t2, _) = s.get_thread(&t.id).unwrap().unwrap();
        assert_eq!(t2.version, t.version + 1);
        let msgs = s.messages_for_thread(&t.id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_identifier.as_deref(), Some("who@x"));
        assert_eq!(msgs[0].id, stored.id);
    }
}
