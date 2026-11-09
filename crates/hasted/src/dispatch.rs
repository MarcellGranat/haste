//! Request dispatch: authenticate, token-subset check, policy gate, execute or propose.

use std::collections::BTreeSet;

use haste_core::audit::read_recent;
use haste_core::connector::{
    CallError, ConnectorError, DraftToSend, EventPatch, Guarded, NewEvent, NewReminder,
};
use haste_core::herdr::visible_to;
use haste_core::model::{notes_snippet, Reminder, Source};
use haste_core::notes::{note_project, slugify, NoteDenial};
use haste_core::policy::{evaluate, Action, Matrix, Outcome, Principal, PrincipalKind, Target};
use haste_core::proposal::Payload;
use haste_core::time::now_secs;
use haste_proto::{
    AuditLine, ConnectorView, ErrorCode, Event, EventView, Hints, ReminderView,
    RemindersStatusView, Request, RequestBody, Response, ThreadView, PROTOCOL_VERSION,
};
use haste_store::{AuthError, NewProposal, StoreError, ThreadFilter};

use crate::notes::{append_note, AppendError};
use crate::state::State;

fn err(code: ErrorCode, message: &str) -> Response {
    Response::error(code, message)
}

fn map_store(e: &StoreError) -> Response {
    match e {
        StoreError::NotFound(m) => err(ErrorCode::NotFound, m),
        StoreError::Validation(m) => err(ErrorCode::Validation, m),
        StoreError::IllegalTransition(m) => err(ErrorCode::IllegalTransition, m),
        StoreError::StaleNeedsAcknowledgement => err(
            ErrorCode::StaleNeedsAcknowledgement,
            "thread changed since the proposal; acknowledge to continue",
        ),
        other => err(ErrorCode::Internal, &other.to_string()),
    }
}

/// The gate action a request body corresponds to.
fn action_for(body: &RequestBody) -> Action {
    match body {
        RequestBody::ThreadList { .. }
        | RequestBody::ThreadShow { .. }
        | RequestBody::ProposalStatus { .. }
        | RequestBody::ProposalList { .. }
        | RequestBody::AgentList
        | RequestBody::RemindersList { .. }
        | RequestBody::EventsList { .. } => Action::Read,
        RequestBody::Propose { payload, .. } => payload.action(),
        RequestBody::NoteAppend { .. } => Action::NoteAppend,
        RequestBody::Approve { .. } => Action::Approve,
        RequestBody::Reject { .. } => Action::Reject,
        // Running an approved reminder proposal is as human-only as sending a draft.
        RequestBody::Send { .. } | RequestBody::Execute { .. } => Action::Send,
        RequestBody::RegisterPrincipal { .. }
        | RequestBody::RevokeToken { .. }
        | RequestBody::AuditRecent { .. }
        | RequestBody::ConnectorStatus
        | RequestBody::ConnectorRelease { .. }
        | RequestBody::ConnectorEngage { .. }
        | RequestBody::ConnectorAuthorized { .. }
        | RequestBody::TagAdd { .. }
        | RequestBody::TagRemove { .. }
        | RequestBody::TagList
        | RequestBody::ReminderTagAdd { .. }
        | RequestBody::ReminderTagRemove { .. }
        | RequestBody::EventTagAdd { .. }
        | RequestBody::EventTagRemove { .. }
        | RequestBody::RemindersStatus
        | RequestBody::ResolveExecuting { .. }
        | RequestBody::Subscribe { .. } => Action::Manage,
    }
}

/// A short, fixed name for a request, used in audit reason codes. Never contains request data.
fn request_name(body: &RequestBody) -> &'static str {
    match body {
        RequestBody::TagAdd { .. } => "tag_add",
        RequestBody::TagRemove { .. } => "tag_remove",
        RequestBody::TagList => "tag_list",
        RequestBody::ConnectorStatus => "connector_status",
        RequestBody::ConnectorRelease { .. } => "connector_release",
        RequestBody::ConnectorEngage { .. } => "connector_engage",
        RequestBody::ConnectorAuthorized { .. } => "connector_authorized",
        RequestBody::RegisterPrincipal { .. } => "register_principal",
        RequestBody::RevokeToken { .. } => "revoke_token",
        RequestBody::AuditRecent { .. } => "audit_recent",
        RequestBody::Subscribe { .. } => "subscribe",
        RequestBody::Approve { .. } => "approve",
        RequestBody::Reject { .. } => "reject",
        RequestBody::Send { .. } => "send",
        RequestBody::Execute { .. } => "execute",
        RequestBody::ReminderTagAdd { .. } => "reminder_tag_add",
        RequestBody::EventTagAdd { .. } => "event_tag_add",
        RequestBody::EventTagRemove { .. } => "event_tag_remove",
        RequestBody::ReminderTagRemove { .. } => "reminder_tag_remove",
        RequestBody::RemindersStatus => "reminders_status",
        RequestBody::ResolveExecuting { .. } => "resolve_executing",
        _ => "request",
    }
}

fn is_human_only(action: Action) -> bool {
    matches!(
        action,
        Action::Send | Action::Approve | Action::Reject | Action::Manage
    )
}

