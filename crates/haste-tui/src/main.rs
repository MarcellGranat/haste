use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
use haste_core::proposal::ProposalState;
use haste_proto::{ErrorCode, RequestBody, Response};
use haste_tui::app::{App, Effect, Key};
use haste_tui::client::{base_dir, Client};
use haste_tui::ui::draw;

const POLL: Duration = Duration::from_millis(200);

fn map_key(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Char(c) => Key::Char(c),
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
    match client.call(RequestBody::ProposalList {
        states: vec![
            ProposalState::Pending,
            ProposalState::Approved,
            ProposalState::Executing,
        ],
    }) {
        Ok(Response::Proposals { proposals }) => app.set_proposals(proposals),
        Ok(other) => app.status = format!("unexpected reply: {other:?}"),
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

fn describe(response: Result<Response, String>) -> Result<(), String> {
    match response {
        Ok(Response::Error { error }) => Err(match error.code {
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
        Effect::Quit => return true,
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
        Effect::ToReminder { thread_id, title } => {
            let key = format!("to-reminder-{thread_id}");
            app.status = match describe(client.call(RequestBody::Propose {
                payload: haste_core::proposal::Payload::ReminderCreate {
                    title,
                    due: None,
                    list: None,
                    project: None,
                    thread_id: None,
                },
                thread_id: Some(thread_id),
                idempotency_key: key,
                hints: None,
            })) {
                Ok(()) => "reminder proposed; approve it in Approvals".into(),
                Err(e) => e,
            };
        }
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
                        if let Some(effect) = map_key(key.code).and_then(|k| app.handle_key(k)) {
                            if run_effect(&mut app, &client, effect) {
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
