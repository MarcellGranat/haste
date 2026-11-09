//! Wire protocol between the daemon and its thin clients (TUI, CLI).
//!
//! Frames are a 4-byte big-endian length followed by a UTF-8 JSON body.

use std::io::{self, Read, Write};

use haste_core::herdr::{AgentStatusRecord, MachineState};
use haste_core::model::{Event as CalendarEvent, Message, Reminder, Thread, Triage};
use haste_core::policy::PrincipalKind;
use haste_core::proposal::{Payload, Proposal, ProposalState};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
/// Largest frame either side accepts.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub token: String,
    pub body: RequestBody,
}

impl Request {
    #[must_use]
    pub fn new(token: &str, body: RequestBody) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            token: token.to_owned(),
            body,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Threads,
    Proposals,
    Notices,
}

// Requests live for one round trip, so the size gap between `Propose` and the small variants
// costs nothing; boxing the payload would only churn every call site.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestBody {
    ThreadList {
        sources: Vec<String>,
        include_archived: bool,
        include_snoozed: bool,
    },
    ThreadShow {
        thread_id: String,
    },
    /// Create a proposal (draft, archive, snooze or reminder) for human review.
    Propose {
        payload: Payload,
        thread_id: Option<String>,
        idempotency_key: String,
        /// Where the agent runs in herdr. Routing hints only; never identity.
        #[serde(default)]
        hints: Option<Hints>,
    },
    NoteAppend {
        path: String,
        text: String,
        idempotency_key: String,
    },
    ProposalStatus {
        proposal_id: String,
    },
    ProposalList {
        states: Vec<ProposalState>,
    },
    Approve {
        proposal_id: String,
        acknowledge_stale: bool,
    },
    Reject {
        proposal_id: String,
        reason: Option<String>,
    },
    /// Human-only; never exposed by the agent CLI.
    Send {
        proposal_id: String,
        acknowledge_stale: bool,
    },
    RegisterPrincipal {
        principal_id: String,
        kind: PrincipalKind,
        allowed_actions: Vec<String>,
    },
    RevokeToken {
        principal_id: String,
    },
    AuditRecent {
        limit: u32,
    },
    /// Human-only: list connectors and whether their kill switch is engaged.
    ConnectorStatus,
    /// Human-only: disengage a connector's kill switch (the stored cursor is kept).
    ConnectorRelease {
        source: String,
    },
    /// Human-only: engage a connector's kill switch.
    ConnectorEngage {
        source: String,
    },
    /// Human-only: record that a connector was (re)authorized out of band, for the audit log.
    ConnectorAuthorized {
        source: String,
    },
    /// Human-only: add a project tag (a known workspace label) to a thread.
    TagAdd {
        thread_id: String,
        tag: String,
    },
    /// Human-only: remove a project tag from a thread.
    TagRemove {
        thread_id: String,
        tag: String,
    },
    /// Human-only: every known project tag.
    TagList,
    /// Agent status by machine, narrowed to what the caller may see.
    AgentList,
    /// Mirrored reminders the caller may see (titles included; never exposed in audit).
    RemindersList {
        include_completed: bool,
    },
    /// Human-only: add a project tag (a known workspace label) to a reminder.
    ReminderTagAdd {
        reminder_id: String,
        tag: String,
    },
    /// Human-only: remove a project tag from a reminder.
    ReminderTagRemove {
        reminder_id: String,
        tag: String,
    },
    /// Human-only: counts and permission state of the Reminders connector (no titles).
    RemindersStatus,
    /// Calendar events the caller may see, from `from` (unix seconds) on, soonest first.
    EventsList {
        from: i64,
    },
    /// Human-only: add a project tag (a known workspace label) to a calendar event.
    EventTagAdd {
        event_id: String,
        tag: String,
    },
    /// Human-only: remove a project tag from a calendar event.
    EventTagRemove {
        event_id: String,
        tag: String,
    },
    /// Human-only: run an approved reminder proposal (`approved -> executing -> sent`).
    Execute {
        proposal_id: String,
        acknowledge_stale: bool,
    },
    /// Human-only: settle a proposal left `executing`: `done` when the effect is visible in
    /// Reminders, otherwise it returns to `approved`.
    ResolveExecuting {
        proposal_id: String,
        done: bool,
    },
    Subscribe {
        topics: Vec<Topic>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnsupportedVersion,
    Unauthenticated,
    Unauthorized,
    Denied,
    NotFound,
    Validation,
    IllegalTransition,
    StaleNeedsAcknowledgement,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

/// Hints from the agent's herdr environment: `HASTE_MACHINE` and `HERDR_PANE_ID`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hints {
    #[serde(default)]
    pub machine: Option<String>,
    #[serde(default)]
    pub pane: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadView {
    pub thread: Thread,
    pub triage: Triage,
    /// Project tags (herdr workspace labels).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// One herdr machine as the daemon sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineView {
    pub machine: String,
    pub state: MachineState,
    pub agents: Vec<AgentStatusRecord>,
}

/// A reminder with its project tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderView {
    pub reminder: Reminder,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// A calendar event with its project tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventView {
    pub event: CalendarEvent,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Counts and permission state only: no titles, notes or list names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemindersStatusView {
    pub registered: bool,
    pub engaged: bool,
    /// `granted`, `denied`, `not_determined`, `restricted`, or `unknown` when it cannot be read.
    pub permission: String,
    pub total: u64,
    pub open: u64,
    pub missing: u64,
    /// Proposals left `executing` that need a human check.
    pub needs_check: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorView {
    pub source: String,
    pub engaged: bool,
    /// A connector implementation is currently loaded in the daemon.
    pub registered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    ThreadChanged {
        thread_id: String,
    },
    ProposalChanged {
        proposal_id: String,
        state: ProposalState,
    },
    Notice {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditLine {
    pub seq: u64,
    pub ts: i64,
    pub principal: String,
    pub action: String,
    pub outcome: String,
    pub object: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Threads {
        threads: Vec<ThreadView>,
    },
    Thread {
        view: ThreadView,
        messages: Vec<Message>,
    },
    /// Result of a pending outcome: the proposal was created (or returned again).
    Proposal {
        proposal: Proposal,
    },
    Proposals {
        proposals: Vec<Proposal>,
    },
    Registered {
        principal_id: String,
        token: String,
    },
    Appended {
        path: String,
        bytes: u64,
    },
    Audit {
        records: Vec<AuditLine>,
    },
    Connectors {
        connectors: Vec<ConnectorView>,
    },
    Tags {
        tags: Vec<String>,
    },
    Reminders {
        reminders: Vec<ReminderView>,
    },
    Events {
        events: Vec<EventView>,
    },
    RemindersStatus {
        status: RemindersStatusView,
    },
    Agents {
        machines: Vec<MachineView>,
    },
    Subscribed,
    Done,
    Event {
        event: Event,
    },
    Error {
        error: ErrorBody,
    },
}

impl Response {
    #[must_use]
    pub fn error(code: ErrorCode, message: &str) -> Self {
        Self::Error {
            error: ErrorBody {
                code,
                message: message.to_owned(),
            },
        }
    }
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    TooLarge(usize),
    Json(serde_json::Error),
}

impl From<io::Error> for FrameError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Encode a value into one frame.
///
/// # Errors
/// Fails if serialization fails or the body exceeds [`MAX_FRAME_BYTES`].
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let body = serde_json::to_vec(value).map_err(FrameError::Json)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(body.len()));
    }
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge(body.len()))?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Validate a frame header and return the announced body length.
///
/// # Errors
/// [`FrameError::TooLarge`] when the length exceeds [`MAX_FRAME_BYTES`].
pub fn parse_header(header: [u8; 4]) -> Result<usize, FrameError> {
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    Ok(len)
}

/// Blocking write of one frame.
///
/// # Errors
/// See [`encode_frame`]; also I/O errors.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    writer.write_all(&encode_frame(value)?)?;
    writer.flush()?;
    Ok(())
}

