use std::io;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use haste_core::proposal::ProposalState;
use haste_proto::{ErrorCode, RequestBody, Response};
use haste_tui::app::{App, Effect, Key, TriageChange};
use haste_tui::client::{base_dir, Client};
use haste_tui::ui::draw;

const POLL: Duration = Duration::from_millis(200);

fn map_key(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
    Some(match code {
        // Ctrl-Shift-q is told apart from Ctrl-q where the terminal reports the Shift
        KeyCode::Char(c)
            if mods.contains(KeyModifiers::CONTROL)
                && c.eq_ignore_ascii_case(&'q')
                && (c == 'Q' || mods.contains(KeyModifiers::SHIFT)) =>
        {
            Key::Ctrl('Q')
        }
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
            Key::Ctrl(c.to_ascii_lowercase())
        }
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::BackTab => Key::BackTab,
        // some terminals send Shift-Tab as a Tab with the Shift modifier
        KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => Key::BackTab,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        // Alt-Enter reaches every terminal, so it works where Ctrl-Enter is not reported
        KeyCode::Enter if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            Key::CtrlEnter
        }
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Down => Key::Down,
        KeyCode::Up => Key::Up,
        _ => return None,
    })
}

/// The sender rule files (mute, priority, allowed), read again by the daemon on every call.
fn load_rules(app: &mut App, client: &Client) {
    if let Ok(Response::Rules {
        mute,
        priority,
        allowed,
        errors,
    }) = client.call(RequestBody::SenderRules)
    {
        app.set_rules(&mute, &priority, &allowed, errors);
    }
}

/// `30 min`, `1 h`, `2 h`.
fn inbox_minutes(minutes: u32) -> String {
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{} h", minutes / 60)
    }
}

/// The TODO list again from the daemon.
fn load_reminders(app: &mut App, client: &Client) {
    if let Ok(Response::Reminders { reminders }) = client.call(RequestBody::RemindersList {
        include_completed: false,
    }) {
        app.set_reminder_list(reminders);
    }
}

/// How often the TODO list is read again while the TUI is open: a change made in the
/// Reminders app, or one the daemon finished syncing, shows up without a restart.
const REMINDERS_EVERY: Duration = Duration::from_secs(20);

/// How often the Logs screen reads the daemon's log again while it is open.
const LOGS_EVERY: Duration = Duration::from_secs(1);

fn refresh(app: &mut App, client: &Client) {
    app.refresh_tz();
    match client.call(RequestBody::ProposalList {
        states: vec![
            ProposalState::Pending,
            ProposalState::Approved,
            ProposalState::Executing,
        ],
    }) {
        Ok(Response::Proposals { proposals }) => app.set_proposals(proposals),
        Ok(other) => {
            app.status = describe(Ok(other))
                .err()
                .unwrap_or_else(|| "unexpected reply".into());
        }
        Err(e) => app.status = e,
    }
    if let Ok(Response::Scheduled { items }) = client.call(RequestBody::ScheduledList) {
        app.set_scheduled(items);
    }
    match client.call(RequestBody::AgentList) {
        Ok(Response::Agents { machines }) => app.set_machines(machines),
        Ok(_) => {}
        Err(e) => app.status = e,
    }
    match client.call(RequestBody::ConnectorStatus) {
        Ok(Response::Connectors { connectors }) => app.set_connectors(connectors),
        Ok(_) => {}
        Err(e) => app.status = e,
    }
    if let Ok(Response::RemindersStatus { status }) = client.call(RequestBody::RemindersStatus) {
        app.set_reminders(status);
    }
    load_rules(app, client);
    if let Ok(Response::Tags { tags }) = client.call(RequestBody::TagList) {
        app.known_labels = tags;
    }
    if let Ok(Response::Filters { filters, error }) = client.call(RequestBody::FilterList) {
        app.set_filters(filters, error);
    }
    if let Ok(Response::Events { events }) = client.call(RequestBody::EventsList {
        // from Monday, so the days already gone this week are drawn too
        from: haste_tui::inbox::week_start(haste_core::time::now_secs(), app.tz_offset, 0),
    }) {
        app.set_events(events);
    }
    load_reminders(app, client);
    match client.call(RequestBody::ThreadList {
        sources: vec![],
        include_archived: true,
        include_snoozed: true,
    }) {
        Ok(Response::Threads { threads }) => app.set_threads(threads),
        Ok(_) => {}
        Err(e) => app.status = e,
    }
}

/// Complete one reminder as the human: propose, approve and run, through the same audited steps
/// an approval in the Approvals view would take.
fn complete_reminder(client: &Client, id: &str) -> Result<(), String> {
    let proposal = match client.call(RequestBody::Propose {
        payload: haste_core::proposal::Payload::ReminderComplete {
            reminder_id: id.to_owned(),
            project: None,
            reminder_version: None,
        },
        thread_id: None,
        idempotency_key: format!("tui-done-{id}"),
        hints: None,
    }) {
        Ok(Response::Proposal { proposal }) => proposal,
        other => {
            return Err(describe(other)
                .err()
                .unwrap_or_else(|| "no proposal".into()))
        }
    };
    // a retry returns the same proposal, which may already be approved: only the run counts
    let _ = client.call(RequestBody::Approve {
        proposal_id: proposal.id.clone(),
        acknowledge_stale: false,
    });
    describe(client.call(RequestBody::Execute {
        proposal_id: proposal.id,
        acknowledge_stale: false,
    }))
}

/// Add an event to a Google calendar as the human: propose, approve and run, through the same
/// audited steps as any other calendar create. Guests are never emailed.
fn create_google_event(
    client: &Client,
    calendar: String,
    title: String,
    start: i64,
    end: i64,
) -> Result<(), String> {
    let proposal = match client.call(RequestBody::Propose {
        payload: haste_core::proposal::Payload::CalendarCreate {
            calendar,
            title,
            start,
            end,
            all_day: false,
            attendees: Vec::new(),
            description: None,
            project: None,
        },
        thread_id: None,
        idempotency_key: format!("tui-event-{}-{start}", haste_core::time::now_secs()),
        hints: None,
    }) {
        Ok(Response::Proposal { proposal }) => proposal,
        other => {
            return Err(describe(other)
                .err()
                .unwrap_or_else(|| "no proposal".into()))
        }
    };
    describe(client.call(RequestBody::Approve {
        proposal_id: proposal.id.clone(),
        acknowledge_stale: false,
    }))?;
    describe(client.call(RequestBody::Execute {
        proposal_id: proposal.id,
        acknowledge_stale: false,
    }))
}