/// Handle one request. Synchronous; callers run it on a blocking thread.
pub fn dispatch(state: &State, request: Request) -> Response {
    if request.version != PROTOCOL_VERSION {
        return err(
            ErrorCode::UnsupportedVersion,
            "unsupported protocol version",
        );
    }
    let auth = match state.store.authenticate(&request.token) {
        Ok(a) => a,
        Err(AuthError::Unauthenticated) => {
            state.audit("unknown", "authenticate", "unauthenticated", None, None);
            return err(ErrorCode::Unauthenticated, "authentication required");
        }
        Err(AuthError::Store(_)) => return err(ErrorCode::Internal, "authentication unavailable"),
    };
    let principal = auth.principal;
    let action = action_for(&request.body);

    // Human-only actions are denied for every non-human principal, whatever the token says.
    if principal.kind != PrincipalKind::Human && is_human_only(action) {
        state.audit(
            &principal.id,
            action.as_str(),
            "denied",
            None,
            Some(&format!(
                "human_only_action:{}",
                request_name(&request.body)
            )),
        );
        return err(ErrorCode::Denied, "action is reserved for the human");
    }
    if !auth.allowed.contains(&action) {
        state.audit(
            &principal.id,
            action.as_str(),
            "unauthorized",
            None,
            Some("outside_token_subset"),
        );
        return err(ErrorCode::Unauthorized, "token does not allow this action");
    }

    let target = match &request.body {
        RequestBody::NoteAppend { path, .. } => Target::NotePath(path.clone()),
        _ => Target::None,
    };
    let mut matrix = Matrix::new();
    matrix.grant_defaults(&principal);
    let decision = evaluate(&principal, action, &target, &matrix);
    let object = match &target {
        Target::NotePath(p) => Some(p.as_str()),
        Target::None => None,
    };
    state.audit(
        &principal.id,
        action.as_str(),
        decision.outcome.as_str(),
        object,
        Some(decision.reason),
    );
    if decision.outcome == Outcome::Denied {
        return err(ErrorCode::Denied, decision.reason);
    }
    execute(state, &principal, request.body)
}