/// Blocking read of one frame.
///
/// # Errors
/// I/O errors, oversized frames, or invalid JSON.
pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(reader: &mut R) -> Result<T, FrameError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header)?;
    let len = parse_header(header)?;
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(FrameError::Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let req = Request::new(
            "tok",
            RequestBody::ProposalStatus {
                proposal_id: "p1".into(),
            },
        );
        let bytes = encode_frame(&req).unwrap();
        let back: Request = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let header = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
        assert!(matches!(parse_header(header), Err(FrameError::TooLarge(_))));
        let mut data = header.to_vec();
        data.extend_from_slice(b"{}");
        let r: Result<Request, _> = read_frame(&mut data.as_slice());
        assert!(matches!(r, Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn connector_admin_requests_round_trip() {
        for body in [
            RequestBody::ConnectorStatus,
            RequestBody::ConnectorRelease {
                source: "gmail".into(),
            },
            RequestBody::ConnectorEngage {
                source: "gmail".into(),
            },
            RequestBody::ConnectorAuthorized {
                source: "gmail".into(),
            },
        ] {
            let req = Request::new("t", body);
            let back: Request = read_frame(&mut encode_frame(&req).unwrap().as_slice()).unwrap();
            assert_eq!(req, back);
        }
        let resp = Response::Connectors {
            connectors: vec![ConnectorView {
                source: "gmail".into(),
                engaged: true,
                registered: true,
            }],
        };
        let back: Response = read_frame(&mut encode_frame(&resp).unwrap().as_slice()).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn herdr_requests_and_hints_round_trip_and_old_clients_still_parse() {
        for body in [
            RequestBody::TagAdd {
                thread_id: "t".into(),
                tag: "haste".into(),
            },
            RequestBody::TagRemove {
                thread_id: "t".into(),
                tag: "haste".into(),
            },
            RequestBody::TagList,
            RequestBody::AgentList,
            RequestBody::Propose {
                payload: Payload::Archive,
                thread_id: Some("t".into()),
                idempotency_key: "k".into(),
                hints: Some(Hints {
                    machine: Some("m1".into()),
                    pane: Some("w1:p1".into()),
                }),
            },
        ] {
            let req = Request::new("t", body);
            let back: Request = read_frame(&mut encode_frame(&req).unwrap().as_slice()).unwrap();
            assert_eq!(req, back);
        }
        // a Propose without `hints` (an older client) still parses
        let json = r#"{"version":1,"token":"t","body":{"type":"propose","payload":{"kind":"archive"},"thread_id":null,"idempotency_key":"k"}}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        assert!(matches!(req.body, RequestBody::Propose { hints: None, .. }));
    }

    #[test]
    fn unknown_version_survives_decoding_so_daemon_can_reject_it() {
        let json =
            r#"{"version":99,"token":"t","body":{"type":"subscribe","topics":["proposals"]}}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        assert_ne!(req.version, PROTOCOL_VERSION);
    }
}
