//! Observed herdr state and project tags. Pure types and functions; no I/O.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::policy::{Principal, PrincipalKind};

/// Longest accepted project tag, in characters.
pub const MAX_TAG_CHARS: usize = 64;
/// The label of the machine herdr runs on (the hub).
pub const LOCAL_MACHINE: &str = "local";

/// What herdr reports for an agent in a pane. Unknown future values read as `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Unknown => "unknown",
        }
    }
}

/// One agent in one pane. Deliberately holds no pane text, title, working directory or labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusRecord {
    pub machine: String,
    pub workspace_id: String,
    /// The workspace's name; this is the project tag.
    pub workspace_label: String,
    pub pane_id: String,
    pub agent: Option<String>,
    pub status: AgentStatus,
    /// Unix seconds when the agent entered `status`.
    pub status_since: i64,
}

/// How the daemon currently sees a herdr machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MachineState {
    Connected,
    Unreachable,
    Mismatch { expected: u32, observed: u32 },
}

/// The project name inside a herdr workspace label. Herdr labels read `[3] haste`, where the number
/// is only the workspace's position on that machine, so it is dropped: `haste` on two machines is
/// one project and reordering workspaces changes no tag. A label without the prefix is kept as is.
#[must_use]
pub fn project_name(label: &str) -> String {
    let label = label.trim();
    if let Some(rest) = label.strip_prefix('[') {
        if let Some((digits, name)) = rest.split_once(']') {
            let name = name.trim();
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) && !name.is_empty()
            {
                return name.to_owned();
            }
        }
    }
    label.to_owned()
}

/// A project tag: a herdr workspace label.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectTag(String);

#[derive(Debug, PartialEq, Eq)]
pub struct TagError(pub &'static str);

impl ProjectTag {
    /// # Errors
    /// Empty, longer than 64 characters, or containing control characters.
    pub fn new(label: &str) -> Result<Self, TagError> {
        let label = label.trim();
        if label.is_empty() {
            return Err(TagError("tag is empty"));
        }
        if label.chars().count() > MAX_TAG_CHARS {
            return Err(TagError("tag is longer than 64 characters"));
        }
        if label.chars().any(char::is_control) {
            return Err(TagError("tag contains a control character"));
        }
        Ok(Self(label.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProjectTag {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value).map_err(|e| e.0.to_owned())
    }
}

impl From<ProjectTag> for String {
    fn from(value: ProjectTag) -> Self {
        value.0
    }
}

/// May this principal see a thread with these tags?
///
/// * the human sees everything;
/// * a machine principal (its id is a herdr machine label) sees a thread only if one of the
///   thread's tags is in that machine's last known workspace-label set, and an id that matches
///   no known machine sees nothing;
/// * an agent principal's thread reads are not narrowed by tags (agents read mail as before; what
///   they may *do* is decided by the policy gate, and their proposal and agent-status visibility
///   is filtered separately).
#[must_use]
pub fn visible_to(
    principal: &Principal,
    thread_tags: &[String],
    machine_labels: Option<&BTreeSet<String>>,
) -> bool {
    match principal.kind {
        PrincipalKind::Human | PrincipalKind::Agent => true,
        PrincipalKind::Machine => {
            machine_labels.is_some_and(|labels| thread_tags.iter().any(|t| labels.contains(t)))
        }
    }
}

/// The herdr agent name for a principal id, valid only for `[a-z][a-z0-9_-]{0,31}`.
///
/// # Errors
/// The id does not match the pattern.
pub fn agent_name_for(principal_id: &str) -> Result<&str, &'static str> {
    let mut chars = principal_id.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if first_ok && rest_ok && principal_id.len() <= 32 {
        Ok(principal_id)
    } else {
        Err("principal id is not a valid herdr agent name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, kind: PrincipalKind) -> Principal {
        Principal {
            id: id.into(),
            kind,
        }
    }

    fn labels(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn tags(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_position_prefix_of_a_herdr_label_is_not_part_of_the_project_name() {
        assert_eq!(project_name("[4] haste"), "haste");
        assert_eq!(project_name("[12] my project"), "my project");
        assert_eq!(project_name("  [1]   gambling  "), "gambling");
        assert_eq!(project_name("[2] articleIVdb"), "articleIVdb");
        // anything that is not exactly "[digits] name" is left alone
        for odd in [
            "haste",
            "[x] haste",
            "[] haste",
            "[3]",
            "[3]   ",
            "[3 haste",
            "~",
            "[1] [2] nested",
        ] {
            let expected = if odd == "[1] [2] nested" {
                "[2] nested"
            } else {
                odd.trim()
            };
            assert_eq!(project_name(odd), expected, "{odd:?}");
        }
    }

    #[test]
    fn tag_validation() {
        assert_eq!(ProjectTag::new("  haste  ").unwrap().as_str(), "haste");
        assert!(ProjectTag::new("").is_err());
        assert!(ProjectTag::new("   ").is_err());
        assert!(ProjectTag::new("bad\u{7}tag").is_err());
        assert!(ProjectTag::new(&"x".repeat(65)).is_err());
        assert!(ProjectTag::new(&"x".repeat(64)).is_ok());
        assert!(
            ProjectTag::new("my project (2)").is_ok(),
            "labels may contain spaces and symbols"
        );
    }

    #[test]
    fn unknown_agent_statuses_read_as_unknown() {
        let s: AgentStatus = serde_json::from_str("\"asleep\"").unwrap();
        assert_eq!(s, AgentStatus::Unknown);
        let s: AgentStatus = serde_json::from_str("\"blocked\"").unwrap();
        assert_eq!(s, AgentStatus::Blocked);
    }

    #[test]
    fn human_sees_everything_even_untagged() {
        assert!(visible_to(&p("human", PrincipalKind::Human), &[], None));
    }

    #[test]
    fn machine_sees_only_threads_tagged_with_its_own_machines_labels() {
        let m1 = labels(&["haste", "notes"]);
        let m = p("m1", PrincipalKind::Machine);
        assert!(visible_to(&m, &tags(&["haste"]), Some(&m1)));
        assert!(visible_to(&m, &tags(&["other", "notes"]), Some(&m1)));
        assert!(!visible_to(&m, &tags(&["dotfiles"]), Some(&m1)));
        assert!(
            !visible_to(&m, &[], Some(&m1)),
            "untagged threads are human-only"
        );
    }

    #[test]
    fn a_machine_that_matches_no_known_machine_sees_nothing() {
        let m = p("nope", PrincipalKind::Machine);
        assert!(!visible_to(&m, &tags(&["haste"]), None));
    }

    #[test]
    fn agents_keep_their_existing_unscoped_thread_reads() {
        assert!(visible_to(
            &p("a", PrincipalKind::Agent),
            &tags(&["other"]),
            Some(&labels(&["haste"]))
        ));
        assert!(visible_to(&p("a", PrincipalKind::Agent), &[], None));
    }

    #[test]
    fn agent_names_follow_the_herdr_pattern() {
        assert_eq!(agent_name_for("agent-a"), Ok("agent-a"));
        assert_eq!(agent_name_for("a"), Ok("a"));
        assert_eq!(agent_name_for(&"a".repeat(32)), Ok(&*"a".repeat(32)));
        for bad in ["Agent A", "1agent", "-a", "", "agent.a", &"a".repeat(33)] {
            assert!(agent_name_for(bad).is_err(), "{bad:?}");
        }
    }
}
