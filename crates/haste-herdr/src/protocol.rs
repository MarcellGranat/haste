//! The slice of herdr's protocol-22 wire format that haste uses, as typed values.
//! Checked against the committed schema in `tests/schema.rs`.

use std::fmt;

use haste_core::herdr::AgentStatus;
use serde::Deserialize;

/// The only herdr protocol number haste speaks.
pub const HERDR_PROTOCOL: u32 = 22;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedError {
    /// The server or snapshot reports a protocol other than [`HERDR_PROTOCOL`].
    Mismatch { expected: u32, observed: u32 },
    /// A message could not be understood (for example a required field is missing).
    Malformed(String),
    /// Connection or process trouble.
    Io(String),
}

impl fmt::Display for FeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mismatch { expected, observed } => {
                write!(
                    f,
                    "herdr protocol {observed} does not match the expected {expected}"
                )
            }
            Self::Malformed(m) => write!(f, "malformed herdr message: {m}"),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for FeedError {}

/// A workspace. `label` is the workspace's name and the project tag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
}

/// A pane. Only its id and workspace are read; the working directory is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneEntry {
    pub pane_id: String,
    pub workspace_id: String,
}

/// An agent in a pane. Titles, working directories and tokens in the wire record are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentEntry {
    pub pane_id: String,
    pub workspace_id: String,
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Snapshot {
    pub protocol: u32,
    pub workspaces: Vec<Workspace>,
    pub agents: Vec<AgentEntry>,
    /// Every pane, with or without a detected agent, so a pane can be mapped to its workspace.
    #[serde(default)]
    pub panes: Vec<PaneEntry>,
}

/// `pane.agent_status_changed` event data. Titles and state labels are not deserialised.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StatusEvent {
    pub pane_id: String,
    pub workspace_id: String,
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub agent: Option<String>,
}

/// Parse a response that wraps a snapshot: `{"id": .., "result": {"snapshot": {..}}}`, which is
/// what both the socket (`session.snapshot`) and `herdr api snapshot` return.
///
/// # Errors
/// [`FeedError::Malformed`] when the JSON or a required field is missing,
/// [`FeedError::Mismatch`] when the snapshot's protocol is not the pinned one.
pub fn parse_snapshot(text: &str) -> Result<Snapshot, FeedError> {
    #[derive(Deserialize)]
    struct Envelope {
        result: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        snapshot: Snapshot,
    }
    // The protocol number is checked before anything else: a snapshot from another protocol may
    // have different fields, and "mismatch" is the useful answer, not "malformed".
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| FeedError::Malformed(e.to_string()))?;
    let observed = value["result"]["snapshot"]["protocol"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .ok_or_else(|| FeedError::Malformed("snapshot has no protocol number".into()))?;
    if observed != HERDR_PROTOCOL {
        return Err(FeedError::Mismatch {
            expected: HERDR_PROTOCOL,
            observed,
        });
    }
    let envelope: Envelope =
        serde_json::from_value(value).map_err(|e| FeedError::Malformed(e.to_string()))?;
    let snapshot = envelope.result.snapshot;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_json(protocol: u32) -> String {
        format!(
            r#"{{"id":"x","result":{{"type":"session_snapshot","snapshot":{{
              "protocol":{protocol},"version":"0.9.5","tabs":[],"panes":[{{"pane_id":"w1:p1","workspace_id":"w1","cwd":"/secret/path"}}],
              "workspaces":[{{"workspace_id":"w1","label":"haste","agent_status":"idle","number":1}}],
              "agents":[{{"pane_id":"w1:p1","workspace_id":"w1","agent_status":"blocked","agent":"claude",
                         "terminal_title":"SECRET TITLE","cwd":"/secret/path","tokens":{{"topic":"SECRET"}}}}]}}}}}}"#
        )
    }

    #[test]
    fn a_real_shaped_snapshot_parses_and_ignores_titles_and_paths() {
        let s = parse_snapshot(&snapshot_json(22)).unwrap();
        assert_eq!(s.workspaces[0].label, "haste");
        assert_eq!(s.agents[0].agent_status, AgentStatus::Blocked);
        assert_eq!(s.agents[0].agent.as_deref(), Some("claude"));
        let dump = format!("{s:?}");
        assert!(!dump.contains("SECRET"), "{dump}");
    }

    #[test]
    fn another_protocol_is_a_mismatch_naming_both_numbers() {
        assert_eq!(
            parse_snapshot(&snapshot_json(23)),
            Err(FeedError::Mismatch {
                expected: 22,
                observed: 23
            })
        );
    }

    #[test]
    fn a_future_protocol_with_different_fields_is_still_reported_as_a_mismatch() {
        let future = r#"{"id":"x","result":{"type":"session_snapshot","snapshot":{"protocol":23,"brand_new":[]}}}"#;
        assert_eq!(
            parse_snapshot(future),
            Err(FeedError::Mismatch {
                expected: 22,
                observed: 23
            })
        );
        let no_protocol = r#"{"id":"x","result":{"snapshot":{}}}"#;
        assert!(matches!(
            parse_snapshot(no_protocol),
            Err(FeedError::Malformed(_))
        ));
    }

    #[test]
    fn a_missing_required_field_is_malformed_not_guessed() {
        let broken = snapshot_json(22).replace("\"agent_status\":\"blocked\",", "");
        assert!(matches!(
            parse_snapshot(&broken),
            Err(FeedError::Malformed(_))
        ));
        assert!(matches!(
            parse_snapshot("not json"),
            Err(FeedError::Malformed(_))
        ));
    }
}