/// `<data>/labels_recent`: one label per line, the last used first.
fn recent_labels() -> Vec<String> {
    std::fs::read_to_string(base_dir().join("labels_recent"))
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn remember_label(label: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let list = haste_tui::compose::bump_recent(recent_labels(), label);
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(base_dir().join("labels_recent"))
        .and_then(|mut f| f.write_all(list.join("\n").as_bytes()));
}

fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(max).collect();
    if out.is_empty() {
        "thread".into()
    } else {
        out
    }
}

/// Copy the chosen files into `<data>/attachments` and describe them for a note's text.
fn keep_attachments(files: &[std::path::PathBuf]) -> Result<Vec<String>, String> {
    let dir = base_dir().join("attachments");
    let stamp = haste_core::time::now_secs();
    files
        .iter()
        .map(|f| {
            haste_tui::compose::store_attachment(&dir, f, stamp)
                .map(|p| format!("attachment: {}", p.display()))
                .map_err(|e| {
                    format!(
                        "{}: {e}",
                        f.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    )
                })
        })
        .collect()
}

fn save_note(
    app: &mut App,
    client: &Client,
    thread_id: String,
    text: String,
    attachments: Vec<std::path::PathBuf>,
) {
    let (subject, tags) = app.thread_facts(&thread_id);
    let lines = match keep_attachments(&attachments) {
        Ok(l) => l,
        Err(e) => {
            app.status = format!("not saved, {e}");
            return;
        }
    };
    let name = slug(&subject, 40);
    let path = match tags.first() {
        Some(tag) => format!("projects/{}/tasks/{name}.md", slug(tag, 40)),
        None => format!("tasks/{name}.md"),
    };
    let mut body = text;
    for l in lines {
        body.push_str(&format!("\n- {l}"));
    }
    body.push_str(&format!("\n- haste:thread/{thread_id}"));
    app.status = match client.call(RequestBody::NoteAppend {
        path: path.clone(),
        text: body,
        idempotency_key: format!("tui-note-{thread_id}-{}", haste_core::time::now_secs()),
    }) {
        Ok(Response::Appended { .. }) => format!("note saved in {path}"),
        other => describe(other)
            .err()
            .map(|e| format!("note not saved: {e}"))
            .unwrap_or_else(|| "note not saved".into()),
    };
}

fn save_todo(
    app: &mut App,
    client: &Client,
    reminder_id: Option<String>,
    title: String,
    due: Option<i64>,
    notes: String,
    attachments: Vec<std::path::PathBuf>,
) {
    let lines = match keep_attachments(&attachments) {
        Ok(l) => l,
        Err(e) => {
            app.status = format!("not saved, {e}");
            return;
        }
    };
    let mut extra = notes;
    for l in lines {
        if !extra.is_empty() {
            extra.push('\n');
        }
        extra.push_str(&l);
    }
    let extra = (!extra.is_empty()).then_some(extra);
    let (request, done) = match reminder_id {
        Some(reminder_id) => (
            RequestBody::ReminderEdit {
                reminder_id,
                title,
                due,
                append_notes: extra,
                repeat: None,
                set_repeat: false,
            },
            "TODO saved",
        ),
        None => (
            RequestBody::ReminderNew {
                title,
                due,
                notes: extra,
                repeat: None,
                tags: Vec::new(),
                thread_id: None,
                minutes: None,
                priority: None,
                early: None,
            },
            "TODO created",
        ),
    };
    app.status = match describe(client.call(request)) {
        Ok(()) => done.into(),
        Err(e) => format!("TODO not saved: {e}"),
    };
}

fn complete_all(app: &mut App, client: &Client, ids: Vec<String>) {
    let total = ids.len();
    let failed: Vec<String> = ids
        .iter()
        .filter_map(|id| complete_reminder(client, id).err())
        .collect();
    app.status = if failed.is_empty() {
        format!("{total} task(s) completed in Reminders")
    } else {
        format!(
            "{} of {total} completed, failed: {}",
            total - failed.len(),
            failed.join("; ")
        )
    };
}

