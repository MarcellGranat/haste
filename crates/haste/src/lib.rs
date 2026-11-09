//! The `haste` command line. Agents call this; every call goes through the daemon.
//!
//! There is deliberately no `send` verb. Human-only verbs (register, revoke, approve,
//! reject) exist only when the crate is built with the `human` feature.

use std::fs;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use haste_core::auth::new_id;
use haste_core::proposal::Payload;
use haste_core::time::now_secs;
use haste_proto::{read_frame, write_frame, Hints, Request, RequestBody, Response};

pub const HOME_ENV: &str = "HASTE_HOME";
/// Hint naming which agent token file to present. Never treated as identity.
pub const AGENT_HINT_ENV: &str = "HASTE_AGENT";
/// Explicit token file, overriding the hint lookup.
pub const TOKEN_FILE_ENV: &str = "HASTE_TOKEN_FILE";
/// Herdr sets this in every pane. Sent to the daemon as a hint to find the agent's workspace.
pub const PANE_HINT_ENV: &str = "HERDR_PANE_ID";
/// The herdr machine this agent runs on (`local` when unset): the hub's saved-machine label.
pub const MACHINE_HINT_ENV: &str = "HASTE_MACHINE";
/// The hub itself.
pub const LOCAL_MACHINE: &str = "local";
const DEFAULT_BASE: &str = "Library/Application Support/haste";
const HINT_MAX_LEN: usize = 32;
const SECS_PER_HOUR: i64 = 3600;
const SECS_PER_DAY: i64 = 86_400;

