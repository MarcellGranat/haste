//! Which Reminders lists haste leaves out, kept in `reminders.json` in the data folder and read
//! again every cycle, so a change needs no restart.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// List names (case-insensitive) whose reminders are never read, stored or shown.
    #[serde(default)]
    pub exclude_lists: Vec<String>,
}

impl Settings {
    /// The settings in `path`; a missing or unreadable file means no exclusions (everything is
    /// shown) so a typo can never hide the whole app.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Is this list left out?
    #[must_use]
    pub fn excludes(&self, list: &str) -> bool {
        let list = list.trim().to_lowercase();
        self.exclude_lists
            .iter()
            .any(|l| l.trim().to_lowercase() == list)
    }

    /// A number that changes when the set of excluded lists does, so the connector knows to
    /// sweep again and drop what was stored before.
    #[must_use]
    pub fn digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut names: Vec<String> = self
            .exclude_lists
            .iter()
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty())
            .collect();
        if names.is_empty() {
            return 0;
        }
        names.sort();
        names.dedup();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        names.hash(&mut h);
        h.finish().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_match_ignoring_case_and_the_digest_follows_the_set() {
        let s = Settings {
            exclude_lists: vec![" Groceries ".into()],
        };
        assert!(s.excludes("groceries") && s.excludes("GROCERIES") && !s.excludes("Work"));
        assert_eq!(Settings::default().digest(), 0);
        let t = Settings {
            exclude_lists: vec!["groceries".into(), "GROCERIES".into()],
        };
        assert_eq!(s.digest(), t.digest());
        assert_ne!(s.digest(), 0);
    }

    #[test]
    fn a_missing_or_broken_file_excludes_nothing() {
        assert_eq!(
            Settings::load(Path::new("/nonexistent/reminders.json")),
            Settings::default()
        );
    }
}