fn execute(state: &State, principal: &Principal, body: RequestBody) -> Response {
    let now = now_secs();
    match body {
        RequestBody::ThreadList {
            sources,
            include_archived,
            include_snoozed,
        } => {
            let filter = ThreadFilter {
                sources: sources.into_iter().map(Source::from).collect(),
                include_archived,
                include_snoozed,
                now,
            };
            let tags = state.store.all_thread_tags().unwrap_or_default();
            let machine_labels = machine_labels_for(state, principal);
            match state.store.list_threads(&filter) {
                Ok(rows) => Response::Threads {
                    threads: rows
                        .into_iter()
                        .filter_map(|(thread, triage)| {
                            let own = tags.get(&thread.id).cloned().unwrap_or_default();
                            // The one scope filter for thread reads: a machine sees only its projects.
                            visible_to(principal, &own, machine_labels.as_ref()).then_some(
                                ThreadView {
                                    thread,
                                    triage,
                                    tags: own,
                                },
                            )
                        })
                        .collect(),
                },
                Err(e) => map_store(&e),
            }
        }
        RequestBody::ThreadShow { thread_id } => match state.store.get_thread(&thread_id) {
            Ok(Some((thread, triage))) => {
                let own = state.store.thread_tags(&thread_id).unwrap_or_default();
                let machine_labels = machine_labels_for(state, principal);
                if !visible_to(principal, &own, machine_labels.as_ref()) {
                    // Indistinguishable from a thread that does not exist.
                    return err(ErrorCode::NotFound, "thread not found");
                }
                match state.store.messages_for_thread(&thread_id) {
                    Ok(messages) => Response::Thread {
                        view: ThreadView {
                            thread,
                            triage,
                            tags: own,
                        },
                        messages,
                    },
                    Err(e) => map_store(&e),
                }
            }
            Ok(None) => err(ErrorCode::NotFound, "thread not found"),
            Err(e) => map_store(&e),
        },
        RequestBody::Propose {
            payload,
            thread_id,
            idempotency_key,
            hints,
        } => propose(
            state,
            principal,
            payload,
            thread_id,
            idempotency_key,
            hints.as_ref(),
            now,
        ),
        RequestBody::NoteAppend {
            path,
            text,
            idempotency_key,
        } => note_append(state, principal, &path, &text, &idempotency_key, now),
        RequestBody::ProposalStatus { proposal_id } => {
            let found = if principal.kind == PrincipalKind::Human {
                state.store.get_proposal(&proposal_id)
            } else {
                state.store.get_proposal_for(&principal.id, &proposal_id)
            };
            match found {
                Ok(proposal) => Response::Proposal { proposal },
                Err(e) => map_store(&e),
            }
        }
        RequestBody::ProposalList { states } => match state.store.list_proposals(&states) {
            Ok(all) => Response::Proposals {
                proposals: all
                    .into_iter()
                    .filter(|p| {
                        principal.kind == PrincipalKind::Human || p.principal_id == principal.id
                    })
                    .collect(),
            },
            Err(e) => map_store(&e),
        },
        RequestBody::Approve {
            proposal_id,
            acknowledge_stale,
        } => {
            match state
                .store
                .approve_proposal(&proposal_id, acknowledge_stale, now)
            {
                Ok(out) => {
                    state.audit(
                        &principal.id,
                        "proposal.approve",
                        "approved",
                        Some(&proposal_id),
                        None,
                    );
                    // For connectors that can draft, approval also creates the provider draft.
                    let proposal = create_provider_draft(state, principal, &out.proposal, now);
                    state.publish(Event::ProposalChanged {
                        proposal_id,
                        state: proposal.state,
                    });
                    Response::Proposal { proposal }
                }
                Err(e) => {
                    state.audit(
                        &principal.id,
                        "proposal.approve",
                        "refused",
                        Some(&proposal_id),
                        None,
                    );
                    map_store(&e)
                }
            }
        }
        RequestBody::Reject {
            proposal_id,
            reason,
        } => {
            match state
                .store
                .reject_proposal(&proposal_id, reason.as_deref(), now)
            {
                Ok(p) => {
                    state.audit(
                        &principal.id,
                        "proposal.reject",
                        "rejected",
                        Some(&proposal_id),
                        None,
                    );
                    state.publish(Event::ProposalChanged {
                        proposal_id,
                        state: p.state,
                    });
                    Response::Proposal { proposal: p }
                }
                Err(e) => map_store(&e),
            }
        }
        RequestBody::Send {
            proposal_id,
            acknowledge_stale,
        } => send(state, principal, &proposal_id, acknowledge_stale, now),
        RequestBody::RegisterPrincipal {
            principal_id,
            kind,
            allowed_actions,
        } => register(state, principal, &principal_id, kind, &allowed_actions),
        RequestBody::RevokeToken { principal_id } => match state.store.revoke_tokens(&principal_id)
        {
            Ok(()) => {
                state.audit(
                    &principal.id,
                    "token.revoke",
                    "ok",
                    Some(&principal_id),
                    None,
                );
                Response::Done
            }
            Err(e) => map_store(&e),
        },
        RequestBody::AuditRecent { limit } => {
            match read_recent(&state.config.audit_path(), limit as usize) {
                Ok(records) => Response::Audit {
                    records: records
                        .into_iter()
                        .map(|r| AuditLine {
                            seq: r.seq,
                            ts: r.ts,
                            principal: r.principal,
                            action: r.action,
                            outcome: r.outcome,
                            object: r.object,
                        })
                        .collect(),
                },
                Err(e) => err(ErrorCode::Internal, &e.to_string()),
            }
        }
        RequestBody::ConnectorStatus => connector_status(state),
        RequestBody::ConnectorRelease { source } => set_switch(state, principal, &source, false),
        RequestBody::ConnectorEngage { source } => set_switch(state, principal, &source, true),
        RequestBody::ConnectorAuthorized { source } => {
            state.audit(
                &principal.id,
                "connector.auth",
                "authorized",
                Some(&source),
                None,
            );
            Response::Done
        }
        RequestBody::AgentList => {
            let mut machines = state.herdr.views();
            match principal.kind {
                PrincipalKind::Human => {}
                // A machine sees its own agents; an agent sees no other agent's status.
                PrincipalKind::Machine => machines.retain(|m| m.machine == principal.id),
                PrincipalKind::Agent => machines.clear(),
            }
            Response::Agents { machines }
        }
        RequestBody::TagAdd { thread_id, tag } => {
            change_tag(state, principal, &thread_id, &tag, true)
        }
        RequestBody::TagRemove { thread_id, tag } => {
            change_tag(state, principal, &thread_id, &tag, false)
        }
        RequestBody::TagList => match state.store.known_tags() {
            Ok(tags) => Response::Tags {
                tags: tags.into_iter().collect(),
            },
            Err(e) => map_store(&e),
        },
        RequestBody::RemindersList { include_completed } => {
            reminders_list(state, principal, include_completed)
        }
        RequestBody::ReminderTagAdd { reminder_id, tag } => {
            change_reminder_tag(state, principal, &reminder_id, &tag, true)
        }
        RequestBody::ReminderTagRemove { reminder_id, tag } => {
            change_reminder_tag(state, principal, &reminder_id, &tag, false)
        }
        RequestBody::EventsList { from } => events_list(state, principal, from),
        RequestBody::EventTagAdd { event_id, tag } => {
            change_event_tag(state, principal, &event_id, &tag, true)
        }
        RequestBody::EventTagRemove { event_id, tag } => {
            change_event_tag(state, principal, &event_id, &tag, false)
        }
        RequestBody::RemindersStatus => reminders_status(state),
        RequestBody::Execute {
            proposal_id,
            acknowledge_stale,
        } => execute_reminder(state, principal, &proposal_id, acknowledge_stale, now),
        RequestBody::ResolveExecuting { proposal_id, done } => {
            match state.store.resolve_executing(&proposal_id, done, now) {
                Ok(p) => {
                    state.audit(
                        &principal.id,
                        "proposal.resolve_executing",
                        if done {
                            "confirmed_done"
                        } else {
                            "returned_to_approved"
                        },
                        Some(&proposal_id),
                        None,
                    );
                    state.publish(Event::ProposalChanged {
                        proposal_id,
                        state: p.state,
                    });
                    Response::Proposal { proposal: p }
                }
                Err(e) => map_store(&e),
            }
        }
        RequestBody::Subscribe { .. } => Response::Subscribed,
    }
}

/// Reminder reads pass through the same scope filter as thread reads.
fn reminders_list(state: &State, principal: &Principal, include_completed: bool) -> Response {
    let tags = state.store.all_reminder_tags().unwrap_or_default();
    let machine_labels = machine_labels_for(state, principal);
    match state.store.list_reminders(include_completed) {
        Ok(rows) => Response::Reminders {
            reminders: rows
                .into_iter()
                .filter_map(|reminder| {
                    let own = tags.get(&reminder.id).cloned().unwrap_or_default();
                    visible_to(principal, &own, machine_labels.as_ref()).then_some(ReminderView {
                        reminder,
                        tags: own,
                    })
                })
                .collect(),
        },
        Err(e) => map_store(&e),
    }
}