fn describe(response: Result<Response, String>) -> Result<(), String> {
    match response {
        Ok(Response::Error { error }) => Err(match error.code {
            ErrorCode::UnsupportedVersion => {
                "hasted is a different version from this TUI: restart hasted".to_owned()
            }
            ErrorCode::StaleNeedsAcknowledgement => {
                "thread changed; acknowledge to continue".to_owned()
            }
            _ => format!("{:?}: {}", error.code, error.message),
        }),
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Returns `true` when the app should quit.
/// An archive handed to the background worker: the thread and what to say when it worked.
struct ArchiveJob {
    thread_id: String,
    done: &'static str,
}

/// The worker's answer.
struct ArchiveDone {
    thread_id: String,
    done: &'static str,
    result: Result<(), String>,
}

/// Archives run one at a time in the background, so the list stays responsive while a mailbox
/// is slow, and never as several connections to the same provider at once.
fn archive_worker(client: Client) -> (Sender<ArchiveJob>, Receiver<ArchiveDone>) {
    let (job_tx, job_rx) = channel::<ArchiveJob>();
    let (done_tx, done_rx) = channel::<ArchiveDone>();
    std::thread::spawn(move || {
        while let Ok(job) = job_rx.recv() {
            let result = describe(client.call(RequestBody::TriageSet {
                thread_id: job.thread_id.clone(),
                priority: None,
                set_aside: None,
                snoozed_until: None,
                archived: Some(true),
            }));
            if done_tx
                .send(ArchiveDone {
                    thread_id: job.thread_id,
                    done: job.done,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
    (job_tx, done_rx)
}

/// A reaction handed to the background worker.
struct ReactJob {
    message_id: String,
    key: String,
}

/// The worker's answer.
struct ReactDone {
    message_id: String,
    result: Result<(), String>,
}

/// Reactions go out one at a time in the background, so the screen shows them at once and
/// Beeper's slowness never holds the keys. A reaction is never sent twice.
fn react_worker(client: Client) -> (Sender<ReactJob>, Receiver<ReactDone>) {
    let (job_tx, job_rx) = channel::<ReactJob>();
    let (done_tx, done_rx) = channel::<ReactDone>();
    std::thread::spawn(move || {
        while let Ok(job) = job_rx.recv() {
            let result = describe(client.call(RequestBody::ReactionAdd {
                message_id: job.message_id.clone(),
                key: job.key,
            }));
            if done_tx
                .send(ReactDone {
                    message_id: job.message_id,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
    (job_tx, done_rx)
}

/// How often an open reply box asks again for a draft that is still being written.
const AI_POLL_EVERY: Duration = Duration::from_secs(4);

/// A rewrite handed to the background worker (the model can take most of a minute).
struct RewriteJob {
    thread_id: String,
    draft: String,
    mode: haste_core::ai::RewriteMode,
    instruction: Option<String>,
}

/// The worker's answer.
struct RewriteDone {
    result: Result<(haste_core::ai::AiOutcome, Option<String>), String>,
}

/// Rewrites run one at a time in the background, so the screen and the keys stay alive while the
/// local model works. A rewrite is never repeated on its own.
fn rewrite_worker(client: Client) -> (Sender<RewriteJob>, Receiver<RewriteDone>) {
    let (job_tx, job_rx) = channel::<RewriteJob>();
    let (done_tx, done_rx) = channel::<RewriteDone>();
    std::thread::spawn(move || {
        while let Ok(job) = job_rx.recv() {
            let answer = client.call(RequestBody::AiRewrite {
                thread_id: job.thread_id,
                draft: job.draft,
                mode: job.mode,
                instruction: job.instruction,
            });
            let result = match answer {
                Ok(Response::AiRewritten { outcome, text }) => Ok((outcome, text)),
                other => Err(describe(other)
                    .err()
                    .unwrap_or_else(|| "the model gave no answer".into())),
            };
            if done_tx.send(RewriteDone { result }).is_err() {
                break;
            }
        }
    });
    (job_tx, done_rx)
}

/// Ask the daemon for the thread's AI suggestions (it also queues what is missing) and tell the
/// app. Quiet when the helpers are off or the daemon cannot answer.
fn fetch_ai(
    app: &mut App,
    client: &Client,
    thread_id: &str,
) -> Option<(haste_proto::AiState, Option<haste_core::ai::TodoSuggestion>)> {
    match client.call(RequestBody::AiSuggestionGet {
        thread_id: thread_id.to_owned(),
    }) {
        Ok(Response::AiSuggestion {
            draft_state,
            draft,
            todo_state,
            todo,
        }) => {
            app.set_ai(haste_tui::app::AiSuggest {
                thread_id: thread_id.to_owned(),
                draft_state,
                draft,
                todo_state,
                todo: todo.clone(),
            });
            Some((todo_state, todo))
        }
        _ => None,
    }
}

/// Take the thread out of the list now and queue the archive.
fn start_archive(app: &mut App, jobs: &Sender<ArchiveJob>, thread_id: String) {
    let done = if app.thread_is_mail(&thread_id) {
        "archived in the mailbox and here"
    } else {
        "archived (in haste only; chats have no upstream archive)"
    };
    app.begin_archive(&thread_id);
    app.status = "archiving …".into();
    if jobs
        .send(ArchiveJob {
            thread_id: thread_id.clone(),
            done,
        })
        .is_err()
    {
        app.finish_archive(&thread_id, Err("the archive worker stopped".into()));
    }
}

fn run_effect(app: &mut App, client: &Client, effect: Effect) -> bool {
    // opening or reloading a thread, loading older messages or copying text change none of the
    // lists, so they do not ask the daemon for all of them again (over a slow link that is the
    // difference between a thread opening at once and seconds later)
    let changes_no_list = matches!(
        effect,
        Effect::OpenThread { .. }
            | Effect::ReloadThread { .. }
            | Effect::LoadOlder { .. }
            | Effect::CopyText { .. }
            | Effect::OpenAndReply { .. }
    );
    match effect {
        Effect::Background => {
            // hand the screen back to the tab before this one; the TUI stays up. Without the
            // helper (or if it fails) it quits as `q` always did.
            // HASTE_BACKGROUND_COMMAND (a shell command line, e.g. tmux's detach) or, for a Herdr
            // tab of its own, HASTE_HERDR_TOGGLE with --leave
            let mut command = if let Some(line) = std::env::var_os("HASTE_BACKGROUND_COMMAND") {
                let mut c = std::process::Command::new("/bin/sh");
                c.arg("-c").arg(line);
                Some(c)
            } else {
                std::env::var_os("HASTE_HERDR_TOGGLE").map(|toggle| {
                    let mut c = std::process::Command::new(toggle);
                    c.arg("--leave");
                    c
                })
            };
            let sent = command.as_mut().is_some_and(|c| {
                c.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            });
            if !sent {
                return run_effect(app, client, Effect::Quit);
            }
        }
        Effect::Quit => {
            // tasks marked done are finished, not dropped, when the TUI closes
            let ids = app.take_pending_done();
            if !ids.is_empty() {
                complete_all(app, client, ids);
            }
            return true;
        }
        Effect::CompleteReminders { ids } => complete_all(app, client, ids),
        Effect::OpenAndReply { thread_id } => {
            let id = thread_id.clone();
            run_effect(app, client, Effect::OpenThread { thread_id });
            if app.reader.as_ref().is_some_and(|r| r.view.thread.id == id) {
                app.open_reply_form(&id);
            }
        }
        Effect::AiRewrite { .. } => {} // handled in the loop (a worker waits for the model)
        Effect::TodoSuggest { thread_id } => {
            // the stored TODO suggestion; without the helpers, or on a failed call, the popup
            // opens as it always did
            let (state, todo) = fetch_ai(app, client, &thread_id)
                .unwrap_or((haste_proto::AiState::Unavailable, None));
            app.show_todo_form(&thread_id, state, todo.as_ref());
        }
        Effect::OpenThread { thread_id } => {
            match client.call(RequestBody::ThreadShow { thread_id }) {
                Ok(Response::Thread {
                    view,
                    messages,
                    own,
                }) => {
                    let thread_id = view.thread.id.clone();
                    app.show_thread(view, messages, own);
                    // queues the draft and the TODO for an approved sender (and shows a draft
                    // that is ready in the reply box)
                    fetch_ai(app, client, &thread_id);
                    if let Some(id) = app.take_mark_read() {
                        // reading it here marks it read, at the provider too (Gmail)
                        if let Err(e) = describe(client.call(RequestBody::ReadSet {
                            thread_id: id,
                            read: true,
                        })) {
                            app.status = e;
                        }
                    }
                    // Ask for a fresh sync of this source (the daemon rate-limits it); the
                    // reader reloads when the new messages arrive. A failure changes nothing.
                    let _ = client.call(RequestBody::ThreadRefresh {
                        thread_id,
                        older: false,
                    });
                }
                other => {
                    app.status = describe(other)
                        .err()
                        .unwrap_or_else(|| "could not open the thread".into());
                }
            }
        }
        Effect::React { message_id, key } => {
            app.status = match client.call(RequestBody::ReactionAdd { message_id, key }) {
                Ok(Response::Done) => "reaction sent".into(),
                other => describe(other)
                    .err()
                    .unwrap_or_else(|| "could not react".into()),
            };
        }
        Effect::LoadOlder { thread_id } => {
            app.status = match client.call(RequestBody::ThreadRefresh {
                thread_id,
                older: true,
            }) {
                Ok(_) => {
                    "reading older messages from the provider (none appear if there are no more)"
                        .into()
                }
                Err(e) => format!("could not ask for older messages: {e}"),
            };
        }
        Effect::ReloadThread { thread_id } => {
            if let Ok(Response::Thread { messages, .. }) =
                client.call(RequestBody::ThreadShow { thread_id })
            {
                app.update_thread(messages);
            }
        }
        Effect::Mute { thread_id } => {
            app.status = match client.call(RequestBody::IgnoreMute { thread_id }) {
                Ok(Response::Muted { .. }) => {
                    "muted: its mail waits on the last screen (7); b there lets the sender back"
                        .to_owned()
                }
                other => describe(other).err().unwrap_or_else(|| "not muted".into()),
            };
            load_rules(app, client);
        }
        Effect::TodoLater {
            reminder_id,
            set_aside,
            until,
        } => {
            app.status = match describe(client.call(RequestBody::ReminderLater {
                reminder_id,
                set_aside,
                until,
            })) {
                Ok(()) if set_aside => {
                    "set aside on the Later tab (here only); g brings it back".into()
                }
                Ok(()) if until.is_some() => "snoozed: the due time is now when it returns".into(),
                Ok(()) => "back in the inbox".into(),
                Err(e) => e,
            };
        }
        Effect::SetDuration {
            reminder_id,
            minutes,
        } => {
            app.status = match describe(client.call(RequestBody::ReminderDuration {
                reminder_id,
                minutes,
            })) {
                Ok(()) => format!("the task takes {} on the calendar", inbox_minutes(minutes)),
                Err(e) => e,
            };
        }
        Effect::SearchThreads { query } => {
            match client.call(RequestBody::ThreadSearch {
                query: query.clone(),
            }) {
                Ok(Response::ThreadIds { ids }) => {
                    app.status = format!("{} threads contain \"{}\"", ids.len(), query);
                    app.set_search_hits(ids);
                }
                other => {
                    app.status = describe(other)
                        .err()
                        .unwrap_or_else(|| "the search found nothing to show".into());
                }
            }
            return false;
        }
        Effect::CopyText { text } => {
            app.status = match copy_to_clipboard(&text) {
                Ok(()) => format!("copied {} characters", text.chars().count()),
                Err(e) => format!("not copied: {e}"),
            };
        }
        Effect::DeleteEvent { event_id } => {
            app.status = match describe(client.call(RequestBody::EventDeleteLocal { event_id })) {
                Ok(()) => "event deleted".into(),
                Err(e) => format!("event not deleted: {e}"),
            };
            refresh(app, client);
            return false;
        }
        Effect::CreateEvent {
            calendar,
            title,
            start,
            end,
        } => {
            app.status = match calendar {
                None => {
                    match describe(client.call(RequestBody::EventCreateLocal { title, start, end }))
                    {
                        Ok(()) => "event added (here only)".into(),
                        Err(e) => format!("event not added: {e}"),
                    }
                }
                Some(calendar) => match create_google_event(client, calendar, title, start, end) {
                    Ok(()) => "event added to Google Calendar".into(),
                    Err(e) => format!("event not added: {e}"),
                },
            };
            // the new event shows now, not at the next refresh
            refresh(app, client);
            return false;
        }
        Effect::ShiftEvent { event_id, times } => {
            app.status = match describe(client.call(RequestBody::EventShift { event_id, times })) {
                Ok(()) if times.is_some() => {
                    "moved here only (Google keeps its time); R puts it back".into()
                }
                Ok(()) => "back to the times Google has".into(),
                Err(e) => e,
            };
        }
        Effect::DeleteMuted { thread_id } => {
            app.status = match client.call(RequestBody::MutedDelete { thread_id }) {
                Ok(Response::Deleted { .. }) => {
                    "deleted here (the mailbox is untouched; the sender stays muted)".to_owned()
                }
                other => describe(other)
                    .err()
                    .unwrap_or_else(|| "nothing was deleted".into()),
            };
        }
        Effect::DeleteThread { thread_id } => {
            app.status = match client.call(RequestBody::ThreadDelete { thread_id }) {
                Ok(Response::Deleted { .. }) => {
                    "deleted here (the mailbox is untouched)".to_owned()
                }
                other => describe(other)
                    .err()
                    .unwrap_or_else(|| "nothing was deleted".into()),
            };
        }
        Effect::SetRead { thread_id, read } => {
            app.status = match describe(client.call(RequestBody::ReadSet { thread_id, read })) {
                Ok(()) if read => "marked read".into(),
                Ok(()) => "marked unread".into(),
                Err(e) => e,
            };
        }
        Effect::Unblock { thread_id } => {
            use haste_proto::RuleKind;
            let steps = [
                RequestBody::RuleChange {
                    thread_id: thread_id.clone(),
                    rule: RuleKind::Mute,
                    add: false,
                    domain: false,
                },
                RequestBody::RuleChange {
                    thread_id: thread_id.clone(),
                    rule: RuleKind::Allowed,
                    add: true,
                    domain: false,
                },
                RequestBody::ReadSet {
                    thread_id,
                    read: true,
                },
            ];
            let mut result = Ok(());
            for step in steps {
                result = describe(client.call(step));
                if result.is_err() {
                    break;
                }
            }
            app.status = match result {
                Ok(()) => "unblocked: this sender is let through".into(),
                Err(e) => format!("not unblocked: {e}"),
            };
            load_rules(app, client);
        }
        Effect::PriorityRule {
            thread_id,
            add,
            domain,
        } => {
            app.status = match describe(client.call(RequestBody::RuleChange {
                thread_id,
                rule: haste_proto::RuleKind::Priority,
                add,
                domain,
            })) {
                Ok(()) if add && domain => "its whole domain is priority now".into(),
                Ok(()) if add => "this sender's mail is priority now".into(),
                Ok(()) => "this sender is no longer priority".into(),
                Err(e) => e,
            };
            load_rules(app, client);
        }
        Effect::SaveNote {
            thread_id,
            text,
            attachments,
        } => save_note(app, client, thread_id, text, attachments),
        Effect::SaveTodo {
            reminder_id,
            title,
            due,
            notes,
            attachments,
        } => save_todo(app, client, reminder_id, title, due, notes, attachments),
        Effect::SaveAttachments { files, dir } => {
            let total = files.len();
            let mut saved = 0;
            let mut failure = None;
            for (message_id, index) in files {
                match client.call(RequestBody::AttachmentSave {
                    message_id,
                    index,
                    dir: Some(dir.to_string_lossy().into_owned()),
                }) {
                    Ok(Response::AttachmentSaved { .. }) => saved += 1,
                    other => {
                        failure = describe(other).err();
                    }
                }
            }
            let home = std::env::var("HOME").unwrap_or_default();
            let shown = dir.to_string_lossy().replacen(&home, "~", 1);
            app.status = match failure {
                None => format!(
                    "saved {saved} file{} to {shown}",
                    if saved == 1 { "" } else { "s" }
                ),
                Some(e) => format!("saved {saved} of {total} to {shown}; {e}"),
            };
        }
        Effect::PreviewAttachment { message_id, index } => {
            preview_attachment(app, client, &message_id, index);
        }
        Effect::ExternalEdit { .. } => {} // needs the terminal; handled in the loop
        Effect::OpenLabels { thread_id } => {
            let workspace = match client.call(RequestBody::TagList) {
                Ok(Response::Tags { tags }) => tags,
                _ => Vec::new(),
            };
            let roots: Vec<std::path::PathBuf> = std::env::var_os("HOME")
                .map(|h| vec![std::path::PathBuf::from(h).join("projects")])
                .unwrap_or_default();
            let candidates =
                haste_tui::compose::label_candidates(&recent_labels(), &roots, &workspace);
            app.show_labels(&thread_id, candidates);
        }
        Effect::LabelChange {
            thread_id,
            label,
            add,
        } => {
            let request = if add {
                RequestBody::TagAdd {
                    thread_id,
                    tag: label.clone(),
                }
            } else {
                RequestBody::TagRemove {
                    thread_id,
                    tag: label.clone(),
                }
            };
            app.status = match describe(client.call(request)) {
                Ok(()) => {
                    if add {
                        remember_label(&label);
                    }
                    format!("{} label {label}", if add { "added" } else { "removed" })
                }
                Err(e) => format!("label not changed: {e}"),
            };
        }
        Effect::Refresh => {}
        Effect::Approve {
            ids,
            acknowledge_stale,
        } => {
            let mut failures = Vec::new();
            let total = ids.len();
            for id in ids {
                if let Err(e) = describe(client.call(RequestBody::Approve {
                    proposal_id: id.clone(),
                    acknowledge_stale,
                })) {
                    failures.push(format!("{id}: {e}"));
                }
            }
            app.status = if failures.is_empty() {
                format!("approved {total}")
            } else {
                format!(
                    "approved {}, failed: {}",
                    total - failures.len(),
                    failures.join("; ")
                )
            };
        }
        Effect::Reject { id } => {
            app.status = match describe(client.call(RequestBody::Reject {
                proposal_id: id,
                reason: None,
            })) {
                Ok(()) => "rejected; the agent will be told".into(),
                Err(e) => e,
            };
        }
        Effect::RefreshConnector { source } => {
            app.status = match describe(client.call(RequestBody::ConnectorRefresh {
                source: source.clone(),
            })) {
                Ok(()) => format!("reading {source} now; the time updates in a moment"),
                Err(e) => format!("not refreshed: {e}"),
            };
        }
        Effect::ReleaseConnector { source } => {
            let reply = client.call(RequestBody::ConnectorRelease {
                source: source.clone(),
            });
            app.status = match describe(reply) {
                Ok(()) => format!("{source} released; it will sync again from its saved cursor"),
                Err(e) => e,
            };
        }
        Effect::EngageConnector { source } => {
            let reply = client.call(RequestBody::ConnectorEngage {
                source: source.clone(),
            });
            app.status = match describe(reply) {
                Ok(()) => format!("{source} engaged"),
                Err(e) => e,
            };
        }
        Effect::Execute {
            id,
            acknowledge_stale,
        } => {
            app.status = match describe(client.call(RequestBody::Execute {
                proposal_id: id,
                acknowledge_stale,
            })) {
                Ok(()) => "done in Reminders".into(),
                Err(e) => e,
            };
        }
        Effect::CreateReminder(task) => {
            let task = *task;
            let lines = match keep_attachments(&task.attachments) {
                Ok(l) => l,
                Err(e) => {
                    app.status = format!("reminder not created, {e}");
                    return false; // nothing changed: no need to reload
                }
            };
            // the notes: what was typed, the attached files, and the mark that links the
            // reminder to its thread
            let mut notes = task.notes.clone();
            for line in lines
                .into_iter()
                .chain(task.thread_id.iter().map(|t| format!("haste:thread/{t}")))
            {
                if !notes.is_empty() {
                    notes.push('\n');
                }
                notes.push_str(&line);
            }
            if let Some(reminder_id) = task.reminder_id.clone() {
                // an edit: the title, the date and the repeat are set; the notes are added to
                // the old ones; the labels change by the difference
                let mut result = describe(client.call(RequestBody::ReminderEdit {
                    reminder_id: reminder_id.clone(),
                    title: task.title.clone(),
                    due: task.due,
                    append_notes: (!notes.is_empty()).then_some(notes),
                    repeat: task.repeat,
                    set_repeat: true,
                }));
                if result.is_ok() && task.minutes != task.minutes_before {
                    result = describe(client.call(RequestBody::ReminderDuration {
                        reminder_id: reminder_id.clone(),
                        minutes: task.minutes,
                    }));
                }
                if result.is_ok() && task.early != task.early_before {
                    result = describe(client.call(RequestBody::ReminderEarly {
                        reminder_id: reminder_id.clone(),
                        minutes: task.early,
                    }));
                }
                if result.is_ok() && task.priority != task.priority_before {
                    result = describe(client.call(RequestBody::ReminderPriority {
                        reminder_id: reminder_id.clone(),
                        priority: task.priority,
                    }));
                }
                if result.is_ok() {
                    let added = task.tags.iter().filter(|t| !task.tags_before.contains(t));
                    let removed = task.tags_before.iter().filter(|t| !task.tags.contains(t));
                    for tag in added {
                        result = result.and(describe(client.call(RequestBody::ReminderTagAdd {
                            reminder_id: reminder_id.clone(),
                            tag: tag.clone(),
                        })));
                    }
                    for tag in removed {
                        result =
                            result.and(describe(client.call(RequestBody::ReminderTagRemove {
                                reminder_id: reminder_id.clone(),
                                tag: tag.clone(),
                            })));
                    }
                }
                app.status = match result {
                    Ok(()) => "TODO saved".into(),
                    Err(e) => format!("TODO not saved: {e}"),
                };
                // the labels and the date changed: the list shows them now, not at quit
                refresh(app, client);
                return false;
            }
            app.status = match describe(client.call(RequestBody::ReminderNew {
                title: task.title,
                due: task.due,
                notes: Some(notes),
                repeat: task.repeat,
                tags: task.tags,
                thread_id: task.thread_id,
                minutes: Some(task.minutes),
                priority: Some(task.priority),
                early: task.early,
            })) {
                Ok(()) => "reminder created".into(),
                Err(e) => format!("reminder not created: {e}"),
            };
        }
        Effect::Triage { thread_id, change } => {
            // mail is archived in the mailbox too; chats only here
            let archived_text = if app.thread_is_mail(&thread_id) {
                "archived in the mailbox and here"
            } else {
                "archived (in haste only; chats have no upstream archive)"
            };
            let (priority, set_aside, snoozed_until, archived, done) = match change {
                TriageChange::Archive => (None, None, None, Some(true), archived_text),
                TriageChange::SetAside(on) => (
                    None,
                    Some(on),
                    None,
                    None,
                    if on { "put aside" } else { "back in the inbox" },
                ),
                TriageChange::Priority(p) => (
                    Some(p),
                    None,
                    None,
                    None,
                    if p > 0 {
                        "marked priority"
                    } else {
                        "priority cleared"
                    },
                ),
                TriageChange::SnoozeUntil(t) => (None, None, Some(Some(t)), None, "snoozed"),
                TriageChange::BringBack => {
                    (None, Some(false), Some(None), None, "back in the inbox")
                }
            };
            app.status = match describe(client.call(RequestBody::TriageSet {
                thread_id,
                priority,
                set_aside,
                snoozed_until,
                archived,
            })) {
                Ok(()) => done.into(),
                Err(e) => e,
            };
        }
        Effect::Reply { .. } => {} // opens the form; handled in the loop
        Effect::SaveReply {
            thread_id,
            body,
            attachments,
            then,
            replaces,
        } => save_reply(app, client, thread_id, body, &attachments, then, replaces),
        Effect::Schedule { id, at } => {
            app.status = match describe(client.call(RequestBody::ScheduleSend {
                proposal_id: id,
                at,
            })) {
                Ok(()) if at.is_some() => "scheduled".into(),
                Ok(()) => "the send time is off: it is a draft again".into(),
                Err(e) => e,
            };
        }
        Effect::UndoSend { id, restore } => {
            // the draft is withdrawn; the text opens in the editor again
            let withdrawn = describe(client.call(RequestBody::Reject {
                proposal_id: id,
                reason: Some("undone".into()),
            }));
            match (withdrawn, restore) {
                (Ok(()), Some(r)) => app.restore_reply(r),
                (Ok(()), None) => {}
                (Err(e), _) => {
                    app.status = format!("undone, but the draft stays in the Outbox: {e}")
                }
            }
        }
        Effect::Resolve { id, done } => {
            let settled = describe(client.call(RequestBody::ResolveExecuting {
                proposal_id: id.clone(),
                done,
            }));
            if settled.is_ok() {
                // the "may or may not have been delivered" notice has done its job
                app.notices.retain(|n| !n.contains(&id));
            }
            app.status = match settled {
                Ok(()) if done => "marked done".into(),
                Ok(()) => "returned to approved".into(),
                Err(e) => e,
            };
        }
        Effect::PreviewSend { id } => {
            match client.call(RequestBody::SendPreview {
                proposal_id: id.clone(),
            }) {
                Ok(Response::SendPreview { preview }) => app.show_send_preview(&id, preview),
                other => {
                    app.status = describe(other).err().unwrap_or_else(|| "no preview".into());
                }
            }
        }
        Effect::Send {
            id,
            acknowledge_stale,
            recipients_digest,
        } => {
            haste_core::log::info(
                "tui",
                "send_requested",
                &[haste_core::log::id("proposal", &id)],
            );
            let sent_at = std::time::Instant::now();
            let reply = client.call(RequestBody::Send {
                proposal_id: id.clone(),
                acknowledge_stale,
                recipients_digest,
            });
            let send_ms = sent_at.elapsed().as_millis().min(1 << 40) as i64;
            let changed = matches!(&reply, Ok(Response::Error { error }) if error.message == "recipients_changed");
            let outcome = match &reply {
                Ok(Response::Error { .. }) => "refused",
                Ok(_) => "accepted",
                Err(_) => "no_answer",
            };
            haste_core::log::info(
                "tui",
                "send_answered",
                &[
                    haste_core::log::id("proposal", &id),
                    haste_core::log::l("outcome", outcome),
                    haste_core::log::n("send_ms", send_ms),
                ],
            );
            app.status = match describe(reply) {
                Ok(()) => "sent".into(),
                Err(_) if changed => {
                    // The recipients differ from what was confirmed: nothing was sent. Reload the
                    // preview so the human sees the new list and decides again.
                    if let Ok(Response::SendPreview { preview }) =
                        client.call(RequestBody::SendPreview {
                            proposal_id: id.clone(),
                        })
                    {
                        app.show_confirmed_send_preview(&id, preview);
                    }
                    "the recipients changed; nothing was sent, check the new list".into()
                }
                Err(e) => e,
            };
        }
    }
    if !changes_no_list {
        refresh(app, client);
    }
    false
}

/// Open `$VISUAL`/`$EDITOR` on a private temp file, then propose the text as a draft reply.
/// Nothing is sent: the draft still needs approval and the human send flow.
/// `Ctrl-G` in a form: the focused field's text goes to `$VISUAL`/`$EDITOR` (the one place the
/// terminal is handed over) and comes back into the form.
/// The editor when `$VISUAL` and `$EDITOR` are unset: `nvim` when it is installed, else `vi`.
fn default_editor() -> &'static str {
    let found = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join("nvim").is_file()));
    if found {
        "nvim"
    } else {
        "vi"
    }
}

fn edit_external(terminal: &mut ratatui::DefaultTerminal, app: &mut App, text: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = base_dir().join("drafts");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("field-{}.txt", std::process::id()));
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if created.is_err() {
        app.status = "could not create the editor file".into();
        return;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| default_editor().into());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi").to_owned();
    restore_keys();
    ratatui::restore();
    let ran = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status();
    *terminal = ratatui::init();
    enhance_keys();
    let _ = terminal.clear();
    let edited = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if matches!(ran, Ok(s) if s.success()) {
        app.compose_replace_text(&edited);
    } else {
        app.status = "the editor did not finish; the field is unchanged".into();
    }
}

/// A reply written in the form becomes a draft proposal, approved right away because the human
/// wrote it. Sending is still its own step in Approvals, with the recipient check and countdown.
/// Fetch one attachment into the daemon's private preview folder, read its text or picture with the
/// system tools, show it in a popup, and delete the file again.
fn preview_attachment(app: &mut App, client: &Client, message_id: &str, index: usize) {
    let Some(file) = app
        .attach
        .as_ref()
        .and_then(|menu| menu.files.get(index).cloned())
    else {
        return;
    };
    let Some(kind) = haste_tui::extract::kind_of(&file.name, &file.mime) else {
        app.status =
            "no preview for this type (pictures, PDF, Word, PowerPoint): d downloads it".into();
        return;
    };
    let saved = client.call(RequestBody::AttachmentSave {
        message_id: message_id.to_owned(),
        index,
        dir: None,
    });
    let Ok(Response::AttachmentSaved { path, .. }) = saved else {
        app.status = describe(saved)
            .err()
            .unwrap_or_else(|| "could not fetch the attachment".into());
        return;
    };
    let path = std::path::PathBuf::from(path);
    if kind == haste_tui::extract::Kind::Image {
        let image = haste_tui::extract::load_image(&path);
        let _ = std::fs::remove_file(&path);
        match image {
            Ok(image) => app.show_image(&file.name, image),
            Err(e) => app.status = e,
        }
        return;
    }
    let text = haste_tui::extract::extract_text(&path, kind);
    let _ = std::fs::remove_file(&path);
    match text {
        Ok(text) => app.show_preview(&file.name, &text),
        Err(e) => app.status = e,
    }
}

fn save_reply(
    app: &mut App,
    client: &Client,
    thread_id: String,
    body: String,
    attachments: &[std::path::PathBuf],
    then: haste_tui::app::AfterSave,
    replaces: Option<String>,
) {
    use haste_tui::app::AfterSave;
    // files are copied into the attachments folder first; the draft names them, the daemon reads
    // them at send time and the confirmation shows them
    let names = match store_for_send(attachments) {
        Ok(n) => n,
        Err(e) => {
            app.status = format!("reply not saved, {e}");
            return;
        }
    };
    let key = format!("tui-reply-{thread_id}-{}", haste_core::time::now_secs());
    let started = std::time::Instant::now();
    let id = match run_draft(client, thread_id, body, names, key) {
        Ok(id) => id,
        Err(e) => {
            app.status = e;
            return;
        }
    };
    // the new copy is kept; the draft it continues is withdrawn
    if let Some(old) = replaces {
        let _ = client.call(RequestBody::Reject {
            proposal_id: old,
            reason: Some("replaced".into()),
        });
    }
    match then {
        AfterSave::Draft => {
            app.status = "draft kept: it is there when you reply again (Outbox: 5)".into()
        }
        AfterSave::Send => {
            if app.pending_send.is_some() {
                app.status = "another send is waiting: the reply is in the Outbox (5)".into();
                return;
            }
            let drafted = started.elapsed();
            let previewed = client.call(RequestBody::SendPreview {
                proposal_id: id.clone(),
            });
            haste_core::log::info(
                "tui",
                "reply_prepared",
                &[
                    haste_core::log::id("proposal", &id),
                    haste_core::log::n("draft_ms", drafted.as_millis().min(1 << 40) as i64),
                    haste_core::log::n("total_ms", started.elapsed().as_millis().min(1 << 40) as i64),
                ],
            );
            match previewed {
                Ok(Response::SendPreview { preview }) => app.show_send_preview(&id, preview),
                other => {
                    app.status = format!(
                        "not sent, the draft is in the Outbox (5): {}",
                        describe(other).err().unwrap_or_else(|| "no preview".into())
                    );
                }
            }
        }
        AfterSave::Later(at) => {
            app.status = match describe(client.call(RequestBody::ScheduleSend {
                proposal_id: id,
                at: Some(at),
            })) {
                Ok(()) => format!(
                    "scheduled for {}: it waits in the Outbox (5)",
                    haste_tui::inbox::stamp(at, app.tz_offset)
                ),
                Err(e) => format!("saved as a draft in the Outbox, not scheduled: {e}"),
            };
        }
    }
}

/// Copy files into the attachments folder and return the stored file names.
fn store_for_send(files: &[std::path::PathBuf]) -> Result<Vec<String>, String> {
    let dir = base_dir().join("attachments");
    let stamp = haste_core::time::now_secs();
    files
        .iter()
        .map(|f| {
            haste_tui::compose::store_attachment(&dir, f, stamp)
                .map(|p| {
                    p.file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                })
                .map_err(|e| {
                    format!(
                        "{}: {e}",
                        f.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    )
                })
        })
        .collect()
}

fn run_draft(
    client: &Client,
    thread_id: String,
    body: String,
    attachments: Vec<String>,
    key: String,
) -> Result<String, String> {
    let proposal = match client.call(RequestBody::Propose {
        payload: haste_core::proposal::Payload::Draft {
            to: None,
            body,
            attachments,
        },
        thread_id: Some(thread_id),
        idempotency_key: key,
        hints: None,
    }) {
        Ok(Response::Proposal { proposal }) => proposal,
        other => {
            return Err(describe(other)
                .err()
                .unwrap_or_else(|| "no proposal".into()))
        }
    };
    let id = proposal.id;
    describe(client.call(RequestBody::Approve {
        proposal_id: id.clone(),
        acknowledge_stale: false,
    }))
    .map(|()| id)
}

/// Put text on the system clipboard: `pbcopy` on macOS, `wl-copy` or `xclip` elsewhere. The text
/// goes only to that program's input, never to a log or the screen.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let programs: [(&str, &[&str]); 3] = [
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
    ];
    for (program, args) in programs {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        return match child.wait() {
            Ok(status) if status.success() => Ok(()),
            _ => Err(format!("{program} failed")),
        };
    }
    Err("no clipboard program found (pbcopy, wl-copy or xclip)".into())
}

/// `haste-tui --status`: the line for Herdr's status bar (see `haste_tui::statusline`), then
/// exit. No screen, no log file, one read of what it needs from the daemon. Printing nothing is
/// meaningful: Herdr then hides the entry.
fn status_main() {
    // herdr gives this a few seconds: a daemon that does not answer is "unreachable" in time
    let client = match Client::from_base(&base_dir()) {
        Ok(c) => c.with_timeout(Duration::from_secs(4)),
        Err(_) => {
            println!("{}", haste_tui::statusline::unreachable());
            return;
        }
    };
    let mut app = App::new();
    app.refresh_tz();
    match client.call(RequestBody::ThreadList {
        sources: vec![],
        include_archived: false,
        include_snoozed: true,
    }) {
        Ok(Response::Threads { threads }) => app.set_threads(threads),
        _ => {
            println!("{}", haste_tui::statusline::unreachable());
            return;
        }
    }
    load_rules(&mut app, &client);
    load_reminders(&mut app, &client);
    if let Ok(Response::Connectors { connectors }) = client.call(RequestBody::ConnectorStatus) {
        app.set_connectors(connectors);
    }
    println!(
        "{}",
        haste_tui::statusline::line(&app, haste_core::time::now_secs())
    );
}

fn main() -> io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--status") {
        status_main();
        return Ok(());
    }
    let _ = haste_core::log::init(
        &base_dir(),
        "haste-tui",
        haste_core::log::level_from_env(haste_core::log::Level::Info),
        None, // the screen is the terminal: never print log lines over it
    );
    haste_core::log::install_panic_hook();
    haste_core::log::info(
        "tui",
        "starting",
        &[haste_core::log::id("version", env!("CARGO_PKG_VERSION"))],
    );
    let client = Client::from_base(&base_dir()).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot read the human token (is hasted running?): {e}"),
        )
    })?;
    let events = client.subscribe();
    let mut app = App::new();
    let (settings, warning) = haste_tui::settings::Settings::load(&haste_tui::settings::Settings::path());
    app.settings = settings;
    if let Some(w) = warning {
        app.status = format!("config ignored: {w}");
    }
    app.calendar_colors = haste_tui::calendarcolors::CalendarColors::load(&base_dir());
    app.background_mode = std::env::var_os("HASTE_HERDR_TAB").is_some()
        || std::env::var_os("HASTE_BACKGROUND_COMMAND").is_some();
    refresh(&mut app, &client);
    if app.calendar_colors.ignored > 0 {
        app.status = format!(
            "calendar_colors.json: {} entries were not understood and are left out",
            app.calendar_colors.ignored
        );
    }
    let mut last_reminders = Instant::now();
    let mut last_logs = Instant::now() - LOGS_EVERY;
    let (archive_jobs, archive_done) = archive_worker(client.clone());
    let (react_jobs, react_done) = react_worker(client.clone());
    let (rewrite_jobs, rewrite_done) = rewrite_worker(client.clone());
    app.ai_prefill = true;
    let mut last_ai_poll = Instant::now();

    let mut terminal = ratatui::init();
    enhance_keys();
    let result = (|| -> io::Result<()> {
        loop {
            let done = terminal.draw(|f| draw(f, &app))?;
            // message links become clickable (OSC 8) once the frame is on screen
            let escape = match (&app.reader, &app.mode) {
                (Some(reader), haste_tui::app::Mode::Read) => haste_tui::ui::link_escape(
                    &haste_tui::ui::link_runs(done.buffer, &reader.links.borrow()),
                ),
                _ => String::new(),
            };
            if !escape.is_empty() {
                use std::io::Write;
                let out = terminal.backend_mut();
                out.write_all(escape.as_bytes())?;
                out.flush()?;
            }
            if event::poll(POLL)? {
                if let TermEvent::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        if let Some(effect) =
                            map_key(key.code, key.modifiers).and_then(|k| app.handle_key(k))
                        {
                            if let Effect::ExternalEdit { text } = effect {
                                edit_external(&mut terminal, &mut app, &text);
                            } else if let Effect::Reply { thread_id } = effect {
                                app.open_reply_form(&thread_id);
                            } else if let Effect::Triage {
                                thread_id,
                                change: TriageChange::Archive,
                            } = effect
                            {
                                start_archive(&mut app, &archive_jobs, thread_id);
                            } else if let Effect::AiRewrite {
                                thread_id,
                                draft,
                                mode,
                                instruction,
                            } = effect
                            {
                                if rewrite_jobs
                                    .send(RewriteJob {
                                        thread_id,
                                        draft,
                                        mode,
                                        instruction,
                                    })
                                    .is_err()
                                {
                                    app.finish_rewrite(Err("the rewrite worker stopped".into()));
                                }
                            } else if let Effect::React { message_id, key } = effect {
                                if react_jobs
                                    .send(ReactJob {
                                        message_id: message_id.clone(),
                                        key,
                                    })
                                    .is_err()
                                {
                                    app.finish_reaction(
                                        &message_id,
                                        Err("the reaction worker stopped".into()),
                                    );
                                }
                            } else if run_effect(&mut app, &client, effect) {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            // the daemon's log is read again every second while its screen is open
            if app.view == haste_tui::app::View::Logs && last_logs.elapsed() >= LOGS_EVERY {
                last_logs = Instant::now();
                app.set_logs(haste_tui::logs::read_tail(
                    &base_dir().join("logs").join("hasted.log"),
                ));
            }
            if last_reminders.elapsed() >= REMINDERS_EVERY {
                last_reminders = Instant::now();
                load_reminders(&mut app, &client);
                // the channels' last-read times move without any change to announce
                if let Ok(Response::Connectors { connectors }) =
                    client.call(RequestBody::ConnectorStatus)
                {
                    app.set_connectors(connectors);
                }
            }
            if let Some(effect) = app.tick(Instant::now()) {
                if run_effect(&mut app, &client, effect) {
                    return Ok(());
                }
            }
            let mut dirty = false;
            while let Ok(answer) = react_done.try_recv() {
                app.finish_reaction(&answer.message_id, answer.result);
            }
            while let Ok(answer) = rewrite_done.try_recv() {
                app.finish_rewrite(answer.result);
            }
            // a reply box open on a draft the model is still writing: look again every few
            // seconds (the daemon only answers from its store)
            if last_ai_poll.elapsed() >= AI_POLL_EVERY {
                last_ai_poll = Instant::now();
                if let Some(thread_id) = app.wants_ai_poll() {
                    fetch_ai(&mut app, &client, &thread_id);
                }
            }
            while let Ok(finished) = archive_done.try_recv() {
                let failed = finished.result.is_err();
                app.finish_archive(&finished.thread_id, finished.result.map(|()| finished.done));
                dirty |= failed;
            }
            while let Ok(ev) = events.try_recv() {
                dirty |= app.on_event(&ev).is_some();
            }
            if dirty {
                refresh(&mut app, &client);
            }
        }
    })();
    restore_keys();
    ratatui::restore();
    result
}

/// Ask the terminal to report Ctrl-Enter and the like apart from plain Enter, where it can.
fn enhance_keys() {
    use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    use std::sync::atomic::Ordering;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        KEYS_ENHANCED.store(true, Ordering::Relaxed);
        let _ = crossterm::execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
}

static KEYS_ENHANCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn restore_keys() {
    if KEYS_ENHANCED.swap(false, std::sync::atomic::Ordering::Relaxed) {
        let _ = crossterm::execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags);
    }
}