#[derive(Parser, Debug)]
#[command(
    name = "haste",
    about = "Personal inbox hub: read, propose, never send"
)]
pub struct Cli {
    /// Base directory (defaults to $HASTE_HOME or ~/Library/Application Support/haste).
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Read threads.
    Thread {
        #[command(subcommand)]
        cmd: ThreadCmd,
    },
    /// Propose a draft reply (needs human approval; cannot be sent from here).
    Draft(DraftArgs),
    /// Propose archiving a thread.
    Archive(KeyedThread),
    /// Propose snoozing a thread.
    Snooze(SnoozeArgs),
    /// Propose a reminder.
    Reminder {
        #[command(subcommand)]
        cmd: ReminderCmd,
    },
    /// Read calendar events and propose creating or changing one (a human approves each).
    Event {
        #[command(subcommand)]
        cmd: EventCmd,
    },
    /// Email actions (human shell only).
    #[cfg(feature = "human")]
    Email {
        #[command(subcommand)]
        cmd: EmailCmd,
    },
    /// Append to a note inside the allow-listed folders.
    Note {
        #[command(subcommand)]
        cmd: NoteCmd,
    },
    /// Inspect or decide proposals.
    Proposal {
        #[command(subcommand)]
        cmd: ProposalCmd,
    },
    /// Manage agents (human shell only).
    #[cfg(feature = "human")]
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    /// Manage tokens (human shell only).
    #[cfg(feature = "human")]
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// Connector administration: status, release, engage, Gmail auth (human shell only).
    #[cfg(feature = "human")]
    Connector {
        #[command(subcommand)]
        cmd: ConnectorCmd,
    },
    /// Project tags on threads (human shell only). Tags are herdr workspace names.
    #[cfg(feature = "human")]
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// Herdr machines and their agents (human shell only).
    #[cfg(feature = "human")]
    Machine {
        #[command(subcommand)]
        cmd: MachineCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum ThreadCmd {
    List {
        /// Restrict to a source (repeatable), e.g. `--source gmail`.
        #[arg(long = "source")]
        sources: Vec<String>,
        #[arg(long)]
        archived: bool,
        #[arg(long)]
        snoozed: bool,
    },
    Show {
        id: String,
    },
}

#[derive(Args, Debug)]
pub struct DraftArgs {
    #[arg(long)]
    pub thread: String,
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    #[arg(long)]
    pub body_file: Option<PathBuf>,
    /// Idempotency key; retries with the same key return the same proposal.
    #[arg(long)]
    pub key: Option<String>,
}

#[derive(Args, Debug)]
pub struct KeyedThread {
    pub thread: String,
    #[arg(long)]
    pub key: Option<String>,
}

#[derive(Args, Debug)]
pub struct SnoozeArgs {
    pub thread: String,
    #[arg(long, group = "when")]
    pub hours: Option<i64>,
    #[arg(long, group = "when")]
    pub days: Option<i64>,
    /// Absolute wake-up time as Unix seconds.
    #[arg(long, group = "when")]
    pub until: Option<i64>,
    #[arg(long)]
    pub key: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ReminderCmd {
    /// Propose a new reminder (a human approves and runs it).
    Create {
        #[arg(long)]
        title: String,
        /// Project tag (a herdr workspace name). Defaults to your pane's workspace.
        #[arg(long)]
        project: Option<String>,
        /// The mail thread this reminder is about (stored as an `about` link).
        #[arg(long, alias = "from-thread")]
        thread: Option<String>,
        /// Due time as Unix seconds.
        #[arg(long)]
        due: Option<i64>,
        /// Reminders list name.
        #[arg(long)]
        list: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Propose marking a reminder done.
    Complete {
        /// Haste reminder id (from `reminder list`).
        reminder: String,
        #[arg(long)]
        key: Option<String>,
    },
    /// List mirrored reminders you may see.
    List {
        /// Include completed reminders.
        #[arg(long)]
        completed: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum EventCmd {
    /// List the calendar events you may see.
    List {
        /// Only events ending at or after this Unix time (default: now).
        #[arg(long)]
        from: Option<i64>,
    },
    /// Propose a new event. Nobody is emailed an invitation by haste.
    Create {
        #[arg(long)]
        calendar: String,
        #[arg(long)]
        title: String,
        /// Start as Unix seconds.
        #[arg(long)]
        start: i64,
        /// End as Unix seconds.
        #[arg(long)]
        end: i64,
        #[arg(long)]
        all_day: bool,
        /// Attendee address (repeatable). They are stored, not notified.
        #[arg(long = "attendee")]
        attendees: Vec<String>,
        #[arg(long)]
        description: Option<String>,
        /// Project tag (a herdr workspace name). Defaults to your pane's workspace.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Propose changing an event's title, times, description or location. Attendees and
    /// cancelling cannot be changed from here.
    Update {
        /// Haste event id (from `event list`).
        event: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        start: Option<i64>,
        #[arg(long)]
        end: Option<i64>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        location: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum EmailCmd {
    /// Propose a reminder about a mail thread; you then approve and run it.
    ToReminder {
        thread: String,
        /// Defaults to the thread subject.
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        due: Option<i64>,
        #[arg(long)]
        list: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum RemindersCmd {
    /// Ask macOS for Reminders access. Run this from a normal Terminal window.
    Auth,
    /// Permission, kill switch and counts (never titles).
    Status,
}

#[derive(Subcommand, Debug)]
pub enum NoteCmd {
    /// Append a dated entry to a Person's note (the daemon writes the front matter and heading).
    Person {
        /// Haste person id.
        person: String,
        #[arg(long, conflicts_with = "stdin")]
        text: Option<String>,
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        key: Option<String>,
    },
    /// Record an identifier-only back-reference (`- haste:<kind>/<id>`); no free text.
    Ref {
        /// `thread`, `reminder` or `event`.
        kind: String,
        id: String,
        /// Topic label naming the note (never an email subject or reminder title).
        #[arg(long)]
        topic: String,
        /// Workspace label: put the note under `projects/<project>/tasks/`.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        key: Option<String>,
    },
    /// Read a note under the notes folder (human shell only).
    #[cfg(feature = "human")]
    Show { path: String },
    /// List note writes from the audit: path, outcome, bytes or reason; never text (human shell only).
    #[cfg(feature = "human")]
    Log {
        /// Only rows at or after this Unix time.
        #[arg(long)]
        since: Option<i64>,
    },
    /// Set how an agent's notes are decided: `auto` writes directly, `approval` queues them
    /// for you (human shell only).
    #[cfg(feature = "human")]
    Mode { principal: String, mode: String },
    Append {
        /// Path relative to the notes root, e.g. people/ana.md
        path: String,
        #[arg(long, conflicts_with = "stdin")]
        text: Option<String>,
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        key: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProposalCmd {
    Status {
        id: String,
    },
    #[cfg(feature = "human")]
    Approve {
        id: String,
        /// Acknowledge that the thread changed since the proposal.
        #[arg(long)]
        ack_stale: bool,
    },
    #[cfg(feature = "human")]
    Reject {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Run an approved reminder proposal in Reminders.
    #[cfg(feature = "human")]
    Execute {
        id: String,
        #[arg(long)]
        ack_stale: bool,
    },
    /// Settle a proposal left `executing`: --done if it shows in Reminders, otherwise it
    /// returns to approved.
    #[cfg(feature = "human")]
    Resolve {
        id: String,
        #[arg(long)]
        done: bool,
    },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    Register {
        id: String,
        #[arg(long, default_value = "agent")]
        kind: String,
        /// Actions the token may request (repeatable).
        #[arg(long = "allow")]
        allow: Vec<String>,
        /// Save the token to `<home>/tokens/<id>` (mode 0600) instead of printing it.
        #[arg(long)]
        write_token: bool,
    },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum TokenCmd {
    Revoke { id: String },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum ConnectorCmd {
    /// List connectors and whether their kill switch is engaged.
    Status,
    /// Disengage a connector's kill switch; sync resumes from its stored cursor.
    Release { source: String },
    /// Engage a connector's kill switch; nothing reaches the provider until released.
    Engage { source: String },
    /// Provider-specific setup.
    Gmail {
        #[command(subcommand)]
        cmd: GmailCmd,
    },
    /// Provider-specific setup for iCloud Mail.
    Icloud {
        #[command(subcommand)]
        cmd: IcloudCmd,
    },
    /// Apple Reminders: permission and status.
    Reminders {
        #[command(subcommand)]
        cmd: RemindersCmd,
    },
    /// Provider-specific setup for Google Calendar.
    Gcal {
        #[command(subcommand)]
        cmd: GcalCmd,
    },
    /// Beeper (WhatsApp, Messenger, iMessage): store the local API token.
    Beeper {
        #[command(subcommand)]
        cmd: BeeperCmd,
    },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum BeeperCmd {
    /// Paste the Beeper Desktop API token (not echoed). It is checked against the local API and
    /// stored in the Keychain; a failed check stores nothing. Needs beeper.json first.
    Auth,
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum GcalCmd {
    /// One-time consent for the two calendar scopes (opens your browser). Uses
    /// $HASTE_GMAIL_CLIENT_ID and $HASTE_GMAIL_CLIENT_SECRET.
    Auth,
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum GmailCmd {
    /// Run the one-time OAuth consent flow (opens your browser).
    Auth {
        /// OAuth client id of your Google Cloud project (or $HASTE_GMAIL_CLIENT_ID).
        #[arg(long, env = "HASTE_GMAIL_CLIENT_ID")]
        client_id: String,
        /// OAuth client secret (or $HASTE_GMAIL_CLIENT_SECRET).
        #[arg(long, env = "HASTE_GMAIL_CLIENT_SECRET")]
        client_secret: String,
    },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum TagCmd {
    /// Tag a thread with a project (a known herdr workspace name).
    Add { thread: String, tag: String },
    /// Remove a tag from a thread.
    Remove { thread: String, tag: String },
    /// List the known project tags.
    List,
    /// Tag a reminder with a project (a known herdr workspace name).
    AddReminder { reminder: String, tag: String },
    /// Remove a tag from a reminder.
    RemoveReminder { reminder: String, tag: String },
    /// Tag a calendar event with a project (a known herdr workspace name).
    AddEvent { event: String, tag: String },
    /// Remove a tag from a calendar event.
    RemoveEvent { event: String, tag: String },
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum MachineCmd {
    /// Each herdr machine's state and its agents.
    Status,
}

#[cfg(feature = "human")]
#[derive(Subcommand, Debug)]
pub enum IcloudCmd {
    /// One-time setup: asks for your app-specific password (no echo) and checks it with iCloud.
    Auth {
        /// Your Apple ID address (asked for if omitted).
        #[arg(long)]
        address: Option<String>,
        /// A folder to sync (repeatable). INBOX only if you give none.
        #[arg(long = "folder")]
        folders: Vec<String>,
        /// How many of the newest messages the first sync fetches per folder (default 200).
        #[arg(long)]
        backfill: Option<usize>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum CliError {
    Usage(String),
    MissingToken(String),
    Connect(String),
    Daemon(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(m) | Self::MissingToken(m) | Self::Connect(m) | Self::Daemon(m) => {
                f.write_str(m)
            }
        }
    }
}

fn key_or_new(key: &Option<String>) -> String {
    key.clone().unwrap_or_else(new_id)
}

/// Routing hints for a proposal: which herdr pane and machine the agent is in. Without a pane
/// there is nothing to look up, so no hints are sent. Hints never affect who the caller is.
#[must_use]
pub fn hints_from(machine: Option<&str>, pane: Option<&str>) -> Option<Hints> {
    let pane = pane.map(str::trim).filter(|p| !p.is_empty())?;
    let machine = machine
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or(LOCAL_MACHINE);
    Some(Hints {
        machine: Some(machine.to_owned()),
        pane: Some(pane.to_owned()),
    })
}

fn env_hints() -> Option<Hints> {
    hints_from(
        std::env::var(MACHINE_HINT_ENV).ok().as_deref(),
        std::env::var(PANE_HINT_ENV).ok().as_deref(),
    )
}

/// Whether a command needs the human token rather than an agent token.
#[must_use]
pub fn is_human_command(command: &Command) -> bool {
    match command {
        #[cfg(feature = "human")]
        Command::Agent { .. }
        | Command::Token { .. }
        | Command::Connector { .. }
        | Command::Tag { .. }
        | Command::Email { .. }
        | Command::Machine { .. } => true,
        #[cfg(feature = "human")]
        Command::Note {
            cmd: NoteCmd::Show { .. } | NoteCmd::Log { .. } | NoteCmd::Mode { .. },
        } => true,
        #[cfg(feature = "human")]
        Command::Proposal {
            cmd:
                ProposalCmd::Approve { .. }
                | ProposalCmd::Reject { .. }
                | ProposalCmd::Execute { .. }
                | ProposalCmd::Resolve { .. },
        } => true,
        _ => {
            let _ = command;
            false
        }
    }
}

/// Translate a parsed command into a protocol request body.
///
/// # Errors
/// [`CliError::Usage`] for unreadable body files or invalid arguments.
pub fn build_body(command: &Command) -> Result<RequestBody, CliError> {
    Ok(match command {
        Command::Thread {
            cmd:
                ThreadCmd::List {
                    sources,
                    archived,
                    snoozed,
                },
        } => RequestBody::ThreadList {
            sources: sources.clone(),
            include_archived: *archived,
            include_snoozed: *snoozed,
        },
        Command::Thread {
            cmd: ThreadCmd::Show { id },
        } => RequestBody::ThreadShow {
            thread_id: id.clone(),
        },
        Command::Draft(a) => {
            let body = match (&a.body, &a.body_file) {
                (Some(b), None) => b.clone(),
                (None, Some(f)) => fs::read_to_string(f)
                    .map_err(|e| CliError::Usage(format!("cannot read body file: {e}")))?,
                _ => return Err(CliError::Usage("give --body or --body-file".into())),
            };
            RequestBody::Propose {
                payload: Payload::Draft {
                    to: a.to.clone(),
                    body,
                },
                thread_id: Some(a.thread.clone()),
                idempotency_key: key_or_new(&a.key),
                hints: env_hints(),
            }
        }
        Command::Archive(a) => RequestBody::Propose {
            payload: Payload::Archive,
            thread_id: Some(a.thread.clone()),
            idempotency_key: key_or_new(&a.key),
            hints: env_hints(),
        },
        Command::Snooze(a) => {
            let until = match (a.hours, a.days, a.until) {
                (Some(h), None, None) => now_secs() + h * SECS_PER_HOUR,
                (None, Some(d), None) => now_secs() + d * SECS_PER_DAY,
                (None, None, Some(u)) => u,
                _ => {
                    return Err(CliError::Usage(
                        "give exactly one of --hours, --days, --until".into(),
                    ))
                }
            };
            RequestBody::Propose {
                payload: Payload::Snooze { until },
                thread_id: Some(a.thread.clone()),
                idempotency_key: key_or_new(&a.key),
                hints: env_hints(),
            }
        }
        Command::Reminder {
            cmd:
                ReminderCmd::Create {
                    title,
                    project,
                    thread,
                    due,
                    list,
                    key,
                },
        } => RequestBody::Propose {
            payload: Payload::ReminderCreate {
                title: title.clone(),
                due: *due,
                list: list.clone(),
                project: project.clone(),
                thread_id: None,
            },
            thread_id: thread.clone(),
            idempotency_key: key_or_new(key),
            hints: env_hints(),
        },
        Command::Reminder {
            cmd: ReminderCmd::Complete { reminder, key },
        } => RequestBody::Propose {
            payload: Payload::ReminderComplete {
                reminder_id: reminder.clone(),
                project: None,
                reminder_version: None,
            },
            thread_id: None,
            idempotency_key: key_or_new(key),
            hints: env_hints(),
        },
        Command::Reminder {
            cmd: ReminderCmd::List { completed },
        } => RequestBody::RemindersList {
            include_completed: *completed,
        },
        Command::Event {
            cmd: EventCmd::List { from },
        } => RequestBody::EventsList {
            from: from.unwrap_or_else(haste_core::time::now_secs),
        },
        Command::Event {
            cmd:
                EventCmd::Create {
                    calendar,
                    title,
                    start,
                    end,
                    all_day,
                    attendees,
                    description,
                    project,
                    key,
                },
        } => RequestBody::Propose {
            payload: Payload::CalendarCreate {
                calendar: calendar.clone(),
                title: title.clone(),
                start: *start,
                end: *end,
                all_day: *all_day,
                attendees: attendees.clone(),
                description: description.clone(),
                project: project.clone(),
            },
            thread_id: None,
            idempotency_key: key_or_new(key),
            hints: env_hints(),
        },
        Command::Event {
            cmd:
                EventCmd::Update {
                    event,
                    title,
                    start,
                    end,
                    description,
                    location,
                    key,
                },
        } => RequestBody::Propose {
            payload: Payload::CalendarUpdate {
                event_id: event.clone(),
                changes: haste_core::proposal::EventChanges {
                    title: title.clone(),
                    start: *start,
                    end: *end,
                    description: description.clone(),
                    location: location.clone(),
                    attendees: None,
                    status: None,
                },
                project: None,
                event_version: None,
            },
            thread_id: None,
            idempotency_key: key_or_new(key),
            hints: env_hints(),
        },
        #[cfg(feature = "human")]
        Command::Email {
            cmd:
                EmailCmd::ToReminder {
                    thread,
                    title,
                    project,
                    due,
                    list,
                    key,
                },
        } => RequestBody::Propose {
            payload: Payload::ReminderCreate {
                title: title.clone().ok_or_else(|| {
                    CliError::Usage("the thread subject could not be used as the title".into())
                })?,
                due: *due,
                list: list.clone(),
                project: project.clone(),
                thread_id: None,
            },
            thread_id: Some(thread.clone()),
            idempotency_key: key
                .clone()
                .unwrap_or_else(|| format!("to-reminder-{thread}")),
            hints: None,
        },
        Command::Note {
            cmd:
                NoteCmd::Person {
                    person,
                    text,
                    stdin,
                    key,
                },
        } => RequestBody::NotePerson {
            person_id: person.clone(),
            text: note_text(text.as_deref(), *stdin)?,
            idempotency_key: key_or_new(key),
        },
        Command::Note {
            cmd:
                NoteCmd::Ref {
                    kind,
                    id,
                    topic,
                    project,
                    key,
                },
        } => RequestBody::NoteReference(haste_proto::NoteReferenceArgs {
            kind: match kind.as_str() {
                "thread" => haste_proto::RefKind::Thread,
                "reminder" => haste_proto::RefKind::Reminder,
                "event" => haste_proto::RefKind::Event,
                other => {
                    return Err(CliError::Usage(format!(
                        "unknown kind '{other}' (thread, reminder or event)"
                    )))
                }
            },
            id: id.clone(),
            topic: topic.clone(),
            project: project.clone(),
            idempotency_key: key_or_new(key),
        }),
        #[cfg(feature = "human")]
        Command::Note {
            cmd: NoteCmd::Show { path },
        } => RequestBody::NoteShow { path: path.clone() },
        #[cfg(feature = "human")]
        Command::Note {
            cmd: NoteCmd::Log { since },
        } => RequestBody::NoteLog { since: *since },
        #[cfg(feature = "human")]
        Command::Note {
            cmd: NoteCmd::Mode { principal, mode },
        } => RequestBody::NoteMode {
            principal_id: principal.clone(),
            mode: haste_core::policy::NoteMode::parse(mode)
                .ok_or_else(|| CliError::Usage("mode must be 'auto' or 'approval'".into()))?,
        },
        Command::Note {
            cmd:
                NoteCmd::Append {
                    path,
                    text,
                    stdin,
                    key,
                },
        } => {
            let text = if *stdin {
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| CliError::Usage(e.to_string()))?;
                buf
            } else {
                text.clone()
                    .ok_or_else(|| CliError::Usage("give --text or --stdin".into()))?
            };
            RequestBody::NoteAppend {
                path: path.clone(),
                text,
                idempotency_key: key_or_new(key),
            }
        }
        Command::Proposal {
            cmd: ProposalCmd::Status { id },
        } => RequestBody::ProposalStatus {
            proposal_id: id.clone(),
        },
        #[cfg(feature = "human")]
        Command::Proposal {
            cmd: ProposalCmd::Approve { id, ack_stale },
        } => RequestBody::Approve {
            proposal_id: id.clone(),
            acknowledge_stale: *ack_stale,
        },
        #[cfg(feature = "human")]
        Command::Proposal {
            cmd: ProposalCmd::Reject { id, reason },
        } => RequestBody::Reject {
            proposal_id: id.clone(),
            reason: reason.clone(),
        },
        #[cfg(feature = "human")]
        Command::Proposal {
            cmd: ProposalCmd::Execute { id, ack_stale },
        } => RequestBody::Execute {
            proposal_id: id.clone(),
            acknowledge_stale: *ack_stale,
        },
        #[cfg(feature = "human")]
        Command::Proposal {
            cmd: ProposalCmd::Resolve { id, done },
        } => RequestBody::ResolveExecuting {
            proposal_id: id.clone(),
            done: *done,
        },
        #[cfg(feature = "human")]
        Command::Tag {
            cmd: TagCmd::AddEvent { event, tag },
        } => RequestBody::EventTagAdd {
            event_id: event.clone(),
            tag: tag.clone(),
        },
        #[cfg(feature = "human")]
        Command::Tag {
            cmd: TagCmd::RemoveEvent { event, tag },
        } => RequestBody::EventTagRemove {
            event_id: event.clone(),
            tag: tag.clone(),
        },
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Beeper { .. },
        } => {
            return Err(CliError::Usage(
                "beeper auth runs the token helper, not a daemon request".into(),
            ));
        }
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Gcal { .. },
        } => {
            return Err(CliError::Usage(
                "gcal auth runs the consent helper, not a daemon request".into(),
            ));
        }
        #[cfg(feature = "human")]
        Command::Tag {
            cmd: TagCmd::AddReminder { reminder, tag },
        } => RequestBody::ReminderTagAdd {
            reminder_id: reminder.clone(),
            tag: tag.clone(),
        },
        #[cfg(feature = "human")]
        Command::Tag {
            cmd: TagCmd::RemoveReminder { reminder, tag },
        } => RequestBody::ReminderTagRemove {
            reminder_id: reminder.clone(),
            tag: tag.clone(),
        },
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Reminders {
                cmd: RemindersCmd::Status,
            },
        } => RequestBody::RemindersStatus,
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Reminders {
                cmd: RemindersCmd::Auth,
            },
        } => {
            return Err(CliError::Usage(
                "reminders auth runs the helper from your terminal, not a daemon request".into(),
            ));
        }
        #[cfg(feature = "human")]
        Command::Agent {
            cmd: AgentCmd::Register {
                id, kind, allow, ..
            },
        } => RequestBody::RegisterPrincipal {
            principal_id: id.clone(),
            kind: haste_core::policy::PrincipalKind::parse(kind)
                .ok_or_else(|| CliError::Usage(format!("unknown kind {kind}")))?,
            allowed_actions: allow.clone(),
        },
        #[cfg(feature = "human")]
        Command::Token {
            cmd: TokenCmd::Revoke { id },
        } => RequestBody::RevokeToken {
            principal_id: id.clone(),
        },
        #[cfg(feature = "human")]
        Command::Tag {
            cmd: TagCmd::Add { thread, tag },
        } => RequestBody::TagAdd {
            thread_id: thread.clone(),
            tag: tag.clone(),
        },
        #[cfg(feature = "human")]
        Command::Tag {
            cmd: TagCmd::Remove { thread, tag },
        } => RequestBody::TagRemove {
            thread_id: thread.clone(),
            tag: tag.clone(),
        },
        #[cfg(feature = "human")]
        Command::Tag { cmd: TagCmd::List } => RequestBody::TagList,
        #[cfg(feature = "human")]
        Command::Machine {
            cmd: MachineCmd::Status,
        } => RequestBody::AgentList,
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Status,
        } => RequestBody::ConnectorStatus,
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Release { source },
        } => RequestBody::ConnectorRelease {
            source: source.clone(),
        },
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Engage { source },
        } => RequestBody::ConnectorEngage {
            source: source.clone(),
        },
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Gmail { .. },
        } => {
            return Err(CliError::Usage(
                "gmail auth runs the consent helper, not a daemon request".into(),
            ));
        }
        #[cfg(feature = "human")]
        Command::Connector {
            cmd: ConnectorCmd::Icloud { .. },
        } => {
            return Err(CliError::Usage(
                "icloud auth runs the authorization helper, not a daemon request".into(),
            ));
        }
    })
}

/// The text of a note: `--text`, or all of stdin.
fn note_text(text: Option<&str>, stdin: bool) -> Result<String, CliError> {
    if stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| CliError::Usage(e.to_string()))?;
        Ok(buf)
    } else {
        text.map(str::to_owned)
            .ok_or_else(|| CliError::Usage("give --text or --stdin".into()))
    }
}

#[must_use]
pub fn base_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(DEFAULT_BASE)
}

fn valid_hint(hint: &str) -> bool {
    let mut chars = hint.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && hint.len() <= HINT_MAX_LEN
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Find the token to present. The environment only picks *which file* to read; the
/// daemon decides identity from the token itself.
///
/// # Errors
/// [`CliError::MissingToken`] when no token file is found. Never falls back to another
/// agent's token or to the human token.
pub fn load_token(
    base: &Path,
    hint: Option<&str>,
    token_file: Option<&Path>,
    human: bool,
) -> Result<String, CliError> {
    let path = if human {
        base.join("human.token")
    } else if let Some(f) = token_file {
        f.to_path_buf()
    } else {
        let hint = hint.ok_or_else(|| {
            CliError::MissingToken(format!("set {AGENT_HINT_ENV} or {TOKEN_FILE_ENV}"))
        })?;
        if !valid_hint(hint) {
            return Err(CliError::MissingToken("invalid agent hint".into()));
        }
        base.join("tokens").join(hint)
    };
    fs::read_to_string(&path)
        .map(|t| t.trim().to_owned())
        .map_err(|_| CliError::MissingToken(format!("no token at {}", path.display())))
}

/// Send one request to the daemon and return its response.
///
/// # Errors
/// [`CliError::Connect`] when the socket cannot be used.
pub fn call(base: &Path, request: &Request) -> Result<Response, CliError> {
    let socket = base.join("run").join("hasted.sock");
    let mut stream = UnixStream::connect(&socket).map_err(|e| {
        CliError::Connect(format!("cannot reach daemon at {}: {e}", socket.display()))
    })?;
    write_frame(&mut stream, request).map_err(|e| CliError::Connect(format!("{e:?}")))?;
    read_frame(&mut stream).map_err(|e| CliError::Connect(format!("{e:?}")))
}

/// Run a parsed command; returns pretty JSON for stdout.
///
/// # Errors
/// Any [`CliError`], including daemon-reported errors.
pub fn run(cli: &Cli) -> Result<String, CliError> {
    let base = base_dir(cli.home.as_deref());
    #[cfg(feature = "human")]
    if let Command::Connector {
        cmd:
            ConnectorCmd::Gmail {
                cmd:
                    GmailCmd::Auth {
                        client_id,
                        client_secret,
                    },
            },
    } = &cli.command
    {
        return run_gmail_auth(&base, client_id, client_secret);
    }
    #[cfg(feature = "human")]
    if let Command::Connector {
        cmd:
            ConnectorCmd::Icloud {
                cmd:
                    IcloudCmd::Auth {
                        address,
                        folders,
                        backfill,
                    },
            },
    } = &cli.command
    {
        return run_icloud_auth(&base, address.as_deref(), folders, *backfill);
    }
    #[cfg(feature = "human")]
    if let Command::Connector {
        cmd: ConnectorCmd::Reminders {
            cmd: RemindersCmd::Auth,
        },
    } = &cli.command
    {
        return run_reminders_auth(&base);
    }
    #[cfg(feature = "human")]
    if let Command::Connector {
        cmd: ConnectorCmd::Gcal { cmd: GcalCmd::Auth },
    } = &cli.command
    {
        return run_gcal_auth(&base);
    }
    #[cfg(feature = "human")]
    if let Command::Connector {
        cmd: ConnectorCmd::Beeper {
            cmd: BeeperCmd::Auth,
        },
    } = &cli.command
    {
        return run_beeper_auth(&base);
    }
    let human = is_human_command(&cli.command);
    let hint = std::env::var(AGENT_HINT_ENV).ok();
    let file = std::env::var_os(TOKEN_FILE_ENV).map(PathBuf::from);
    let token = load_token(&base, hint.as_deref(), file.as_deref(), human)?;
    #[cfg(feature = "human")]
    let body = match &cli.command {
        // no title given: use the thread subject
        Command::Email {
            cmd:
                EmailCmd::ToReminder {
                    thread,
                    title: None,
                    project,
                    due,
                    list,
                    key,
                },
        } => {
            let subject = thread_subject(&base, &token, thread)?;
            build_body(&Command::Email {
                cmd: EmailCmd::ToReminder {
                    thread: thread.clone(),
                    title: Some(subject),
                    project: project.clone(),
                    due: *due,
                    list: list.clone(),
                    key: key.clone(),
                },
            })?
        }
        other => build_body(other)?,
    };
    #[cfg(not(feature = "human"))]
    let body = build_body(&cli.command)?;
    let response = call(&base, &Request::new(&token, body))?;
    #[cfg(feature = "human")]
    if let (
        Command::Agent {
            cmd: AgentCmd::Register {
                write_token: true, ..
            },
        },
        Response::Registered {
            principal_id,
            token,
        },
    ) = (&cli.command, &response)
    {
        let path = write_token_file(&base, principal_id, token)?;
        return Ok(format!(
            "token for {principal_id} written to {}",
            path.display()
        ));
    }
    let text =
        serde_json::to_string_pretty(&response).map_err(|e| CliError::Daemon(e.to_string()))?;
    match response {
        Response::Error { error } => Err(CliError::Daemon(format!(
            "{:?}: {}",
            error.code, error.message
        ))),
        _ => Ok(text),
    }
}

/// Name of the helper that runs the consent flow. It is a separate binary so this CLI does
/// not link an HTTP client or Keychain code.
#[cfg(feature = "human")]
pub const GMAIL_AUTH_HELPER: &str = "haste-gmail-auth";

/// Run the Gmail consent helper (next to this executable, else on `PATH`). Secrets are
/// passed through the environment, not the argument list.
///
/// # Errors
/// [`CliError::Usage`] when the helper cannot be started or fails.
#[cfg(feature = "human")]
pub fn run_gmail_auth(
    base: &Path,
    client_id: &str,
    client_secret: &str,
) -> Result<String, CliError> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(GMAIL_AUTH_HELPER)));
    let program = beside
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(GMAIL_AUTH_HELPER));
    let status = std::process::Command::new(&program)
        .arg("--home")
        .arg(base)
        .env("HASTE_GMAIL_CLIENT_ID", client_id)
        .env("HASTE_GMAIL_CLIENT_SECRET", client_secret)
        .status()
        .map_err(|e| CliError::Usage(format!("cannot start {GMAIL_AUTH_HELPER}: {e} (build it with `cargo build -p haste-connector-gmail`)")))?;
    if status.success() {
        Ok("gmail authorization finished".to_owned())
    } else {
        Err(CliError::Usage(format!(
            "{GMAIL_AUTH_HELPER} failed with {status}"
        )))
    }
}

/// Name of the helper that stores the Beeper API token.
#[cfg(feature = "human")]
pub const BEEPER_AUTH_HELPER: &str = "haste-beeper-auth";

/// Run the Beeper token helper (next to this executable, else on `PATH`). The helper prompts for
/// the token on the terminal itself, so it never passes through this process.
///
/// # Errors
/// [`CliError::Usage`] when the helper cannot be started or fails.
#[cfg(feature = "human")]
pub fn run_beeper_auth(base: &Path) -> Result<String, CliError> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(BEEPER_AUTH_HELPER)));
    let program = beside
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(BEEPER_AUTH_HELPER));
    let status = std::process::Command::new(&program)
        .arg("--home")
        .arg(base)
        .status()
        .map_err(|e| {
            CliError::Usage(format!(
                "cannot start {BEEPER_AUTH_HELPER}: {e} (build it with `cargo build -p haste-connector-beeper`)"
            ))
        })?;
    if status.success() {
        Ok("beeper authorization finished".to_owned())
    } else {
        Err(CliError::Usage(format!(
            "{BEEPER_AUTH_HELPER} failed with {status}"
        )))
    }
}

/// Name of the helper that runs the Google Calendar consent flow.
#[cfg(feature = "human")]
pub const GCAL_AUTH_HELPER: &str = "haste-gcal-auth";

/// Run the Calendar consent helper (next to this executable, else on `PATH`). It reads the OAuth
/// client from the environment and never prints a token.
///
/// # Errors
/// [`CliError::Usage`] when the helper cannot be started or fails.
#[cfg(feature = "human")]
pub fn run_gcal_auth(base: &Path) -> Result<String, CliError> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(GCAL_AUTH_HELPER)));
    let program = beside
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(GCAL_AUTH_HELPER));
    let status = std::process::Command::new(&program)
        .arg("--home")
        .arg(base)
        .status()
        .map_err(|e| {
            CliError::Usage(format!(
                "cannot start {GCAL_AUTH_HELPER}: {e} (build it with `cargo build -p haste-connector-gcal`)"
            ))
        })?;
    if status.success() {
        Ok("gcal authorization finished".to_owned())
    } else {
        Err(CliError::Usage(format!(
            "{GCAL_AUTH_HELPER} failed with {status}"
        )))
    }
}