fn change_reminder_tag(
    state: &State,
    actor: &Principal,
    reminder_id: &str,
    tag: &str,
    add: bool,
) -> Response {
    let result = if add {
        state.store.add_reminder_tag(reminder_id, tag)
    } else {
        state.store.remove_reminder_tag(reminder_id, tag)
    };
    match result {
        Ok(changed) => {
            if changed {
                state.audit(
                    &actor.id,
                    "reminder.tag",
                    if add { "added" } else { "removed" },
                    Some(reminder_id),
                    Some(&format!("tag={tag}")),
                );
            }
            Response::Done
        }
        Err(e) => {
            let reason = match &e {
                StoreError::NotFound(_) => "reminder_not_found",
                StoreError::Validation(m) if m.contains("not a known") => "unknown_tag",
                _ => "invalid_tag",
            };
            state.audit(
                &actor.id,
                "reminder.tag",
                "refused",
                Some(reminder_id),
                Some(reason),
            );
            map_store(&e)
        }
    }
}

/// Event reads pass through the same scope filter as thread and reminder reads.
fn events_list(state: &State, principal: &Principal, from: i64) -> Response {
    let tags = state.store.all_event_tags().unwrap_or_default();
    let machine_labels = machine_labels_for(state, principal);
    match state.store.list_events(from) {
        Ok(rows) => Response::Events {
            events: rows
                .into_iter()
                .filter_map(|event| {
                    let own = tags.get(&event.id).cloned().unwrap_or_default();
                    visible_to(principal, &own, machine_labels.as_ref())
                        .then_some(EventView { event, tags: own })
                })
                .collect(),
        },
        Err(e) => map_store(&e),
    }
}

fn change_event_tag(
    state: &State,
    actor: &Principal,
    event_id: &str,
    tag: &str,
    add: bool,
) -> Response {
    let result = if add {
        state.store.add_event_tag(event_id, tag)
    } else {
        state.store.remove_event_tag(event_id, tag)
    };
    match result {
        Ok(changed) => {
            if changed {
                state.audit(
                    &actor.id,
                    "event.tag",
                    if add { "added" } else { "removed" },
                    Some(event_id),
                    Some(&format!("tag={tag}")),
                );
            }
            Response::Done
        }
        Err(e) => {
            let reason = match &e {
                StoreError::NotFound(_) => "event_not_found",
                StoreError::Validation(m) if m.contains("not a known") => "unknown_tag",
                _ => "invalid_tag",
            };
            state.audit(
                &actor.id,
                "event.tag",
                "refused",
                Some(event_id),
                Some(reason),
            );
            map_store(&e)
        }
    }
}

