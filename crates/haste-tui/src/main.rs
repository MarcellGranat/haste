use std::io;
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
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
            Key::Ctrl(c.to_ascii_lowercase())
        }
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::BackTab => Key::Tab,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Down => Key::Down,
        KeyCode::Up => Key::Up,
        _ => return None,
    })
}

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
    if let Ok(Response::Ignore { patterns, errors }) = client.call(RequestBody::IgnoreList) {
        app.set_ignore(&patterns, errors);
    }
    if let Ok(Response::Filters { filters, error }) = client.call(RequestBody::FilterList) {
        app.set_filters(filters, error);
    }
    if let Ok(Response::Events { events }) = client.call(RequestBody::EventsList {
        from: haste_core::time::now_secs() - 3600,
    }) {
        app.set_events(events);
    }
    if let Ok(Response::Reminders { reminders }) = client.call(RequestBody::RemindersList {
        include_completed: false,
    }) {
        app.set_reminder_list(reminders);
    }
    match client.call(RequestBody::ThreadList {
        sources: vec![],
        include_archived: false,
        include_snoozed: false,
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

/// Run a reminder proposal the human made themselves: propose, approve and execute in one go.
/// The approval step exists for agents; nothing a human does in the TUI waits for it.
fn run_as_human(
    client: &Client,
    payload: haste_core::proposal::Payload,
    thread_id: Option<String>,
    key: String,
) -> Result<(), String> {
    let proposal = match client.call(RequestBody::Propose {
        payload,
        thread_id,
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
    let _ = client.call(RequestBody::Approve {
        proposal_id: proposal.id.clone(),
        acknowledge_stale: false,
    });
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
            },
            "TODO saved",
        ),
        None => (
            RequestBody::ReminderNew {
                title,
                due,
                notes: extra,
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
fn run_effect(app: &mut App, client: &Client, effect: Effect) -> bool {
    match effect {
        Effect::Quit => {
            // tasks marked done are finished, not dropped, when the TUI closes
            let ids = app.take_pending_done();
            if !ids.is_empty() {
                complete_all(app, client, ids);
            }
            return true;
        }
        Effect::CompleteReminders { ids } => complete_all(app, client, ids),
        Effect::OpenThread { thread_id } => {
            match client.call(RequestBody::ThreadShow { thread_id }) {
                Ok(Response::Thread {
                    view,
                    messages,
                    own,
                }) => app.show_thread(view, messages, own),
                other => {
                    app.status = describe(other)
                        .err()
                        .unwrap_or_else(|| "could not open the thread".into());
                }
            }
        }
        Effect::Mute { thread_id } => {
            app.status = match client.call(RequestBody::IgnoreMute { thread_id }) {
                Ok(Response::Muted { pattern }) => {
                    format!("muted {pattern} (to undo, delete that line in .haste_ignore)")
                }
                other => describe(other).err().unwrap_or_else(|| "not muted".into()),
            };
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
        Effect::OpenUrl { url } => {
            // only plain web links, so nothing but a page can be opened from message text
            let ok = (url.starts_with("https://") || url.starts_with("http://"))
                && !url.chars().any(|c| c.is_control() || c.is_whitespace());
            app.status = if ok
                && std::process::Command::new("open")
                    .arg(&url)
                    .status()
                    .is_ok_and(|s| s.success())
            {
                "opened in the browser".into()
            } else {
                "could not open the link".into()
            };
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
        Effect::ToReminder {
            thread_id,
            title,
            due,
        } => {
            let key = format!("to-reminder-{thread_id}-{}", haste_core::time::now_secs());
            app.status = match run_as_human(
                client,
                haste_core::proposal::Payload::ReminderCreate {
                    title,
                    due,
                    list: None,
                    project: None,
                    thread_id: Some(thread_id.clone()),
                },
                Some(thread_id),
                key,
            ) {
                Ok(()) => "reminder created".into(),
                Err(e) => e,
            };
        }
        Effect::Triage { thread_id, change } => {
            let (priority, set_aside, snoozed_until, archived, done) = match change {
                TriageChange::Archive => (None, None, None, Some(true), "archived (in haste only)"),
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
        Effect::SaveReply { thread_id, body } => save_reply(app, client, thread_id, body),
        Effect::Resolve { id, done } => {
            app.status = match describe(client.call(RequestBody::ResolveExecuting {
                proposal_id: id,
                done,
            })) {
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
            let reply = client.call(RequestBody::Send {
                proposal_id: id.clone(),
                acknowledge_stale,
                recipients_digest,
            });
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
    refresh(app, client);
    false
}

/// Open `$VISUAL`/`$EDITOR` on a private temp file, then propose the text as a draft reply.
/// Nothing is sent: the draft still needs approval and the human send flow.
/// `Ctrl-G` in a form: the focused field's text goes to `$VISUAL`/`$EDITOR` (the one place the
/// terminal is handed over) and comes back into the form.
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
        .unwrap_or_else(|_| "vi".into());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi").to_owned();
    ratatui::restore();
    let ran = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status();
    *terminal = ratatui::init();
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
fn save_reply(app: &mut App, client: &Client, thread_id: String, body: String) {
    let key = format!("tui-reply-{thread_id}-{}", haste_core::time::now_secs());
    app.status = match run_draft(client, thread_id, body, key) {
        Ok(()) => "reply drafted and approved: 4 Approvals, then s to send".into(),
        Err(e) => e,
    };
}

fn run_draft(client: &Client, thread_id: String, body: String, key: String) -> Result<(), String> {
    let proposal = match client.call(RequestBody::Propose {
        payload: haste_core::proposal::Payload::Draft { to: None, body },
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
    describe(client.call(RequestBody::Approve {
        proposal_id: proposal.id,
        acknowledge_stale: false,
    }))
}

fn main() -> io::Result<()> {
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
    refresh(&mut app, &client);

    let mut terminal = ratatui::init();
    let result = (|| -> io::Result<()> {
        loop {
            terminal.draw(|f| draw(f, &app))?;
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
                            } else if run_effect(&mut app, &client, effect) {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            if let Some(effect) = app.tick(Instant::now()) {
                if run_effect(&mut app, &client, effect) {
                    return Ok(());
                }
            }
            let mut dirty = false;
            while let Ok(ev) = events.try_recv() {
                dirty |= app.on_event(&ev).is_some();
            }
            if dirty {
                refresh(&mut app, &client);
            }
        }
    })();
    ratatui::restore();
    result
}