/// Where the Reminders helper is: `$HASTE_REMINDERS_HELPER`, next to this executable, or in
/// the data directory.
#[cfg(feature = "human")]
fn reminders_helper_path(base: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HASTE_REMINDERS_HELPER") {
        return Some(PathBuf::from(p));
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("haste-reminders-helper")));
    [beside, Some(base.join("haste-reminders-helper"))]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

/// Ask macOS for Reminders access by running the helper from this terminal, so the system
/// prompt names the terminal. Prints the resulting status only.
///
/// # Errors
/// [`CliError::Usage`] when the helper is missing or fails.
#[cfg(feature = "human")]
pub fn run_reminders_auth(base: &Path) -> Result<String, CliError> {
    use std::io::Write;
    let program = reminders_helper_path(base).ok_or_else(|| {
        CliError::Usage(
            "reminders helper not found; build it with crates/haste-connector-reminders/helper/build.sh"
                .into(),
        )
    })?;
    let mut child = std::process::Command::new(&program)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Usage(format!("cannot start the reminders helper: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(br#"{"op":"authorization","prompt":true}"#);
    }
    let out = child
        .wait_with_output()
        .map_err(|e| CliError::Usage(e.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| CliError::Usage("the reminders helper gave no answer".into()))?;
    let status = value["status"].as_str().unwrap_or("unknown");
    if status == "granted" {
        Ok("reminders access: granted".to_owned())
    } else {
        Err(CliError::Usage(format!(
            "reminders access: {status} (System Settings > Privacy & Security > Reminders)"
        )))
    }
}

/// The subject of a thread, used as the default reminder title.
#[cfg(feature = "human")]
fn thread_subject(base: &Path, token: &str, thread: &str) -> Result<String, CliError> {
    let response = call(
        base,
        &Request::new(
            token,
            RequestBody::ThreadShow {
                thread_id: thread.to_owned(),
            },
        ),
    )?;
    match response {
        Response::Thread { view, .. } => Ok(view.thread.subject),
        Response::Error { error } => Err(CliError::Daemon(error.message)),
        _ => Err(CliError::Daemon("unexpected response".into())),
    }
}

/// Name of the helper that validates and stores the iCloud app-specific password.
#[cfg(feature = "human")]
pub const ICLOUD_AUTH_HELPER: &str = "haste-icloud-auth";

/// Run the iCloud authorization helper (next to this executable, else on `PATH`). The helper
/// itself prompts for the password on the terminal, so it never passes through this process.
///
/// # Errors
/// [`CliError::Usage`] when the helper cannot be started or fails.
#[cfg(feature = "human")]
pub fn run_icloud_auth(
    base: &Path,
    address: Option<&str>,
    folders: &[String],
    backfill: Option<usize>,
) -> Result<String, CliError> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(ICLOUD_AUTH_HELPER)));
    let program = beside
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(ICLOUD_AUTH_HELPER));
    let mut command = std::process::Command::new(&program);
    command.arg("--home").arg(base);
    if let Some(a) = address {
        command.arg("--address").arg(a);
    }
    for f in folders {
        command.arg("--folder").arg(f);
    }
    if let Some(n) = backfill {
        command.arg("--backfill").arg(n.to_string());
    }
    let status = command.status().map_err(|e| {
        CliError::Usage(format!(
            "cannot start {ICLOUD_AUTH_HELPER}: {e} (build it with `cargo build -p haste-connector-icloud`)"
        ))
    })?;
    if status.success() {
        Ok("icloud authorization finished".to_owned())
    } else {
        Err(CliError::Usage(format!(
            "{ICLOUD_AUTH_HELPER} failed with {status}"
        )))
    }
}