fn reminders_status(state: &State) -> Response {
    let source = Source::Reminders;
    let registered = state.registered_sources().contains(&source);
    let engaged = state.store.kill_switch_engaged(&source).unwrap_or(true);
    let helper = state
        .reminders_helper
        .read()
        .ok()
        .and_then(|h| h.as_ref().cloned());
    let permission = helper
        .and_then(|h| {
            h.call(&haste_connector_reminders::RemindersRequest::Authorization { prompt: false })
                .ok()
        })
        .and_then(|r| match r {
            haste_connector_reminders::RemindersResponse::Authorization { status } => {
                Some(status.as_str().to_owned())
            }
            _ => None,
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let (total, open, missing) = match state.store.reminder_counts() {
        Ok(c) => c,
        Err(e) => return map_store(&e),
    };
    let needs_check = state
        .store
        .executing_proposals()
        .map_or(0, |p| p.len() as u64);
    Response::RemindersStatus {
        status: RemindersStatusView {
            registered,
            engaged,
            permission,
            total,
            open,
            missing,
            needs_check,
        },
    }
}

/// True when the failure certainly had no effect in Reminders, so the proposal may go back to
/// `approved`. Timeouts, crashes and garbled answers are ambiguous: the proposal stays
/// `executing` for the human to check.
fn certainly_no_effect(error: &CallError) -> bool {
    match error {
        CallError::CapabilityMissing(_)
        | CallError::KillSwitchEngaged
        | CallError::BreakerOpened(_) => true,
        CallError::TransientExhausted(_) => false,
        CallError::Connector(ConnectorError::Authorization(_) | ConnectorError::Unsupported(_)) => {
            true
        }
        CallError::Connector(ConnectorError::Other(m)) => {
            m.starts_with("reminders helper refused")
                || m.starts_with("reminders helper unavailable")
                // Google rejected the request (4xx) or the event is gone: nothing was written
                || m.starts_with("http 4")
                || m == "event not found"
        }
        CallError::Connector(_) => false,
    }
}

/// Human-only: run an approved reminder proposal through the Reminders connector.
fn execute_reminder(
    state: &State,
    actor: &Principal,
    proposal_id: &str,
    acknowledge_stale: bool,
    now: i64,
) -> Response {
    let started = match state
        .store
        .begin_execution(proposal_id, acknowledge_stale, now)
    {
        Ok(s) => s.proposal,
        Err(e) => {
            state.audit(
                &actor.id,
                "proposal.execute",
                "refused",
                Some(proposal_id),
                None,
            );
            return map_store(&e);
        }
    };
    state.publish(Event::ProposalChanged {
        proposal_id: proposal_id.to_owned(),
        state: started.state,
    });
    let (source, no_connector) = if started.payload.is_calendar() {
        (Source::GoogleCalendar, "no calendar connector")
    } else {
        (Source::Reminders, "no reminders connector")
    };
    let Some(connector) = state.connector_for(&source) else {
        return back_to_approved(state, actor, proposal_id, no_connector, now);
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let handle = tokio::runtime::Handle::current();
    let finished = match &started.payload {
        Payload::ReminderCreate {
            title,
            due,
            list,
            thread_id,
            ..
        } => {
            // The only thing written into Reminders besides the title: an identifier.
            let notes = thread_id.as_ref().map(|t| format!("haste:thread/{t}"));
            let new = NewReminder {
                title: title.clone(),
                due: *due,
                list: list.clone(),
                notes: notes.clone(),
            };
            match handle.block_on(guard.create_reminder(&new)) {
                Ok(receipt) => {
                    let created = Reminder {
                        id: String::new(),
                        external_id: receipt.external_id,
                        title: new.title,
                        notes_snippet: notes_snippet(notes.as_deref().unwrap_or("")),
                        due_at: new.due,
                        completed: false,
                        list_name: new.list.unwrap_or_default(),
                        remote_modified_at: now,
                        missing_since: None,
                        version: 0,
                    };
                    Ok(state
                        .store
                        .finish_create(proposal_id, &created, now)
                        .map(|r| r.0))
                }
                Err(e) => Err(e),
            }
        }
        Payload::ReminderComplete { reminder_id, .. } => {
            let external = match state.store.get_reminder(reminder_id) {
                Ok(Some(r)) => r.external_id,
                Ok(None) => {
                    return back_to_approved(state, actor, proposal_id, "reminder not found", now)
                }
                Err(e) => return map_store(&e),
            };
            match handle.block_on(guard.complete_reminder(&external)) {
                Ok(()) => Ok(state.store.finish_complete(proposal_id, now)),
                Err(e) => Err(e),
            }
        }
        Payload::CalendarCreate {
            calendar,
            title,
            start,
            end,
            all_day,
            attendees,
            description,
            ..
        } => {
            let new = NewEvent {
                calendar: calendar.clone(),
                title: title.clone(),
                start: *start,
                end: *end,
                all_day: *all_day,
                attendees: attendees.clone(),
                description: description.clone(),
            };
            match handle.block_on(guard.create_event(&new)) {
                Ok(receipt) => Ok(state
                    .store
                    .finish_event_create(proposal_id, &receipt.event, now)
                    .map(|r| r.0)),
                Err(e) => Err(e),
            }
        }
        Payload::CalendarUpdate {
            event_id, changes, ..
        } => {
            let external = match state.store.get_event(event_id) {
                Ok(Some(e)) => e.external_id,
                Ok(None) => {
                    return back_to_approved(state, actor, proposal_id, "event not found", now)
                }
                Err(e) => return map_store(&e),
            };
            let patch = EventPatch {
                title: changes.title.clone(),
                start: changes.start,
                end: changes.end,
                description: changes.description.clone(),
                location: changes.location.clone(),
            };
            match handle.block_on(guard.update_event(&external, &patch)) {
                Ok(receipt) => {
                    Ok(state
                        .store
                        .finish_event_update(proposal_id, &receipt.event, now))
                }
                Err(e) => Err(e),
            }
        }
        _ => {
            return back_to_approved(state, actor, proposal_id, "not an executable proposal", now);
        }
    };
    match finished {
        Ok(Ok(p)) => {
            // For calendar proposals the detail is the Google event id; never a title.
            state.audit(
                &actor.id,
                "proposal.execute",
                "sent",
                Some(proposal_id),
                p.result.as_deref().filter(|r| r.starts_with("event:")),
            );
            state.publish(Event::ProposalChanged {
                proposal_id: proposal_id.to_owned(),
                state: p.state,
            });
            Response::Proposal { proposal: p }
        }
        Ok(Err(e)) => {
            // The effect happened in Reminders but could not be recorded: leave it executing.
            state.audit(
                &actor.id,
                "proposal.execute",
                "needs_check",
                Some(proposal_id),
                Some("recorded_failed"),
            );
            map_store(&e)
        }
        Err(e) if certainly_no_effect(&e) => {
            let reason = match &e {
                CallError::Connector(ConnectorError::Authorization(_)) => "permission needed",
                CallError::KillSwitchEngaged => "kill switch engaged",
                _ => "write refused",
            };
            back_to_approved(state, actor, proposal_id, reason, now)
        }
        Err(_) => {
            state.audit(
                &actor.id,
                "proposal.execute",
                "needs_check",
                Some(proposal_id),
                Some("outcome_unknown"),
            );
            err(
                ErrorCode::Internal,
                "the outcome is unknown; the proposal stays 'executing' until you check the app",
            )
        }
    }
}

fn back_to_approved(
    state: &State,
    actor: &Principal,
    proposal_id: &str,
    reason: &str,
    now: i64,
) -> Response {
    match state.store.fail_execution(proposal_id, reason, now) {
        Ok(p) => {
            state.audit(
                &actor.id,
                "proposal.execute",
                "failed",
                Some(proposal_id),
                Some(reason),
            );
            state.publish(Event::ProposalChanged {
                proposal_id: proposal_id.to_owned(),
                state: p.state,
            });
            err(ErrorCode::Denied, reason)
        }
        Err(e) => map_store(&e),
    }
}

/// A machine principal's id is a herdr machine label; its scope is that machine's workspace labels.
fn machine_labels_for(state: &State, principal: &Principal) -> Option<BTreeSet<String>> {
    (principal.kind == PrincipalKind::Machine).then(|| {
        state
            .store
            .machine_labels(&principal.id)
            .unwrap_or_default()
    })
}

/// Turn an agent's herdr hints into `(project, origin_machine)`. They are found only if the
/// machine is connected and knows the pane; otherwise the proposal simply has neither.
fn resolve_hints(state: &State, hints: Option<&Hints>) -> (Option<String>, Option<String>) {
    let Some(Hints {
        machine: Some(machine),
        pane: Some(pane),
    }) = hints
    else {
        return (None, None);
    };
    match state.herdr.workspace_for(machine, pane) {
        Some(label) => (Some(label), Some(machine.clone())),
        None => (None, None),
    }
}

/// Human-only: add or remove a project tag. Every outcome is audited with a reason code.
fn change_tag(state: &State, actor: &Principal, thread_id: &str, tag: &str, add: bool) -> Response {
    let result = if add {
        state.store.add_thread_tag(thread_id, tag)
    } else {
        state.store.remove_thread_tag(thread_id, tag)
    };
    match result {
        Ok(changed) => {
            if changed {
                let outcome = if add { "added" } else { "removed" };
                state.audit(
                    &actor.id,
                    "tag.change",
                    outcome,
                    Some(thread_id),
                    Some(&format!("tag={tag}")),
                );
                state.publish(Event::ThreadChanged {
                    thread_id: thread_id.to_owned(),
                });
            }
            Response::Done
        }
        Err(e) => {
            let reason = match &e {
                StoreError::NotFound(_) => "thread_not_found",
                StoreError::Validation(m) if m.contains("not a known") => "unknown_tag",
                _ => "invalid_tag",
            };
            state.audit(
                &actor.id,
                "tag.change",
                "refused",
                Some(thread_id),
                Some(reason),
            );
            map_store(&e)
        }
    }
}

fn connector_status(state: &State) -> Response {
    let registered = state.registered_sources();
    let switches = match state.store.list_kill_switches() {
        Ok(s) => s,
        Err(e) => return map_store(&e),
    };
    let mut sources: Vec<Source> = registered.clone();
    for (source, _) in &switches {
        if !sources.contains(source) {
            sources.push(source.clone());
        }
    }
    sources.sort_by_key(Source::as_name);
    Response::Connectors {
        connectors: sources
            .into_iter()
            .map(|source| ConnectorView {
                engaged: switches.iter().any(|(s, on)| s == &source && *on),
                registered: registered.contains(&source),
                source: source.as_name(),
            })
            .collect(),
    }
}

/// Engage or release a kill switch. Releasing keeps the stored cursor.
fn set_switch(state: &State, actor: &Principal, source: &str, engaged: bool) -> Response {
    let source = Source::from(source.to_owned());
    if !state.registered_sources().contains(&source) {
        return err(
            ErrorCode::Validation,
            "no connector is registered for this source",
        );
    }
    if let Err(e) = state.store.set_kill_switch(&source, engaged) {
        return map_store(&e);
    }
    let (action, verb) = if engaged {
        ("connector.engage", "engaged")
    } else {
        ("connector.release", "released")
    };
    state.audit(&actor.id, action, verb, Some(&source.as_name()), None);
    state.publish(Event::Notice {
        message: format!("connector {} {verb}", source.as_name()),
    });
    Response::Done
}

/// After a human approved a draft proposal, create the provider-side draft (never a send).
/// The proposal stays `approved`; a failure is audited and shown as a notice.
fn create_provider_draft(
    state: &State,
    actor: &Principal,
    proposal: &haste_core::Proposal,
    now: i64,
) -> haste_core::Proposal {
    let Payload::Draft { to, body } = &proposal.payload else {
        return proposal.clone();
    };
    let Some(thread_id) = &proposal.source_thread_id else {
        return proposal.clone();
    };
    let Ok(Some((thread, _))) = state.store.get_thread(thread_id) else {
        return proposal.clone();
    };
    let Some(connector) = state.connector_for(&thread.source) else {
        return proposal.clone();
    };
    if !connector.capabilities().draft {
        return proposal.clone();
    }
    let draft = DraftToSend {
        thread_id: Some(thread.external_id.clone()),
        to: to.clone(),
        body: body.clone(),
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    match tokio::runtime::Handle::current().block_on(guard.create_draft(&draft)) {
        Ok(receipt) => {
            state.audit(
                &actor.id,
                "draft.create",
                "created",
                Some(&proposal.id),
                Some(&format!(
                    "source={} draft_id={}",
                    thread.source.as_name(),
                    receipt.draft_id
                )),
            );
            state
                .store
                .set_proposal_result(&proposal.id, &format!("draft:{}", receipt.draft_id), now)
                .unwrap_or_else(|_| proposal.clone())
        }
        Err(e) => {
            state.audit(
                &actor.id,
                "draft.create",
                "failed",
                Some(&proposal.id),
                Some(&format!("{e:?}")),
            );
            state.publish(Event::Notice {
                message: format!(
                    "provider draft for proposal {} was not created; it stays approved",
                    proposal.id
                ),
            });
            proposal.clone()
        }
    }
}

fn propose(
    state: &State,
    principal: &Principal,
    payload: Payload,
    thread_id: Option<String>,
    idempotency_key: String,
    hints: Option<&Hints>,
    now: i64,
) -> Response {
    let (project, origin_machine) = resolve_hints(state, hints);
    let payload = match prepare_reminder_payload(state, payload, thread_id.as_deref(), &project) {
        Ok(p) => p,
        Err(response) => return *response,
    };
    let new = NewProposal {
        principal_id: principal.id.clone(),
        idempotency_key,
        payload,
        thread_id,
        now,
        ttl_secs: state.config.proposal_ttl_secs,
        project,
        origin_machine,
    };
    match state.store.create_proposal(&new) {
        Ok((proposal, created, superseded)) => {
            if created {
                state.audit(
                    &principal.id,
                    "proposal.create",
                    "pending",
                    Some(&proposal.id),
                    None,
                );
                state.publish(Event::ProposalChanged {
                    proposal_id: proposal.id.clone(),
                    state: proposal.state,
                });
            }
            for old in superseded {
                state.audit(
                    &principal.id,
                    "proposal.supersede",
                    "superseded",
                    Some(&old.id),
                    None,
                );
                state.publish(Event::ProposalChanged {
                    proposal_id: old.id,
                    state: old.state,
                });
            }
            Response::Proposal { proposal }
        }
        Err(e) => map_store(&e),
    }
}

/// Fill in and validate reminder payloads at proposal time: the project must be a known tag,
/// the source thread is carried into the payload, and a completion records the reminder
/// version it was proposed against (so a later change makes it stale).
fn prepare_reminder_payload(
    state: &State,
    payload: Payload,
    thread_id: Option<&str>,
    hinted_project: &Option<String>,
) -> Result<Payload, Box<Response>> {
    // Refused fields, empty titles and impossible times never reach the store as pending.
    if let Err(why) = payload.validate() {
        return Err(Box::new(err(ErrorCode::Validation, why)));
    }
    match payload {
        Payload::CalendarCreate {
            calendar,
            title,
            start,
            end,
            all_day,
            attendees,
            description,
            project,
        } => {
            let project = project.or_else(|| hinted_project.clone());
            check_known_project(state, project.as_deref())?;
            Ok(Payload::CalendarCreate {
                calendar,
                title,
                start,
                end,
                all_day,
                attendees,
                description,
                project,
            })
        }
        Payload::CalendarUpdate {
            event_id,
            changes,
            project,
            event_version,
        } => match state.store.get_event(&event_id) {
            Ok(Some(e)) => Ok(Payload::CalendarUpdate {
                event_id,
                changes,
                project: project.or_else(|| hinted_project.clone()),
                event_version: event_version.or(Some(e.version)),
            }),
            Ok(None) => Err(Box::new(err(ErrorCode::NotFound, "event not found"))),
            Err(e) => Err(Box::new(map_store(&e))),
        },
        Payload::ReminderCreate {
            title,
            due,
            list,
            project,
            thread_id: payload_thread,
        } => {
            let project = project.or_else(|| hinted_project.clone());
            if let Some(p) = &project {
                match state.store.known_tags() {
                    Ok(known) if known.contains(p) => {}
                    Ok(_) => {
                        return Err(Box::new(err(
                            ErrorCode::Validation,
                            &format!("'{p}' is not a known project (workspace) label"),
                        )))
                    }
                    Err(e) => return Err(Box::new(map_store(&e))),
                }
            }
            Ok(Payload::ReminderCreate {
                title,
                due,
                list,
                project,
                thread_id: payload_thread.or_else(|| thread_id.map(str::to_owned)),
            })
        }
        Payload::ReminderComplete {
            reminder_id,
            project,
            reminder_version,
        } => match state.store.get_reminder(&reminder_id) {
            Ok(Some(r)) => Ok(Payload::ReminderComplete {
                reminder_id,
                project: project.or_else(|| hinted_project.clone()),
                reminder_version: reminder_version.or(Some(r.version)),
            }),
            Ok(None) => Err(Box::new(err(ErrorCode::NotFound, "reminder not found"))),
            Err(e) => Err(Box::new(map_store(&e))),
        },
        other => Ok(other),
    }
}

/// A project given with a proposal must be a known workspace label.
fn check_known_project(state: &State, project: Option<&str>) -> Result<(), Box<Response>> {
    let Some(p) = project else {
        return Ok(());
    };
    match state.store.known_tags() {
        Ok(known) if known.contains(p) => Ok(()),
        Ok(_) => Err(Box::new(err(
            ErrorCode::Validation,
            &format!("'{p}' is not a known project (workspace) label"),
        ))),
        Err(e) => Err(Box::new(map_store(&e))),
    }
}

fn note_append(
    state: &State,
    principal: &Principal,
    path: &str,
    text: &str,
    key: &str,
    now: i64,
) -> Response {
    if key.trim().is_empty() {
        return err(ErrorCode::Validation, "idempotency key is required");
    }
    if !state.remember_note_key(&principal.id, key) {
        return Response::Appended {
            path: path.to_owned(),
            bytes: 0,
        };
    }
    match guarded_note_write(state, &principal.id, path, now, || {
        append_note(&state.config.notes_root, path, &principal.id, text, now)
    }) {
        Ok(bytes) => Response::Appended {
            path: path.to_owned(),
            bytes,
        },
        Err(response) => *response,
    }
}

/// Refuse a note action: one audit row with the path and the reason code, never any text.
pub(crate) fn note_denied(state: &State, actor: &str, path: &str, denial: NoteDenial) -> Response {
    state.audit(
        actor,
        "note.append",
        "denied",
        Some(path),
        Some(denial.as_str()),
    );
    let code = match denial {
        NoteDenial::SizeLimit | NoteDenial::BodyRejected => ErrorCode::Validation,
        _ => ErrorCode::Denied,
    };
    err(code, denial.as_str())
}

/// Whether `projects/<name>/…` names a known workspace (by slug); other paths always pass.
fn project_is_known(state: &State, path: &str) -> bool {
    let Some(project) = note_project(path) else {
        return true;
    };
    state
        .store
        .known_tags()
        .is_ok_and(|tags| tags.iter().any(|label| slugify(label) == project))
}

/// Every note write goes through here: project check, per-day budget, the write itself, and
/// exactly one audit row (`written` with the byte count, `denied` with a reason code, or
/// `io_error`).
pub(crate) fn guarded_note_write(
    state: &State,
    actor: &str,
    path: &str,
    now: i64,
    write: impl FnOnce() -> Result<u64, AppendError>,
) -> Result<u64, Box<Response>> {
    if !project_is_known(state, path) {
        return Err(Box::new(note_denied(
            state,
            actor,
            path,
            NoteDenial::UnknownProject,
        )));
    }
    if !state.note_budget_left(actor, now) {
        return Err(Box::new(note_denied(
            state,
            actor,
            path,
            NoteDenial::SizeLimit,
        )));
    }
    match write() {
        Ok(bytes) => {
            state.note_budget_use(actor, now);
            state.audit(
                actor,
                "note.append",
                "written",
                Some(path),
                Some(&format!("bytes={bytes}")),
            );
            Ok(bytes)
        }
        Err(AppendError::Denied(d)) => Err(Box::new(note_denied(state, actor, path, d))),
        Err(AppendError::Io(m)) => {
            state.audit(actor, "note.append", "io_error", Some(path), None);
            Err(Box::new(err(ErrorCode::Internal, &m)))
        }
    }
}

fn register(
    state: &State,
    actor: &Principal,
    principal_id: &str,
    kind: PrincipalKind,
    allowed: &[String],
) -> Response {
    if kind == PrincipalKind::Human {
        return err(
            ErrorCode::Validation,
            "only agent and machine principals can be registered",
        );
    }
    let mut actions = Vec::new();
    for name in allowed {
        match Action::parse(name) {
            Some(a) if is_human_only(a) => {
                return err(
                    ErrorCode::Validation,
                    &format!("action {name} cannot be granted to agents"),
                );
            }
            Some(a) => actions.push(a),
            None => return err(ErrorCode::Validation, &format!("unknown action {name}")),
        }
    }
    match state.store.register_principal(principal_id, kind, &actions) {
        Ok(token) => {
            state.audit(
                &actor.id,
                "principal.register",
                "ok",
                Some(principal_id),
                None,
            );
            Response::Registered {
                principal_id: principal_id.to_owned(),
                token,
            }
        }
        Err(e) => map_store(&e),
    }
}

fn send(state: &State, actor: &Principal, proposal_id: &str, ack: bool, now: i64) -> Response {
    let proposal = match state.store.prepare_send(proposal_id, ack) {
        Ok(p) => p,
        Err(e) => {
            state.audit(
                &actor.id,
                "proposal.send",
                "refused",
                Some(proposal_id),
                None,
            );
            return map_store(&e);
        }
    };
    let Payload::Draft { to, body } = &proposal.payload else {
        return err(ErrorCode::IllegalTransition, "only drafts can be sent");
    };
    let Some(thread_id) = proposal.source_thread_id.clone() else {
        return err(
            ErrorCode::Validation,
            "proposal has no source thread to send through",
        );
    };
    let source = match state.store.get_thread(&thread_id) {
        Ok(Some((t, _))) => t.source,
        Ok(None) => return err(ErrorCode::NotFound, "source thread not found"),
        Err(e) => return map_store(&e),
    };
    let Some(connector) = state.connector_for(&source) else {
        return err(
            ErrorCode::Validation,
            "no connector registered for this source",
        );
    };
    let draft = DraftToSend {
        thread_id: Some(thread_id),
        to: to.clone(),
        body: body.clone(),
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let outcome = tokio::runtime::Handle::current().block_on(guard.send(&draft));
    match outcome {
        Ok(receipt) => match state.store.complete_send(
            proposal_id,
            &format!("sent:{}", receipt.external_id),
            now,
        ) {
            Ok(p) => {
                state.audit(&actor.id, "proposal.send", "sent", Some(proposal_id), None);
                state.publish(Event::ProposalChanged {
                    proposal_id: proposal_id.to_owned(),
                    state: p.state,
                });
                Response::Proposal { proposal: p }
            }
            Err(e) => map_store(&e),
        },
        Err(CallError::CapabilityMissing(_)) => err(ErrorCode::Denied, "connector cannot send"),
        Err(CallError::KillSwitchEngaged) => {
            err(ErrorCode::Denied, "connector kill switch engaged")
        }
        Err(CallError::BreakerOpened(_)) => {
            err(ErrorCode::Denied, "connector stopped by circuit breaker")
        }
        Err(other) => err(ErrorCode::Internal, &format!("send failed: {other:?}")),
    }
}
