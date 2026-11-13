//! Herdr-derived data in the store: the last known workspace labels of each machine and the
//! project tags on threads. Agent status itself is never stored.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use haste_core::herdr::ProjectTag;
use rusqlite::{params, Connection, OptionalExtension};

use crate::{Store, StoreError, StoreResult};

fn parse_tags(json: Option<String>) -> Vec<String> {
    json.and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

fn known(conn: &Connection) -> StoreResult<BTreeSet<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT label FROM machine_labels")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

impl Store {
    /// Replace a machine's last known workspace labels.
    ///
    /// # Errors
    /// Database errors.
    pub fn replace_machine_labels(
        &self,
        machine: &str,
        labels: &BTreeSet<String>,
    ) -> StoreResult<()> {
        let machine = machine.to_owned();
        let labels = labels.clone();
        self.write(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM machine_labels WHERE machine = ?1", [&machine])?;
            for label in &labels {
                tx.execute(
                    "INSERT INTO machine_labels (machine, label) VALUES (?1, ?2)",
                    params![machine, label],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Forget a machine that herdr no longer lists. Its labels leave the known set.
    ///
    /// # Errors
    /// Database errors.
    pub fn forget_machine(&self, machine: &str) -> StoreResult<()> {
        let machine = machine.to_owned();
        self.write(move |conn| {
            conn.execute("DELETE FROM machine_labels WHERE machine = ?1", [&machine])?;
            Ok(())
        })
    }

    /// The labels last seen on one machine (empty if it is unknown).
    ///
    /// # Errors
    /// Database errors.
    pub fn machine_labels(&self, machine: &str) -> StoreResult<BTreeSet<String>> {
        let machine = machine.to_owned();
        self.read(move |conn| {
            let mut stmt = conn.prepare("SELECT label FROM machine_labels WHERE machine = ?1")?;
            let rows = stmt.query_map([&machine], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Every machine with at least one known label.
    ///
    /// # Errors
    /// Database errors.
    pub fn labels_by_machine(&self) -> StoreResult<BTreeMap<String, BTreeSet<String>>> {
        self.read(|conn| {
            let mut stmt = conn.prepare("SELECT machine, label FROM machine_labels")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for row in rows {
                let (machine, label) = row?;
                out.entry(machine).or_default().insert(label);
            }
            Ok(out)
        })
    }

    /// The known project tags: the union of every machine's last known workspace labels.
    ///
    /// # Errors
    /// Database errors.
    pub fn known_tags(&self) -> StoreResult<BTreeSet<String>> {
        self.read(known)
    }

    /// The tags on one thread.
    ///
    /// # Errors
    /// Database errors.
    pub fn thread_tags(&self, thread_id: &str) -> StoreResult<Vec<String>> {
        let id = thread_id.to_owned();
        self.read(move |conn| {
            let json: Option<Option<String>> = conn
                .query_row("SELECT scope_tags FROM threads WHERE id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()?;
            Ok(parse_tags(json.flatten()))
        })
    }

    /// Tags of every tagged thread, keyed by thread id.
    ///
    /// # Errors
    /// Database errors.
    pub fn all_thread_tags(&self) -> StoreResult<HashMap<String, Vec<String>>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare("SELECT id, scope_tags FROM threads WHERE scope_tags IS NOT NULL")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            let mut out = HashMap::new();
            for row in rows {
                let (id, json) = row?;
                let tags = parse_tags(json);
                if !tags.is_empty() {
                    out.insert(id, tags);
                }
            }
            Ok(out)
        })
    }

    /// Add a tag to a thread. Returns `true` if it was added, `false` if it was already there.
    ///
    /// # Errors
    /// [`StoreError::Validation`] for an invalid tag, [`StoreError::NotFound`] for a missing
    /// thread. Any valid label is accepted: a human may label a thread with a project that has no
    /// Herdr workspace. Such a label never widens what a machine principal can read, because that
    /// scope only matches the workspace labels on the machine itself.
    pub fn add_thread_tag(&self, thread_id: &str, tag: &str) -> StoreResult<bool> {
        let tag = ProjectTag::new(tag)
            .map_err(|e| StoreError::Validation(e.0.into()))?
            .as_str()
            .to_owned();
        let id = thread_id.to_owned();
        self.write(move |conn| {
            let current: Option<Option<String>> = conn
                .query_row("SELECT scope_tags FROM threads WHERE id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(current) = current else {
                return Err(StoreError::NotFound(format!("thread {id}")));
            };
            let mut tags = parse_tags(current);
            if tags.contains(&tag) {
                return Ok(false);
            }
            tags.push(tag);
            tags.sort();
            conn.execute(
                "UPDATE threads SET scope_tags = ?2 WHERE id = ?1",
                params![id, serde_json::to_string(&tags)?],
            )?;
            Ok(true)
        })
    }

    /// Remove a tag from a thread. Returns `true` if it was present.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] for a missing thread.
    pub fn remove_thread_tag(&self, thread_id: &str, tag: &str) -> StoreResult<bool> {
        let tag = tag.trim().to_owned();
        let id = thread_id.to_owned();
        self.write(move |conn| {
            let current: Option<Option<String>> = conn
                .query_row("SELECT scope_tags FROM threads WHERE id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(current) = current else {
                return Err(StoreError::NotFound(format!("thread {id}")));
            };
            let mut tags = parse_tags(current);
            let before = tags.len();
            tags.retain(|t| t != &tag);
            if tags.len() == before {
                return Ok(false);
            }
            let value = if tags.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&tags)?)
            };
            conn.execute(
                "UPDATE threads SET scope_tags = ?2 WHERE id = ?1",
                params![id, value],
            )?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_store;
    use haste_core::model::Source;

    fn labels(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn thread(s: &Store) -> String {
        s.upsert_thread(&Source::Gmail, "e1", "subject", &[], 1)
            .unwrap()
            .id
    }

    #[test]
    fn labels_are_replaced_per_machine_and_the_known_set_is_their_union() {
        let (_d, s) = test_store();
        s.replace_machine_labels("m1", &labels(&["haste", "dotfiles"]))
            .unwrap();
        s.replace_machine_labels("mbp", &labels(&["dotfiles", "notes"]))
            .unwrap();
        assert_eq!(
            s.known_tags().unwrap(),
            labels(&["dotfiles", "haste", "notes"]),
            "dotfiles counts once"
        );
        s.replace_machine_labels("m1", &labels(&["haste"])).unwrap();
        assert_eq!(s.machine_labels("m1").unwrap(), labels(&["haste"]));
        assert_eq!(s.machine_labels("nope").unwrap(), labels(&[]));
        s.forget_machine("mbp").unwrap();
        assert_eq!(s.known_tags().unwrap(), labels(&["haste"]));
    }

    #[test]
    fn the_known_set_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        Store::open(&path)
            .unwrap()
            .replace_machine_labels("m1", &labels(&["haste"]))
            .unwrap();
        assert_eq!(
            Store::open(&path).unwrap().known_tags().unwrap(),
            labels(&["haste"])
        );
    }

    #[test]
    fn any_valid_label_can_be_added_and_adding_twice_is_a_no_op() {
        let (_d, s) = test_store();
        let t = thread(&s);
        s.replace_machine_labels("m1", &labels(&["haste"])).unwrap();
        assert!(matches!(
            s.add_thread_tag(&t, "bad\u{7}"),
            Err(StoreError::Validation(_))
        ));
        assert!(matches!(
            s.add_thread_tag("no-such-thread", "haste"),
            Err(StoreError::NotFound(_))
        ));
        assert!(s.add_thread_tag(&t, "haste").unwrap());
        assert!(!s.add_thread_tag(&t, "haste").unwrap());
        assert_eq!(s.thread_tags(&t).unwrap(), ["haste"]);
        // a label with no Herdr workspace is fine for the human
        assert!(s.add_thread_tag(&t, "free text").unwrap());
        assert_eq!(s.thread_tags(&t).unwrap(), ["free text", "haste"]);
    }

    #[test]
    fn tags_can_be_removed_and_an_untagged_thread_has_none() {
        let (_d, s) = test_store();
        let t = thread(&s);
        s.replace_machine_labels("m1", &labels(&["haste", "notes"]))
            .unwrap();
        assert!(s.thread_tags(&t).unwrap().is_empty());
        s.add_thread_tag(&t, "notes").unwrap();
        s.add_thread_tag(&t, "haste").unwrap();
        assert_eq!(s.thread_tags(&t).unwrap(), ["haste", "notes"]);
        assert!(s.remove_thread_tag(&t, "haste").unwrap());
        assert!(!s.remove_thread_tag(&t, "haste").unwrap());
        assert_eq!(s.all_thread_tags().unwrap().get(&t).map(Vec::len), Some(1));
        s.remove_thread_tag(&t, "notes").unwrap();
        assert!(s.all_thread_tags().unwrap().is_empty());
    }

    #[test]
    fn a_sync_never_touches_tags() {
        let (_d, s) = test_store();
        let t = thread(&s);
        s.replace_machine_labels("m1", &labels(&["haste"])).unwrap();
        s.add_thread_tag(&t, "haste").unwrap();
        s.upsert_thread(&Source::Gmail, "e1", "a new subject", &["x@y".into()], 99)
            .unwrap();
        assert_eq!(s.thread_tags(&t).unwrap(), ["haste"]);
    }
}