/// Store an issued token under `<base>/tokens/<id>` with mode `0600`.
///
/// # Errors
/// [`CliError::Usage`] when the id is not a valid hint or the file cannot be written.
#[cfg(feature = "human")]
pub fn write_token_file(base: &Path, principal_id: &str, token: &str) -> Result<PathBuf, CliError> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if !valid_hint(principal_id) {
        return Err(CliError::Usage(
            "principal id must match [a-z][a-z0-9_-]{0,31} to be stored as a token file".into(),
        ));
    }
    let dir = base.join("tokens");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| CliError::Usage(e.to_string()))?;
    let path = dir.join(principal_id);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| CliError::Usage(e.to_string()))?;
    f.write_all(token.as_bytes())
        .map_err(|e| CliError::Usage(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn names(cmd: &clap::Command) -> Vec<String> {
        cmd.get_subcommands()
            .map(|c| c.get_name().to_owned())
            .collect()
    }

    #[test]
    fn send_verb_is_not_recognized_anywhere() {
        assert!(Cli::try_parse_from(["haste", "send", "p1"]).is_err());
        let root = Cli::command();
        assert!(!names(&root).iter().any(|n| n == "send"));
        for sub in root.get_subcommands() {
            assert!(
                !names(sub).iter().any(|n| n == "send"),
                "send under {}",
                sub.get_name()
            );
        }
    }

    #[cfg(not(feature = "human"))]
    #[test]
    fn agent_build_has_no_human_verbs() {
        for argv in [
            ["haste", "proposal", "approve", "p1"],
            ["haste", "proposal", "reject", "p1"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
        assert!(Cli::try_parse_from(["haste", "agent", "register", "x"]).is_err());
        assert!(Cli::try_parse_from(["haste", "token", "revoke", "x"]).is_err());
    }

    #[cfg(feature = "human")]
    #[test]
    fn human_build_has_human_verbs_but_still_no_send() {
        let cli =
            Cli::try_parse_from(["haste", "proposal", "approve", "p1", "--ack-stale"]).unwrap();
        assert!(is_human_command(&cli.command));
        assert!(matches!(
            build_body(&cli.command).unwrap(),
            RequestBody::Approve {
                acknowledge_stale: true,
                ..
            }
        ));
        assert!(Cli::try_parse_from(["haste", "send", "p1"]).is_err());
    }

    #[test]
    fn agents_can_propose_reminders_from_a_thread_but_not_execute_or_tag() {
        let cli = Cli::try_parse_from([
            "haste",
            "reminder",
            "create",
            "--title",
            "Call",
            "--from-thread",
            "th1",
            "--key",
            "k",
        ])
        .unwrap();
        assert!(!is_human_command(&cli.command));
        match build_body(&cli.command).unwrap() {
            RequestBody::Propose {
                thread_id,
                idempotency_key,
                payload: Payload::ReminderCreate { title, .. },
                ..
            } => {
                assert_eq!(thread_id.as_deref(), Some("th1"));
                assert_eq!((idempotency_key.as_str(), title.as_str()), ("k", "Call"));
            }
            other => panic!("{other:?}"),
        }
        let cli = Cli::try_parse_from(["haste", "reminder", "complete", "r1"]).unwrap();
        assert!(matches!(
            build_body(&cli.command).unwrap(),
            RequestBody::Propose {
                payload: Payload::ReminderComplete { .. },
                ..
            }
        ));
        #[cfg(not(feature = "human"))]
        for argv in [
            vec!["haste", "proposal", "execute", "p1"],
            vec!["haste", "proposal", "resolve", "p1"],
            vec!["haste", "tag", "add-reminder", "r1", "haste"],
            vec!["haste", "email", "to-reminder", "t1"],
            vec!["haste", "connector", "reminders", "auth"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
    }

    #[test]
    fn agents_can_write_person_and_reference_notes_but_not_read_log_or_set_modes() {
        let cli = Cli::try_parse_from([
            "haste", "note", "person", "p1", "--text", "hi", "--key", "k",
        ])
        .unwrap();
        assert!(!is_human_command(&cli.command));
        assert!(matches!(
            build_body(&cli.command).unwrap(),
            RequestBody::NotePerson { person_id, idempotency_key, .. } if person_id == "p1" && idempotency_key == "k"
        ));
        let cli = Cli::try_parse_from([
            "haste",
            "note",
            "ref",
            "thread",
            "t1",
            "--topic",
            "Billing",
            "--project",
            "My Project",
        ])
        .unwrap();
        match build_body(&cli.command).unwrap() {
            RequestBody::NoteReference(a) => {
                assert_eq!((a.id.as_str(), a.topic.as_str()), ("t1", "Billing"));
                assert_eq!(a.project.as_deref(), Some("My Project"));
            }
            other => panic!("{other:?}"),
        }
        // a reference has no text option, and unknown kinds are refused before any request
        assert!(Cli::try_parse_from([
            "haste", "note", "ref", "thread", "t1", "--topic", "x", "--text", "y"
        ])
        .is_err());
        let bad_kind =
            Cli::try_parse_from(["haste", "note", "ref", "message", "m1", "--topic", "x"]).unwrap();
        assert!(build_body(&bad_kind.command).is_err());
        #[cfg(not(feature = "human"))]
        for argv in [
            vec!["haste", "note", "show", "people/ana.md"],
            vec!["haste", "note", "log"],
            vec!["haste", "note", "mode", "agent-a", "approval"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
    }

    #[cfg(feature = "human")]
    #[test]
    fn human_note_verbs_map_to_human_requests() {
        for argv in [
            vec!["haste", "note", "show", "people/ana.md"],
            vec!["haste", "note", "log", "--since", "5"],
            vec!["haste", "note", "mode", "agent-a", "approval"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(is_human_command(&cli.command));
            assert!(build_body(&cli.command).is_ok());
        }
        let bad = Cli::try_parse_from(["haste", "note", "mode", "agent-a", "sometimes"]).unwrap();
        assert!(build_body(&bad.command).is_err());
    }

    #[test]
    fn agents_can_propose_events_but_have_no_attendee_or_status_flags() {
        let cli = Cli::try_parse_from([
            "haste",
            "event",
            "create",
            "--calendar",
            "primary",
            "--title",
            "Sync",
            "--start",
            "100",
            "--end",
            "200",
            "--attendee",
            "a@x.org",
            "--key",
            "k",
        ])
        .unwrap();
        assert!(!is_human_command(&cli.command));
        match build_body(&cli.command).unwrap() {
            RequestBody::Propose {
                payload:
                    Payload::CalendarCreate {
                        attendees, title, ..
                    },
                idempotency_key,
                ..
            } => {
                assert_eq!((title.as_str(), idempotency_key.as_str()), ("Sync", "k"));
                assert_eq!(attendees, ["a@x.org"]);
            }
            other => panic!("{other:?}"),
        }
        let cli =
            Cli::try_parse_from(["haste", "event", "update", "e1", "--title", "New"]).unwrap();
        match build_body(&cli.command).unwrap() {
            RequestBody::Propose {
                payload: Payload::CalendarUpdate { changes, .. },
                ..
            } => {
                assert_eq!(changes.title.as_deref(), Some("New"));
                assert!(changes.attendees.is_none() && changes.status.is_none());
            }
            other => panic!("{other:?}"),
        }
        // the update verb has no way to name attendees, a status, or a delete
        for argv in [
            vec!["haste", "event", "update", "e1", "--attendee", "a@x.org"],
            vec!["haste", "event", "update", "e1", "--status", "cancelled"],
            vec!["haste", "event", "delete", "e1"],
            vec!["haste", "event", "cancel", "e1"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
        #[cfg(not(feature = "human"))]
        for argv in [
            vec!["haste", "connector", "gcal", "auth"],
            vec!["haste", "tag", "add-event", "e1", "haste"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
    }

    #[cfg(feature = "human")]
    #[test]
    fn human_calendar_verbs_map_to_human_requests() {
        for argv in [
            vec!["haste", "tag", "add-event", "e1", "haste"],
            vec!["haste", "tag", "remove-event", "e1", "haste"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(is_human_command(&cli.command));
            assert!(build_body(&cli.command).is_ok());
        }
        let cli = Cli::try_parse_from(["haste", "connector", "gcal", "auth"]).unwrap();
        assert!(is_human_command(&cli.command));
    }

    #[cfg(feature = "human")]
    #[test]
    fn human_reminder_verbs_map_to_human_requests() {
        for (argv, human) in [
            (
                vec!["haste", "proposal", "execute", "p1", "--ack-stale"],
                true,
            ),
            (vec!["haste", "proposal", "resolve", "p1", "--done"], true),
            (vec!["haste", "tag", "add-reminder", "r1", "haste"], true),
            (
                vec!["haste", "email", "to-reminder", "t1", "--title", "x"],
                true,
            ),
            (vec!["haste", "connector", "reminders", "status"], true),
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert_eq!(is_human_command(&cli.command), human);
        }
        let cli =
            Cli::try_parse_from(["haste", "email", "to-reminder", "t1", "--title", "x"]).unwrap();
        match build_body(&cli.command).unwrap() {
            RequestBody::Propose {
                thread_id,
                idempotency_key,
                ..
            } => {
                assert_eq!(thread_id.as_deref(), Some("t1"));
                assert_eq!(
                    idempotency_key, "to-reminder-t1",
                    "re-running is idempotent"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[cfg(not(feature = "human"))]
    #[test]
    fn agent_build_has_no_connector_command() {
        for argv in [
            vec!["haste", "connector", "icloud", "auth"],
            vec!["haste", "connector", "status"],
            vec!["haste", "connector", "release", "gmail"],
            vec!["haste", "connector", "engage", "gmail"],
            vec!["haste", "connector", "gmail", "auth"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
        assert!(!names(&Cli::command()).iter().any(|n| n == "connector"));
    }

    #[cfg(feature = "human")]
    #[test]
    fn human_build_maps_connector_verbs_to_admin_requests() {
        let cases = [
            (
                vec!["haste", "connector", "status"],
                RequestBody::ConnectorStatus,
            ),
            (
                vec!["haste", "connector", "release", "gmail"],
                RequestBody::ConnectorRelease {
                    source: "gmail".into(),
                },
            ),
            (
                vec!["haste", "connector", "engage", "gmail"],
                RequestBody::ConnectorEngage {
                    source: "gmail".into(),
                },
            ),
        ];
        for (argv, expected) in cases {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(is_human_command(&cli.command));
            assert_eq!(build_body(&cli.command).unwrap(), expected);
        }
        let icloud = Cli::try_parse_from([
            "haste",
            "connector",
            "icloud",
            "auth",
            "--address",
            "me@icloud.com",
            "--folder",
            "INBOX",
            "--folder",
            "Sent Messages",
            "--backfill",
            "500",
        ])
        .unwrap();
        match icloud.command {
            Command::Connector {
                cmd:
                    ConnectorCmd::Icloud {
                        cmd:
                            IcloudCmd::Auth {
                                address,
                                folders,
                                backfill,
                            },
                    },
            } => {
                assert_eq!(address.as_deref(), Some("me@icloud.com"));
                assert_eq!(folders, vec!["INBOX", "Sent Messages"]);
                assert_eq!(backfill, Some(500));
            }
            other => panic!("{other:?}"),
        }
        let auth = Cli::try_parse_from([
            "haste",
            "connector",
            "gmail",
            "auth",
            "--client-id",
            "a",
            "--client-secret",
            "b",
        ])
        .unwrap();
        assert!(matches!(
            auth.command,
            Command::Connector {
                cmd: ConnectorCmd::Gmail { .. }
            }
        ));
    }

    #[test]
    fn draft_parses_into_a_proposal_request() {
        let cli = Cli::try_parse_from([
            "haste", "draft", "--thread", "t1", "--body", "hi", "--key", "k",
        ])
        .unwrap();
        match build_body(&cli.command).unwrap() {
            RequestBody::Propose {
                payload: Payload::Draft { body, .. },
                thread_id,
                idempotency_key,
                ..
            } => {
                assert_eq!(body, "hi");
                assert_eq!(thread_id.as_deref(), Some("t1"));
                assert_eq!(idempotency_key, "k");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hints_need_a_pane_and_default_the_machine_to_local() {
        assert_eq!(
            hints_from(None, None),
            None,
            "outside herdr there is nothing to look up"
        );
        assert_eq!(hints_from(Some("m1"), None), None);
        assert_eq!(hints_from(None, Some("  ")), None);
        let h = hints_from(None, Some("w1:p1")).unwrap();
        assert_eq!(
            (h.machine.as_deref(), h.pane.as_deref()),
            (Some("local"), Some("w1:p1"))
        );
        let h = hints_from(Some(" m1 "), Some("w2:p3")).unwrap();
        assert_eq!(
            (h.machine.as_deref(), h.pane.as_deref()),
            (Some("m1"), Some("w2:p3"))
        );
    }

    #[cfg(not(feature = "human"))]
    #[test]
    fn agent_build_has_no_tag_or_machine_commands() {
        for argv in [
            vec!["haste", "tag", "list"],
            vec!["haste", "tag", "add", "t1", "haste"],
            vec!["haste", "tag", "remove", "t1", "haste"],
            vec!["haste", "machine", "status"],
        ] {
            assert!(Cli::try_parse_from(argv).is_err());
        }
        let root = Cli::command();
        assert!(!names(&root).iter().any(|n| n == "tag" || n == "machine"));
    }

    #[cfg(feature = "human")]
    #[test]
    fn human_build_maps_tag_and_machine_verbs_to_requests() {
        let cases = [
            (vec!["haste", "tag", "list"], RequestBody::TagList),
            (
                vec!["haste", "tag", "add", "t1", "haste"],
                RequestBody::TagAdd {
                    thread_id: "t1".into(),
                    tag: "haste".into(),
                },
            ),
            (
                vec!["haste", "tag", "remove", "t1", "haste"],
                RequestBody::TagRemove {
                    thread_id: "t1".into(),
                    tag: "haste".into(),
                },
            ),
            (vec!["haste", "machine", "status"], RequestBody::AgentList),
        ];
        for (argv, expected) in cases {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(is_human_command(&cli.command));
            assert_eq!(build_body(&cli.command).unwrap(), expected);
        }
    }

    #[test]
    fn snooze_needs_exactly_one_time_spec() {
        assert!(
            Cli::try_parse_from(["haste", "snooze", "t1", "--hours", "2", "--days", "1"]).is_err()
        );
        let cli = Cli::try_parse_from(["haste", "snooze", "t1", "--until", "42"]).unwrap();
        assert!(matches!(
            build_body(&cli.command).unwrap(),
            RequestBody::Propose {
                payload: Payload::Snooze { until: 42 },
                ..
            }
        ));
    }

    #[test]
    fn omitted_key_is_generated() {
        let cli = Cli::try_parse_from(["haste", "archive", "t1"]).unwrap();
        let RequestBody::Propose {
            idempotency_key, ..
        } = build_body(&cli.command).unwrap()
        else {
            panic!()
        };
        assert!(!idempotency_key.is_empty());
    }

    #[test]
    fn env_hint_selects_a_file_but_never_acts_as_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("tokens")).unwrap();
        fs::write(dir.path().join("tokens/agent-a"), "TOKEN-A\n").unwrap();
        // A forged hint pointing at a missing agent fails; it never falls back to another token.
        assert!(matches!(
            load_token(dir.path(), Some("agent-b"), None, false),
            Err(CliError::MissingToken(_))
        ));
        // The hint only chooses the file; the value presented is the file's content.
        assert_eq!(
            load_token(dir.path(), Some("agent-a"), None, false).unwrap(),
            "TOKEN-A"
        );
        // Traversal in the hint is rejected.
        assert!(load_token(dir.path(), Some("../human"), None, false).is_err());
        // No hint and no file: no token.
        assert!(load_token(dir.path(), None, None, false).is_err());
    }
}
