//! Request dispatch: authenticate, token-subset check, policy gate, execute or propose.

use std::collections::BTreeSet;

use haste_core::audit::read_recent;
use haste_core::connector::{
    CallError, ConnectorError, DraftToSend, EventPatch, Guarded, NewEvent, NewReminder,
};
use haste_core::herdr::visible_to;
use haste_core::ignore;
use haste_core::model::{notes_snippet, Reminder, Source};
use haste_core::notes::{note_project, slugify, NoteDenial};
use haste_core::policy::{
    evaluate, resolve_note_path, Action, Matrix, NotePathError, Outcome, Principal, PrincipalKind,
    Target,
};
use haste_core::proposal::{NoteEntry, Payload};
use haste_core::send::{
    chat_send_digest, classify as classify_send, recipients_digest, reply_subject,
    validate_recipients, validate_subject, SendOutcome, SendScope, CHAT_MAX_SEND_BYTES,
    MAX_SEND_BODY_BYTES,
};
use haste_core::time::now_secs;
use haste_proto::{
    AuditLine, ConnectorView, ErrorCode, Event, EventView, Hints, NoteLogRow, ReminderView,
    RemindersStatusView, Request, RequestBody, Response, SendPreviewView, ThreadView,
    PROTOCOL_VERSION,
};
use haste_store::{AuthError, NewProposal, StoreError, ThreadFilter};

use crate::notes::{
    append_note, append_person_entry, append_reference, person_note_path, reference_path,
    valid_reference_id, AppendError,
};
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
        RequestBody::NoteAppend { .. }
        | RequestBody::NotePerson { .. }
        | RequestBody::NoteReference(_) => Action::NoteAppend,
        RequestBody::Approve { .. } => Action::Approve,
        RequestBody::Reject { .. } => Action::Reject,
        // Running an approved reminder proposal is as human-only as sending a draft.
        RequestBody::Send { .. }
        | RequestBody::SendPreview { .. }
        | RequestBody::Execute { .. } => Action::Send,
        RequestBody::RegisterPrincipal { .. }
        | RequestBody::RevokeToken { .. }
        | RequestBody::AuditRecent { .. }
        | RequestBody::ConnectorStatus
        | RequestBody::ConnectorRelease { .. }
        | RequestBody::ConnectorEngage { .. }
        | RequestBody::ConnectorResync { .. }
        | RequestBody::AttachmentSave { .. }
        | RequestBody::ReactionAdd { .. }
        | RequestBody::ConnectorAuthorized { .. }
        | RequestBody::TagAdd { .. }
        | RequestBody::TagRemove { .. }
        | RequestBody::TagList
        | RequestBody::ThreadRefresh { .. }
        | RequestBody::ReminderNew { .. }
        | RequestBody::ReminderEdit { .. }
        | RequestBody::ReminderTagAdd { .. }
        | RequestBody::ReminderTagRemove { .. }
        | RequestBody::NoteMode { .. }
        | RequestBody::NoteShow { .. }
        | RequestBody::NoteLog { .. }
        | RequestBody::EventTagAdd { .. }
        | RequestBody::EventTagRemove { .. }
        | RequestBody::RemindersStatus
        | RequestBody::RemindersAuth
        | RequestBody::TriageSet { .. }
        | RequestBody::FilterList
        | RequestBody::IgnoreList
        | RequestBody::IgnoreMute { .. }
        | RequestBody::ReadSet { .. }
        | RequestBody::SenderRules
        | RequestBody::EventShift { .. }
        | RequestBody::ReminderDuration { .. }
        | RequestBody::ReminderLater { .. }
        | RequestBody::ReminderPriority { .. }
        | RequestBody::ReminderEarly { .. }
        | RequestBody::MutedDelete { .. }
        | RequestBody::ThreadDelete { .. }
        | RequestBody::RuleChange { .. }
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
        RequestBody::ThreadRefresh { .. } => "thread_refresh",
        RequestBody::ReminderNew { .. } => "reminder_new",
        RequestBody::ReminderEdit { .. } => "reminder_edit",
        RequestBody::ConnectorStatus => "connector_status",
        RequestBody::ConnectorRelease { .. } => "connector_release",
        RequestBody::ConnectorEngage { .. } => "connector_engage",
        RequestBody::ConnectorResync { .. } => "connector_resync",
        RequestBody::AttachmentSave { .. } => "attachment_save",
        RequestBody::ReactionAdd { .. } => "reaction_add",
        RequestBody::ConnectorAuthorized { .. } => "connector_authorized",
        RequestBody::RegisterPrincipal { .. } => "register_principal",
        RequestBody::RevokeToken { .. } => "revoke_token",
        RequestBody::AuditRecent { .. } => "audit_recent",
        RequestBody::Subscribe { .. } => "subscribe",
        RequestBody::Approve { .. } => "approve",
        RequestBody::Reject { .. } => "reject",
        RequestBody::Send { .. } => "send",
        RequestBody::SendPreview { .. } => "send_preview",
        RequestBody::Execute { .. } => "execute",
        RequestBody::ReminderTagAdd { .. } => "reminder_tag_add",
        RequestBody::EventTagAdd { .. } => "event_tag_add",
        RequestBody::NoteMode { .. } => "note_mode",
        RequestBody::NoteShow { .. } => "note_show",
        RequestBody::NoteLog { .. } => "note_log",
        RequestBody::EventTagRemove { .. } => "event_tag_remove",
        RequestBody::ReminderTagRemove { .. } => "reminder_tag_remove",
        RequestBody::RemindersStatus => "reminders_status",
        RequestBody::RemindersAuth => "reminders_auth",
        RequestBody::ResolveExecuting { .. } => "resolve_executing",
        RequestBody::TriageSet { .. } => "triage_set",
        RequestBody::FilterList => "filter_list",
        RequestBody::IgnoreList => "ignore_list",
        RequestBody::IgnoreMute { .. } => "ignore_mute",
        RequestBody::ReadSet { .. } => "read_set",
        RequestBody::SenderRules => "sender_rules",
        RequestBody::EventShift { .. } => "event_shift",
        RequestBody::ReminderDuration { .. } => "reminder_duration",
        RequestBody::ReminderLater { .. } => "reminder_later",
        RequestBody::ReminderPriority { .. } => "reminder_priority",
        RequestBody::ReminderEarly { .. } => "reminder_early",
        RequestBody::MutedDelete { .. } => "muted_delete",
        RequestBody::ThreadDelete { .. } => "thread_delete",
        RequestBody::RuleChange { .. } => "rule_change",
        _ => "request",
    }
}

fn is_human_only(action: Action) -> bool {
    matches!(
        action,
        Action::Send | Action::Approve | Action::Reject | Action::Manage
    )
}

fn code_label(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::UnsupportedVersion => "unsupported_version",
        ErrorCode::Unauthenticated => "unauthenticated",
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::Denied => "denied",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Validation => "validation",
        ErrorCode::IllegalTransition => "illegal_transition",
        ErrorCode::StaleNeedsAcknowledgement => "stale_needs_acknowledgement",
        ErrorCode::Internal => "internal",
    }
}

/// Handle one request and log it: failures at `warn` (internal errors at `error`), successes at
/// `debug` (set `HASTE_LOG=debug` to see them). Only the fixed request name, the outcome class and
/// the duration are logged, never request data.
pub fn dispatch(state: &State, request: Request) -> Response {
    let name = request_name(&request.body);
    let timer = haste_core::log::Timer::start();
    let response = dispatch_inner(state, request);
    let (level, outcome) = match &response {
        Response::Error { error } => (
            if error.code == ErrorCode::Internal {
                haste_core::log::Level::Error
            } else {
                haste_core::log::Level::Warn
            },
            code_label(error.code),
        ),
        _ => (haste_core::log::Level::Debug, "ok"),
    };
    haste_core::log::event(
        level,
        "daemon",
        "request",
        &[
            haste_core::log::id("request", name),
            haste_core::log::l("outcome", outcome),
            haste_core::log::n("ms", timer.ms()),
        ],
    );
    response
}

fn dispatch_inner(state: &State, request: Request) -> Response {
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
        // The person note's real path depends on the store (slug collisions); the gate only
        // needs its class, so it sees a representative `people/` path.
        RequestBody::NotePerson { .. } => Target::NotePath("people/person.md".into()),
        RequestBody::NoteReference(args) => Target::NotePath(
            reference_path(&args.topic, args.project.as_deref())
                .unwrap_or_else(|| "tasks/topic.md".into()),
        ),
        _ => Target::None,
    };
    let mut matrix = Matrix::new();
    matrix.grant_defaults(&principal);
    matrix.set_note_mode(&principal.id, auth.note_mode);
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
    // A pending decision for a note request means the principal is in `approval` mode.
    let queue_notes = decision.outcome == Outcome::Pending;
    execute(state, &principal, request.body, queue_notes)
}

fn execute(state: &State, principal: &Principal, body: RequestBody, queue_notes: bool) -> Response {
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
            let lasts = state.store.last_messages().unwrap_or_default();
            let with_files = state.store.threads_with_attachments().unwrap_or_default();
            let flags = MailFlags::load(state);
            match state.store.list_threads(&filter) {
                Ok(rows) => Response::Threads {
                    threads: rows
                        .into_iter()
                        .filter_map(|(thread, triage)| {
                            let own = tags.get(&thread.id).cloned().unwrap_or_default();
                            // The one scope filter for thread reads: a machine sees only its projects.
                            visible_to(principal, &own, machine_labels.as_ref()).then_some(
                                ThreadView {
                                    unread: flags.unread.contains(&thread.id),
                                    replied: flags.replied.contains(&thread.id),
                                    sender_known: MailFlags::accepted(&thread),
                                    sender: flags.senders.get(&thread.id).cloned(),
                                    last: lasts.get(&thread.id).map(|(sender, body)| {
                                        haste_proto::LastMessage {
                                            sender: sender.clone(),
                                            snippet: snippet(body),
                                            has_attachment: with_files.contains(&thread.id),
                                        }
                                    }),
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
                        // only the human's own screen needs it
                        own: (principal.kind == PrincipalKind::Human)
                            .then(|| state.connector_for(&thread.source))
                            .flatten()
                            .and_then(|c| c.own_address()),
                        view: ThreadView {
                            unread: state
                                .store
                                .unread_threads()
                                .is_ok_and(|u| u.contains(&thread.id)),
                            replied: false,
                            sender_known: true,
                            sender: None,
                            thread,
                            triage,
                            tags: own,
                            last: None,
                        },
                        messages,
                    },
                    Err(e) => map_store(&e),
                }
            }
            Ok(None) => err(ErrorCode::NotFound, "thread not found"),
            Err(e) => map_store(&e),
        },
        RequestBody::ThreadRefresh { thread_id, older } => match state.store.get_thread(&thread_id)
        {
            Ok(Some((thread, _))) => {
                // Opening a thread may wake its source's sync, at most once per interval.
                if state.connector_for(&thread.source).is_some()
                    && state.refresh_due(&thread.source)
                {
                    state.wake_sync(&thread.source);
                }
                // and its earlier history is read once, if the provider can give it
                if state.connector_for(&thread.source).is_some() {
                    state.queue_history(&thread.source, &thread.id, older);
                }
                Response::Done
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
        } => {
            if queue_notes {
                queue_note(
                    state,
                    principal,
                    NoteEntry::Text { path, text },
                    &idempotency_key,
                    now,
                )
            } else {
                note_append(state, principal, &path, &text, &idempotency_key, now)
            }
        }
        RequestBody::NotePerson {
            person_id,
            text,
            idempotency_key,
        } => {
            if queue_notes {
                match state.store.get_person(&person_id) {
                    Ok(Some(_)) => queue_note(
                        state,
                        principal,
                        NoteEntry::Person { person_id, text },
                        &idempotency_key,
                        now,
                    ),
                    Ok(None) => err(ErrorCode::NotFound, "person not found"),
                    Err(e) => map_store(&e),
                }
            } else {
                note_person(state, principal, &person_id, &text, &idempotency_key, now)
            }
        }
        RequestBody::NoteReference(args) => {
            if queue_notes {
                match check_reference(state, &args) {
                    Ok(_) => queue_note(
                        state,
                        principal,
                        NoteEntry::Reference {
                            kind: args.kind.as_str().to_owned(),
                            id: args.id.clone(),
                            topic: args.topic.clone(),
                            project: args.project.clone(),
                        },
                        &args.idempotency_key,
                        now,
                    ),
                    Err(response) => *response,
                }
            } else {
                note_reference(state, principal, &args, now)
            }
        }
        RequestBody::NoteShow { path } => note_show(state, principal, &path),
        RequestBody::NoteLog { since } => note_log(state, since),
        RequestBody::NoteMode { principal_id, mode } => {
            match state.store.set_note_mode(&principal_id, mode) {
                Ok(()) => {
                    state.audit(
                        &principal.id,
                        "note.mode",
                        "changed",
                        Some(&principal_id),
                        Some(&format!("mode={}", mode.as_str())),
                    );
                    Response::Done
                }
                Err(e) => map_store(&e),
            }
        }
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
            // Approving an archive proposal archives for real: the provider first, then here.
            if let Ok(p) = state.store.get_proposal(&proposal_id) {
                if matches!(p.payload, Payload::Archive) {
                    if let Some(thread_id) = &p.source_thread_id {
                        if let Err(response) = archive_upstream(state, principal, thread_id) {
                            return *response;
                        }
                    }
                }
            }
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
                    let proposal = if out.proposal.payload.is_note() {
                        // Approving a queued note runs the write, once, as the original agent.
                        run_note_proposal(state, &out.proposal, now)
                    } else {
                        create_provider_draft(state, principal, &out.proposal, now)
                    };
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
            recipients_digest,
        } => send(
            state,
            principal,
            &proposal_id,
            acknowledge_stale,
            &recipients_digest,
            now,
        ),
        RequestBody::SendPreview { proposal_id } => send_preview(state, &proposal_id, now),
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
        RequestBody::ConnectorResync { source } => connector_resync(state, principal, &source),
        RequestBody::AttachmentSave {
            message_id,
            index,
            dir,
        } => attachment_save(state, principal, &message_id, index, dir.as_deref()),
        RequestBody::ReactionAdd { message_id, key } => {
            reaction_add(state, principal, &message_id, &key)
        }
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
        RequestBody::ReminderNew {
            title,
            due,
            notes,
            repeat,
            tags,
            thread_id,
            minutes,
            priority,
            early,
        } => reminder_write(
            state,
            principal,
            ReminderWrite::New {
                title,
                due,
                notes,
                repeat,
                tags,
                thread_id,
                minutes,
                priority,
                early,
            },
        ),
        RequestBody::ReminderEdit {
            reminder_id,
            title,
            due,
            append_notes,
            repeat,
            set_repeat,
        } => reminder_write(
            state,
            principal,
            ReminderWrite::Edit {
                reminder_id,
                title,
                due,
                append_notes,
                repeat,
                set_repeat,
            },
        ),
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
        RequestBody::RemindersAuth => reminders_auth(state, principal),
        RequestBody::IgnoreList => {
            let (list, errors) = read_ignore(&state.config.data_dir);
            Response::Ignore {
                patterns: list.patterns(),
                errors,
            }
        }
        RequestBody::IgnoreMute { thread_id } => ignore_mute(state, principal, &thread_id),
        RequestBody::MutedDelete { thread_id } => muted_delete(state, principal, &thread_id),
        RequestBody::ThreadDelete { thread_id } => {
            delete_local(state, principal, &thread_id, "thread.delete")
        }
        RequestBody::ReminderDuration {
            reminder_id,
            minutes,
        } => reminder_duration(state, principal, &reminder_id, minutes),
        RequestBody::ReminderLater {
            reminder_id,
            set_aside,
            until,
        } => reminder_later(state, principal, &reminder_id, set_aside, until),
        RequestBody::ReminderPriority {
            reminder_id,
            priority,
        } => reminder_priority(state, principal, &reminder_id, priority),
        RequestBody::ReminderEarly {
            reminder_id,
            minutes,
        } => reminder_early(state, principal, &reminder_id, minutes),
        RequestBody::EventShift { event_id, times } => {
            event_shift(state, principal, &event_id, times)
        }
        RequestBody::SenderRules => {
            let (mute, mut errors) = read_rule_file(&state.config.data_dir, ignore::IGNORE_FILE);
            let (priority, e2) = read_rule_file(&state.config.data_dir, ignore::PRIORITY_FILE);
            let (allowed, e3) = read_rule_file(&state.config.data_dir, ignore::ALLOWED_FILE);
            errors.extend(e2);
            errors.extend(e3);
            Response::Rules {
                mute: mute.patterns(),
                priority: priority.patterns(),
                allowed: allowed.patterns(),
                errors,
            }
        }
        RequestBody::RuleChange {
            thread_id,
            rule,
            add,
            domain,
        } => rule_change(state, principal, &thread_id, rule, add, domain),
        RequestBody::ReadSet { thread_id, read } => read_set(state, principal, &thread_id, read),
        RequestBody::FilterList => {
            let (filters, error) = crate::filters::load(&state.config.data_dir);
            Response::Filters { filters, error }
        }
        RequestBody::TriageSet {
            thread_id,
            priority,
            set_aside,
            snoozed_until,
            archived,
        } => triage_set(
            state,
            principal,
            &thread_id,
            &haste_store::TriageUpdate {
                priority,
                snoozed_until,
                set_aside,
                archived,
            },
            now,
        ),
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
    let repeats = state.store.reminder_repeats().unwrap_or_default();
    let durations = state.store.reminder_durations().unwrap_or_default();
    let priorities = state.store.reminder_priorities().unwrap_or_default();
    let earlies = state.store.reminder_earlies().unwrap_or_default();
    let laters = state.store.reminder_laters().unwrap_or_default();
    let machine_labels = machine_labels_for(state, principal);
    match state.store.list_reminders(include_completed) {
        Ok(rows) => Response::Reminders {
            reminders: rows
                .into_iter()
                .filter_map(|reminder| {
                    let own = tags.get(&reminder.id).cloned().unwrap_or_default();
                    let (set_aside, shown) = laters
                        .get(&reminder.external_id)
                        .copied()
                        .unwrap_or((false, None));
                    let mut reminder = reminder;
                    if let Some(due) = shown {
                        reminder.due_at = Some(due);
                    }
                    visible_to(principal, &own, machine_labels.as_ref()).then_some(ReminderView {
                        set_aside,
                        snoozed: shown.is_some(),
                        repeat: repeats.get(&reminder.external_id).copied(),
                        minutes: durations
                            .get(&reminder.external_id)
                            .copied()
                            .unwrap_or(haste_proto::DEFAULT_TASK_MINUTES),
                        early: earlies.get(&reminder.external_id).copied(),
                        priority: priorities
                            .get(&reminder.external_id)
                            .and_then(|r| haste_proto::TaskPriority::from_rank(*r))
                            .unwrap_or_default(),
                        reminder,
                        tags: own,
                    })
                })
                .collect(),
        },
        Err(e) => map_store(&e),
    }
}

enum ReminderWrite {
    New {
        title: String,
        due: Option<i64>,
        notes: Option<String>,
        repeat: Option<haste_core::model::RepeatRule>,
        tags: Vec<String>,
        thread_id: Option<String>,
        minutes: Option<u32>,
        priority: Option<haste_proto::TaskPriority>,
        early: Option<u32>,
    },
    Edit {
        reminder_id: String,
        title: String,
        due: Option<i64>,
        append_notes: Option<String>,
        repeat: Option<haste_core::model::RepeatRule>,
        set_repeat: bool,
    },
}

/// The human's own reminder change: run through the connector guard (so the kill switch and the
/// breaker still apply), then ask the sync loop to refresh the mirror. Titles and notes are never
/// audited; only the action and the reminder's haste id.
fn reminder_write(state: &State, actor: &Principal, write: ReminderWrite) -> Response {
    let title_ok = |t: &str| !t.trim().is_empty() && t.chars().count() <= 500;
    let (action, object) = match &write {
        ReminderWrite::New { .. } => ("reminder.new", None),
        ReminderWrite::Edit { reminder_id, .. } => ("reminder.edit", Some(reminder_id.clone())),
    };
    let valid = match &write {
        ReminderWrite::New {
            title,
            notes,
            tags,
            minutes,
            early,
            ..
        } => {
            title_ok(title)
                && notes.as_ref().is_none_or(|n| n.len() <= 20_000)
                && tags.len() <= 20
                && minutes.is_none_or(haste_proto::valid_task_minutes)
                && early.is_none_or(|m| m <= haste_proto::MAX_EARLY_MINUTES)
        }
        ReminderWrite::Edit {
            title,
            append_notes,
            ..
        } => title_ok(title) && append_notes.as_ref().is_none_or(|n| n.len() <= 20_000),
    };
    if !valid {
        return err(ErrorCode::Validation, "invalid title or notes");
    }
    let Some(connector) = state.connector_for(&Source::Reminders) else {
        return err(ErrorCode::NotFound, "no reminders connector");
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let handle = tokio::runtime::Handle::current();
    let result = match write {
        ReminderWrite::New {
            title,
            due,
            notes,
            repeat,
            tags,
            thread_id,
            minutes,
            priority,
            early,
        } => handle
            .block_on(guard.create_reminder(&NewReminder {
                title: title.clone(),
                due,
                list: None,
                notes: notes.clone(),
                repeat,
                priority,
                early: early.filter(|m| *m != 0 && due.is_some()),
            }))
            .map(|receipt| {
                // show it at once, with its tags, its thread and its repeat rule
                let created = Reminder {
                    id: String::new(),
                    external_id: receipt.external_id,
                    title,
                    notes_snippet: notes_snippet(notes.as_deref().unwrap_or("")),
                    due_at: due,
                    completed: false,
                    list_name: String::new(),
                    remote_modified_at: now_secs(),
                    missing_since: None,
                    version: 0,
                };
                let _ = state.store.record_created_reminder(
                    &created,
                    &tags,
                    thread_id.as_deref(),
                    repeat,
                );
                // how long it takes on the calendar, when not the default
                if let Some(m) = minutes.filter(|m| *m != haste_proto::DEFAULT_TASK_MINUTES) {
                    let _ = state
                        .store
                        .set_reminder_duration(&created.external_id, Some(m));
                }
                if let Some(m) = early.filter(|m| *m != 0 && due.is_some()) {
                    let _ = state
                        .store
                        .set_reminder_earlies(&[(created.external_id.clone(), Some(m))]);
                }
                if let Some(p) = priority.filter(|p| *p != haste_proto::TaskPriority::Middle) {
                    let _ = state
                        .store
                        .set_reminder_priority(&created.external_id, Some(p.rank()));
                }
            }),
        ReminderWrite::Edit {
            reminder_id,
            title,
            due,
            append_notes,
            repeat,
            set_repeat,
        } => {
            let external = match state.store.get_reminder(&reminder_id) {
                Ok(Some(r)) => r.external_id,
                Ok(None) => return err(ErrorCode::NotFound, "reminder not found"),
                Err(e) => return map_store(&e),
            };
            let patch_title = title.clone();
            let done = handle.block_on(guard.update_reminder(
                &external,
                &haste_core::connector::ReminderPatch {
                    title,
                    due,
                    append_notes,
                    repeat,
                    set_repeat,
                },
            ));
            // the edit shows at once; the next sync finds the same values
            if done.is_ok() {
                let _ = state
                    .store
                    .apply_reminder_edit(&reminder_id, &patch_title, due);
                // the edit wrote its own due time: a snooze no longer overrides it
                if let Ok(laters) = state.store.reminder_laters() {
                    if let Some((aside, Some(_))) = laters.get(&external) {
                        let _ = state.store.set_reminder_later(&external, *aside, None);
                    }
                }
                if set_repeat {
                    let _ = state.store.set_reminder_repeats(&[(external, repeat)]);
                }
            }
            done
        }
    };
    match result {
        Ok(()) => {
            state.audit(&actor.id, action, "ok", object.as_deref(), None);
            state.wake_sync(&Source::Reminders);
            Response::Done
        }
        Err(e) => {
            state.audit(&actor.id, action, "error", object.as_deref(), None);
            err(ErrorCode::Internal, &format!("{e:?}"))
        }
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
/// How long a task takes on the calendar, here only: one of the offered lengths.
fn reminder_duration(
    state: &State,
    actor: &Principal,
    reminder_id: &str,
    minutes: u32,
) -> Response {
    if !haste_proto::valid_task_minutes(minutes) {
        return err(
            ErrorCode::Validation,
            "a task takes 5 minutes to 4 hours, in steps of 5 minutes",
        );
    }
    let external = match state.store.get_reminder(reminder_id) {
        Ok(Some(r)) => r.external_id,
        Ok(None) => return err(ErrorCode::NotFound, "reminder not found"),
        Err(e) => return map_store(&e),
    };
    let stored = (minutes != haste_proto::DEFAULT_TASK_MINUTES).then_some(minutes);
    if let Err(e) = state.store.set_reminder_duration(&external, stored) {
        return map_store(&e);
    }
    state.audit(
        &actor.id,
        "reminder.duration",
        "set",
        Some(reminder_id),
        None,
    );
    Response::Done
}

/// Set a task aside or snooze it, here only: nothing is written to Reminders.
fn reminder_later(
    state: &State,
    actor: &Principal,
    reminder_id: &str,
    set_aside: bool,
    until: Option<i64>,
) -> Response {
    if set_aside && until.is_some() {
        return err(
            ErrorCode::Validation,
            "a task is set aside or snoozed, not both",
        );
    }
    let external = match state.store.get_reminder(reminder_id) {
        Ok(Some(r)) => r.external_id,
        Ok(None) => return err(ErrorCode::NotFound, "reminder not found"),
        Err(e) => return map_store(&e),
    };
    if let Err(e) = state.store.set_reminder_later(&external, set_aside, until) {
        return map_store(&e);
    }
    let label = match (set_aside, until) {
        (true, _) => "aside",
        (_, Some(_)) => "snooze",
        _ => "back",
    };
    state.audit(&actor.id, "reminder.later", label, Some(reminder_id), None);
    Response::Done
}

/// The early reminder of a task: written to Reminders as an alert, then kept here as it reads
/// back. A task without a due date cannot have one.
fn reminder_early(
    state: &State,
    actor: &Principal,
    reminder_id: &str,
    minutes: Option<u32>,
) -> Response {
    if minutes.is_some_and(|m| m > haste_proto::MAX_EARLY_MINUTES) {
        return err(ErrorCode::Validation, "an early reminder is a week at most");
    }
    let reminder = match state.store.get_reminder(reminder_id) {
        Ok(Some(r)) => r,
        Ok(None) => return err(ErrorCode::NotFound, "reminder not found"),
        Err(e) => return map_store(&e),
    };
    let minutes = minutes.filter(|m| *m != 0 && reminder.due_at.is_some());
    let Some(connector) = state.connector_for(&Source::Reminders) else {
        return err(ErrorCode::NotFound, "no reminders connector");
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let done = tokio::runtime::Handle::current()
        .block_on(guard.set_reminder_early(&reminder.external_id, minutes));
    if let Err(e) = done {
        state.audit(
            &actor.id,
            "reminder.early",
            "error",
            Some(reminder_id),
            None,
        );
        return err(ErrorCode::Internal, &format!("{e:?}"));
    }
    if let Err(e) = state
        .store
        .set_reminder_earlies(&[(reminder.external_id, minutes)])
    {
        return map_store(&e);
    }
    state.audit(&actor.id, "reminder.early", "set", Some(reminder_id), None);
    Response::Done
}

/// How important a task is, here only. Middle is the default and has no row.
fn reminder_priority(
    state: &State,
    actor: &Principal,
    reminder_id: &str,
    priority: haste_proto::TaskPriority,
) -> Response {
    let external = match state.store.get_reminder(reminder_id) {
        Ok(Some(r)) => r.external_id,
        Ok(None) => return err(ErrorCode::NotFound, "reminder not found"),
        Err(e) => return map_store(&e),
    };
    // Reminders first: a change it refuses is not kept here (the next sync would undo it)
    let Some(connector) = state.connector_for(&Source::Reminders) else {
        return err(ErrorCode::NotFound, "no reminders connector");
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let done = tokio::runtime::Handle::current()
        .block_on(guard.set_reminder_priority(&external, priority));
    if let Err(e) = done {
        state.audit(
            &actor.id,
            "reminder.priority",
            "error",
            Some(reminder_id),
            None,
        );
        return err(ErrorCode::Internal, &format!("{e:?}"));
    }
    let stored = (priority != haste_proto::TaskPriority::Middle).then_some(priority.rank());
    if let Err(e) = state.store.set_reminder_priority(&external, stored) {
        return map_store(&e);
    }
    state.audit(
        &actor.id,
        "reminder.priority",
        "set",
        Some(reminder_id),
        None,
    );
    Response::Done
}

/// Move or resize an event here only. Google is never written to; `None` puts it back.
fn event_shift(
    state: &State,
    actor: &Principal,
    event_id: &str,
    times: Option<(i64, i64)>,
) -> Response {
    let event = match state.store.get_event(event_id) {
        Ok(Some(e)) => e,
        Ok(None) => return err(ErrorCode::NotFound, "event not found"),
        Err(e) => return map_store(&e),
    };
    if let Some((start, end)) = times {
        // a minute at least, a day at most
        if end <= start || end - start > 86_400 {
            return err(ErrorCode::Validation, "the event must last up to a day");
        }
    }
    if let Err(e) = state.store.set_event_override(&event.external_id, times) {
        return map_store(&e);
    }
    state.audit(
        &actor.id,
        "event.shift",
        if times.is_some() { "moved" } else { "reset" },
        Some(event_id),
        None,
    );
    Response::Done
}

fn events_list(state: &State, principal: &Principal, from: i64) -> Response {
    let tags = state.store.all_event_tags().unwrap_or_default();
    let overrides = state.store.event_overrides().unwrap_or_default();
    let machine_labels = machine_labels_for(state, principal);
    match state.store.list_events(from) {
        Ok(rows) => Response::Events {
            events: rows
                .into_iter()
                .filter_map(|event| {
                    let own = tags.get(&event.id).cloned().unwrap_or_default();
                    visible_to(principal, &own, machine_labels.as_ref()).then(|| {
                        let mut event = event;
                        let shifted = match overrides.get(&event.external_id) {
                            Some(&(start, end)) => {
                                event.start_at = start;
                                event.end_at = end;
                                // an all-day event given a time is a timed one here
                                event.all_day = false;
                                true
                            }
                            None => false,
                        };
                        EventView {
                            event,
                            tags: own,
                            shifted,
                        }
                    })
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

/// How long the system dialog may stay unanswered.
const REMINDERS_AUTH_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// Run the Reminders helper with the prompt enabled, from this daemon's own process, so macOS
/// attributes the permission to the daemon and shows its dialog on the Mac's screen.
fn reminders_auth(state: &State, actor: &Principal) -> Response {
    use haste_connector_reminders::{Helper, ProcessHelper, RemindersRequest, RemindersResponse};
    let Some(bin) = crate::find_reminders_helper(&state.config.data_dir) else {
        return err(ErrorCode::NotFound, "the Reminders helper was not found");
    };
    let mut helper = ProcessHelper::new(bin);
    helper.timeout = REMINDERS_AUTH_WAIT;
    let status = match helper.call(&RemindersRequest::Authorization { prompt: true }) {
        Ok(RemindersResponse::Authorization { status }) => status.as_str().to_owned(),
        _ => "unknown".to_owned(),
    };
    state.audit(
        &actor.id,
        "reminders.auth",
        &status,
        Some("reminders"),
        None,
    );
    Response::RemindersAccess { status }
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
                repeat: None,
                priority: None,
                early: None,
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
/// Human-only triage write. Local state only: no connector is called, thread content is not
/// touched, and the audit row names the fields changed (labels), never the thread's text.
/// Take a thread out of the provider's inbox, when its source can. Sources without an archive
/// capability (chats) are archived here only. Refusals carry a fixed reason, never a name.
fn archive_upstream(
    state: &State,
    actor: &Principal,
    thread_id: &str,
) -> Result<(), Box<Response>> {
    let fail = |code: ErrorCode, msg: &str| Box::new(err(code, msg));
    let thread = match state.store.get_thread(thread_id) {
        Ok(Some((t, _))) => t,
        Ok(None) => return Err(fail(ErrorCode::NotFound, "thread not found")),
        Err(e) => return Err(Box::new(map_store(&e))),
    };
    let Some(connector) = state.connector_for(&thread.source) else {
        return Ok(());
    };
    if !connector.capabilities().archive {
        return Ok(());
    }
    let messages = state
        .store
        .messages_for_thread(thread_id)
        .map_err(|e| Box::new(map_store(&e)))?;
    let target = haste_core::connector::ArchiveTarget {
        thread_external_id: thread.external_id.clone(),
        message_external_ids: messages.iter().map(|m| m.external_id.clone()).collect(),
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let outcome = tokio::runtime::Handle::current().block_on(guard.archive_thread(&target));
    match outcome {
        Ok(()) => {
            state.audit(
                &actor.id,
                "thread.archive",
                "archived_upstream",
                Some(thread_id),
                Some(&format!("source={}", thread.source.as_name())),
            );
            Ok(())
        }
        Err(e) => {
            let mut detail: Option<String> = None;
            let (code, reason) = match &e {
                CallError::KillSwitchEngaged => (
                    ErrorCode::Denied,
                    "the connector is stopped; release it to archive in the mailbox",
                ),
                CallError::BreakerOpened(_) => (ErrorCode::Denied, "circuit_breaker"),
                CallError::Connector(ConnectorError::Other(m))
                    if m.contains("gmail.modify") =>
                {
                    (
                        ErrorCode::Denied,
                        "Gmail needs the new permission: run `haste connector gmail auth`, restart hasted",
                    )
                }
                CallError::Connector(ConnectorError::Transient(_)) => {
                    (ErrorCode::Internal, "the mail server could not be reached")
                }
                // iCloud's failure labels are fixed strings (the IMAP step and kind), safe to show
                CallError::Connector(ConnectorError::Other(m))
                    if thread.source.as_name() == "icloud" =>
                {
                    detail = Some(m.clone());
                    (ErrorCode::Internal, "the mailbox refused the archive")
                }
                _ => (ErrorCode::Internal, "the mailbox refused the archive"),
            };
            state.audit(
                &actor.id,
                "thread.archive",
                "failed",
                Some(thread_id),
                Some(&format!(
                    "source={}{}",
                    thread.source.as_name(),
                    detail
                        .as_deref()
                        .map_or(String::new(), |d| format!(" step={d}"))
                )),
            );
            Err(fail(
                code,
                &detail.map_or_else(|| reason.to_owned(), |d| format!("{reason} ({d})")),
            ))
        }
    }
}

fn triage_set(
    state: &State,
    actor: &Principal,
    thread_id: &str,
    update: &haste_store::TriageUpdate,
    now: i64,
) -> Response {
    match state.store.get_thread(thread_id) {
        Ok(Some(_)) => {}
        Ok(None) => return err(ErrorCode::NotFound, "thread not found"),
        Err(e) => return map_store(&e),
    }
    if let Some(p) = update.priority {
        if !(-1..=MAX_TRIAGE_PRIORITY).contains(&p) {
            return err(ErrorCode::Validation, "priority out of range");
        }
    }
    // Archiving a mail thread is real: it leaves the provider's inbox first, and only when that
    // worked is it archived here too. A refusal leaves everything as it was.
    if update.archived == Some(true) {
        if let Err(response) = archive_upstream(state, actor, thread_id) {
            return *response;
        }
    }
    if let Err(e) = state.store.set_triage(
        thread_id,
        haste_core::model::ObjectKind::Thread,
        update,
        now,
    ) {
        return map_store(&e);
    }
    let mut fields = Vec::new();
    if update.priority.is_some() {
        fields.push("priority");
    }
    if update.set_aside.is_some() {
        fields.push("set_aside");
    }
    if update.snoozed_until.is_some() {
        fields.push("snooze");
    }
    if update.archived.is_some() {
        fields.push("archive");
    }
    state.audit(
        &actor.id,
        "triage.set",
        "applied",
        Some(thread_id),
        Some(&format!("fields={}", fields.join(","))),
    );
    state.publish(Event::ThreadChanged {
        thread_id: thread_id.to_owned(),
    });
    Response::Done
}

fn read_ignore(data_dir: &std::path::Path) -> (haste_core::ignore::IgnoreList, Vec<String>) {
    let text =
        std::fs::read_to_string(data_dir.join(haste_core::ignore::IGNORE_FILE)).unwrap_or_default();
    haste_core::ignore::IgnoreList::parse(&text)
}

/// Who a rule applies to for a thread. Mail: the latest sender that is not the human's own address (the
/// human is never muted). Chats: the chat name, since a chat has no single sender.
fn rule_sender(state: &State, thread_id: &str) -> Result<String, &'static str> {
    let (thread, _) = state
        .store
        .get_thread(thread_id)
        .ok()
        .flatten()
        .ok_or("thread not found")?;
    if !matches!(thread.source, Source::Gmail | Source::Icloud) {
        return if thread.subject.trim().is_empty() {
            Err("this chat has no name to mute")
        } else {
            Ok(thread.subject.clone())
        };
    }
    let own = state
        .connector_for(&thread.source)
        .and_then(|c| c.own_address())
        .map(|a| a.to_lowercase())
        .ok_or("your own address is not known yet, so nobody can be muted safely")?;
    let messages = state
        .store
        .messages_for_thread(thread_id)
        .map_err(|_| "could not read the thread")?;
    messages
        .iter()
        .rev()
        .filter_map(|m| m.sender_identifier.as_deref())
        .find(|s| s.to_lowercase() != own)
        .map(str::to_owned)
        .ok_or("no sender other than you")
}

/// One muted mail is deleted from the local store (the thread, its messages, its read mark,
/// triage state and links). Only a thread of a muted sender can go this way; the sender stays
/// muted and the mailbox is not touched.
fn muted_delete(state: &State, actor: &Principal, thread_id: &str) -> Response {
    let sender = match rule_sender(state, thread_id) {
        Ok(s) => s,
        Err(why) => return err(ErrorCode::Validation, why),
    };
    let (mute, _) = read_ignore(&state.config.data_dir);
    if !mute.is_muted(&[sender.as_str()]) {
        return err(ErrorCode::Validation, "this sender is not muted");
    }
    delete_local(state, actor, thread_id, "muted.delete")
}

/// Delete one thread's stored copy here only. Counts and ids are audited, never the content.
fn delete_local(state: &State, actor: &Principal, thread_id: &str, action: &str) -> Response {
    match state.store.get_thread(thread_id) {
        Ok(Some(_)) => {}
        Ok(None) => return err(ErrorCode::NotFound, "thread not found"),
        Err(e) => return map_store(&e),
    }
    let messages = state
        .store
        .messages_for_thread(thread_id)
        .map_or(0, |m| m.len());
    if state.store.delete_threads(&[thread_id.to_owned()]).is_err() {
        return err(ErrorCode::Internal, "could not delete the mail");
    }
    state.audit(
        &actor.id,
        action,
        "deleted",
        Some(thread_id),
        Some(&format!("messages={messages}")),
    );
    state.publish(Event::ThreadChanged {
        thread_id: thread_id.to_owned(),
    });
    Response::Deleted { messages }
}

fn mute_pattern(state: &State, thread_id: &str) -> Result<String, &'static str> {
    rule_sender(state, thread_id).map(|s| haste_core::ignore::pattern_for(&s))
}

fn read_rule_file(
    data_dir: &std::path::Path,
    file: &str,
) -> (haste_core::ignore::IgnoreList, Vec<String>) {
    let text = std::fs::read_to_string(data_dir.join(file)).unwrap_or_default();
    haste_core::ignore::IgnoreList::parse(&text)
}

/// Add the thread's sender (or its domain) to a rule file, or remove every line that matches it.
/// Local files only; the audit records which file and that it happened, never the address.
fn rule_change(
    state: &State,
    actor: &Principal,
    thread_id: &str,
    rule: haste_proto::RuleKind,
    add: bool,
    domain: bool,
) -> Response {
    use haste_proto::RuleKind;
    let sender = match rule_sender(state, thread_id) {
        Ok(s) => s,
        Err(why) => return err(ErrorCode::Validation, why),
    };
    let (file, label) = match rule {
        RuleKind::Mute => (ignore::IGNORE_FILE, "mute"),
        RuleKind::Priority => (ignore::PRIORITY_FILE, "priority"),
        RuleKind::Allowed => (ignore::ALLOWED_FILE, "allowed"),
    };
    let path = state.config.data_dir.join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if add {
        let pattern = if domain {
            match ignore::domain_pattern_for(&sender) {
                Some(p) => p,
                None => return err(ErrorCode::Validation, "this sender has no domain"),
            }
        } else {
            ignore::pattern_for(&sender)
        };
        if !lines.iter().any(|l| l.trim() == pattern) {
            lines.push(pattern);
        }
    } else {
        lines.retain(|l| {
            let t = l.trim();
            t.is_empty()
                || t.starts_with('#')
                || !ignore::IgnoreList::from_patterns(&[t.to_owned()]).is_muted(&[sender.as_str()])
        });
    }
    let mut out = lines.join("\n");
    out.push('\n');
    if out != text {
        if write_private(&path, &out).is_err() {
            return err(ErrorCode::Internal, "could not write the rule file");
        }
        state.audit(
            &actor.id,
            if add { "rule.add" } else { "rule.remove" },
            label,
            Some(thread_id),
            None,
        );
    }
    state.publish(Event::ThreadChanged {
        thread_id: thread_id.to_owned(),
    });
    Response::Done
}

/// Write a file by replacing it whole (a temporary file, then a rename), readable by the owner.
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    drop(f);
    std::fs::rename(tmp, path)
}

/// Mark a mail thread read or unread. The mark is stored here first; then the provider is
/// asked once (Gmail). A provider that cannot or does not is reported, and the local mark stays.
fn read_set(state: &State, actor: &Principal, thread_id: &str, read: bool) -> Response {
    let thread = match state.store.get_thread(thread_id) {
        Ok(Some((t, _))) => t,
        Ok(None) => return err(ErrorCode::NotFound, "thread not found"),
        Err(e) => return map_store(&e),
    };
    if !matches!(thread.source, Source::Gmail | Source::Icloud) {
        return err(ErrorCode::Validation, "only mail has a read mark");
    }
    if let Err(e) = state
        .store
        .set_unread(&[(thread_id.to_owned(), !read)], now_secs())
    {
        return map_store(&e);
    }
    state.publish(Event::ThreadChanged {
        thread_id: thread_id.to_owned(),
    });
    state.audit(
        &actor.id,
        "thread.read_state",
        if read { "read" } else { "unread" },
        Some(thread_id),
        Some(&format!("source={}", thread.source.as_name())),
    );
    let Some(connector) = state.connector_for(&thread.source) else {
        return Response::Done;
    };
    let messages = match state.store.messages_for_thread(thread_id) {
        Ok(m) => m,
        Err(e) => return map_store(&e),
    };
    let target = haste_core::connector::ReadTarget {
        thread_external_id: thread.external_id.clone(),
        message_external_ids: messages.iter().map(|m| m.external_id.clone()).collect(),
        read,
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    match tokio::runtime::Handle::current().block_on(guard.set_thread_read(&target)) {
        Ok(()) => Response::Done,
        // a provider that has no such call: the mark stays local
        Err(CallError::Connector(ConnectorError::Unsupported(_))) => Response::Done,
        Err(CallError::Connector(ConnectorError::Other(m))) if m.contains("gmail.modify") => err(
            ErrorCode::Denied,
            "marked here only: run `haste connector gmail auth` to let it change Gmail",
        ),
        Err(_) => err(
            ErrorCode::Internal,
            "marked here only: the mailbox did not take the change",
        ),
    }
}

fn ignore_mute(state: &State, actor: &Principal, thread_id: &str) -> Response {
    let pattern = match mute_pattern(state, thread_id) {
        Ok(p) => p,
        Err(why) => return err(ErrorCode::Validation, why),
    };
    let path = state.config.data_dir.join(haste_core::ignore::IGNORE_FILE);
    let (existing, _) = read_ignore(&state.config.data_dir);
    if !existing.patterns().contains(&pattern) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{pattern}"));
        if written.is_err() {
            return err(ErrorCode::Internal, "could not write the mute list");
        }
        // The pattern holds an address or chat name: the audit records only that it happened.
        state.audit(&actor.id, "ignore.add", "added", Some(thread_id), None);
    }
    state.publish(Event::ThreadChanged {
        thread_id: thread_id.to_owned(),
    });
    Response::Muted { pattern }
}

/// Highest priority a human can set from the TUI.
const MAX_TRIAGE_PRIORITY: i32 = 9;

/// What the thread list needs about mail beyond the threads themselves: unread marks, which
/// threads the human answered last, and who the newest sender is.
struct MailFlags {
    unread: std::collections::HashSet<String>,
    replied: std::collections::HashSet<String>,
    /// Per thread: the newest sender who is not the human.
    senders: std::collections::HashMap<String, String>,
}

impl MailFlags {
    fn load(state: &State) -> Self {
        let own = |source: &Source| {
            state
                .connector_for(source)
                .and_then(|c| c.own_address())
                .map(|a| haste_core::ignore::address_of(&a))
                .unwrap_or_default()
        };
        let (gmail, icloud) = (own(&Source::Gmail), own(&Source::Icloud));
        let replied = state
            .store
            .replied_threads(&[
                ("gmail".to_owned(), gmail.clone()),
                ("icloud".to_owned(), icloud.clone()),
            ])
            .unwrap_or_default();
        let senders = state
            .store
            .last_incoming_senders(&gmail, &icloud)
            .unwrap_or_default();
        Self {
            unread: state.store.unread_threads().unwrap_or_default(),
            replied,
            senders,
        }
    }

    /// Mail senders are blocked until the human accepts them (`b`: the allowed list, or the
    /// priority list); nothing is accepted by reading or answering. Chats are never blocked.
    fn accepted(thread: &haste_core::model::Thread) -> bool {
        !matches!(thread.source, Source::Gmail | Source::Icloud)
    }
}

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
/// Fetch one attachment of a stored message from its provider and write it as a plain file: into
/// `dir` (an absolute folder the human picked) or into the private preview folder. Never
/// overwrites. The audit has the source and the size, never a file name.
/// The human reacts to a chat message: sent through the network once, never retried; the stored
/// message then shows the reaction at once (the next read of the chat confirms it).
fn reaction_add(state: &State, actor: &Principal, message_id: &str, key: &str) -> Response {
    let fail = |code: ErrorCode, msg: &str| err(code, msg);
    if !haste_core::reactions::valid_key(key) {
        return fail(ErrorCode::Validation, "not a usable reaction");
    }
    let message = match state.store.get_message(message_id) {
        Ok(Some(m)) => m,
        Ok(None) => return fail(ErrorCode::NotFound, "message not found"),
        Err(e) => return map_store(&e),
    };
    let thread = match state.store.get_thread(&message.thread_id) {
        Ok(Some((t, _))) => t,
        _ => return fail(ErrorCode::NotFound, "thread not found"),
    };
    let Some(connector) = state.connector_for(&message.source) else {
        return fail(
            ErrorCode::Validation,
            "no connector is registered for this source",
        );
    };
    let request = haste_core::connector::ReactionRequest {
        thread_external_id: thread.external_id.clone(),
        message_external_id: message.external_id.clone(),
        key: key.trim().to_owned(),
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let source = message.source.as_name();
    match tokio::runtime::Handle::current().block_on(guard.add_reaction(&request)) {
        Ok(()) => {
            let mut updated = message.clone();
            updated.body_text = haste_core::reactions::add_key(&message.body_text, key);
            let _ = state.store.upsert_message(&updated);
            state.audit(
                &actor.id,
                "reaction.add",
                "sent",
                Some(message_id),
                Some(&format!("source={source}")),
            );
            state.publish(Event::ThreadChanged {
                thread_id: message.thread_id,
            });
            Response::Done
        }
        Err(e) => {
            let reason = match &e {
                CallError::KillSwitchEngaged => {
                    "the connector is stopped; release it to react".to_owned()
                }
                CallError::Connector(ConnectorError::Unsupported(_)) => {
                    "reactions are only for chats".to_owned()
                }
                CallError::Connector(ConnectorError::Other(m))
                | CallError::Connector(ConnectorError::Authorization(m)) => {
                    m.chars().take(200).collect()
                }
                CallError::Connector(ConnectorError::Ambiguous(_)) => {
                    "the reaction may or may not have arrived; look in the chat".to_owned()
                }
                _ => "the chat network refused the reaction".to_owned(),
            };
            state.audit(
                &actor.id,
                "reaction.add",
                "failed",
                Some(message_id),
                Some(&format!("source={source}")),
            );
            fail(ErrorCode::Internal, &reason)
        }
    }
}

fn attachment_save(
    state: &State,
    actor: &Principal,
    message_id: &str,
    index: usize,
    dir: Option<&str>,
) -> Response {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let fail = |code: ErrorCode, msg: &str| err(code, msg);
    let message = match state.store.get_message(message_id) {
        Ok(Some(m)) => m,
        Ok(None) => return fail(ErrorCode::NotFound, "message not found"),
        Err(e) => return map_store(&e),
    };
    let lines = haste_core::attachment::lines_of(&message.body_text);
    let Some(line) = lines.get(index) else {
        return fail(ErrorCode::NotFound, "attachment not found");
    };
    let thread = match state.store.get_thread(&message.thread_id) {
        Ok(Some((t, _))) => t,
        _ => return fail(ErrorCode::NotFound, "thread not found"),
    };
    let Some(connector) = state.connector_for(&message.source) else {
        return fail(
            ErrorCode::Validation,
            "no connector is registered for this source",
        );
    };
    // where the file goes, checked before anything is fetched
    let (target_dir, preview) = match dir {
        Some(d) => {
            let path = std::path::PathBuf::from(d);
            if !path.is_absolute() || !path.is_dir() {
                return fail(ErrorCode::Validation, "choose an existing folder");
            }
            (path, false)
        }
        None => {
            let path = state.config.data_dir.join("preview");
            let made = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path);
            if made.is_err() {
                return fail(ErrorCode::Internal, "cannot create the preview folder");
            }
            (path, true)
        }
    };
    let request = haste_core::connector::AttachmentRequest {
        message_external_id: message.external_id.clone(),
        thread_external_id: thread.external_id.clone(),
        reference: line.reference.clone().unwrap_or_default(),
        bytes: line.bytes,
        name: line.name.clone(),
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let bytes = match tokio::runtime::Handle::current().block_on(guard.fetch_attachment(&request)) {
        Ok(b) => b,
        Err(e) => {
            let reason = match &e {
                CallError::KillSwitchEngaged => {
                    "the connector is stopped; release it to download".to_owned()
                }
                CallError::Connector(ConnectorError::Other(m)) => m.chars().take(200).collect(),
                CallError::Connector(ConnectorError::Authorization(m)) => {
                    m.chars().take(200).collect()
                }
                CallError::Connector(ConnectorError::Transient(_)) => {
                    "the provider could not be reached".to_owned()
                }
                _ => "the provider refused the download".to_owned(),
            };
            state.audit(
                &actor.id,
                "attachment.save",
                "failed",
                Some(message_id),
                Some(&format!("source={}", message.source.as_name())),
            );
            return fail(ErrorCode::Internal, &reason);
        }
    };
    // a name that cannot collide and cannot escape the folder; never an overwrite
    let base = haste_core::attachment::safe_file_name(&line.name);
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (base.clone(), String::new()),
    };
    let mut written = None;
    for n in 0..100 {
        let name = match (preview, n) {
            (true, n) => format!("{}-{n}-{base}", haste_core::time::now_secs()),
            (false, 0) => base.clone(),
            (false, n) => format!("{stem} ({n}){ext}"),
        };
        let path = target_dir.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                if std::io::Write::write_all(&mut file, &bytes).is_err() {
                    let _ = std::fs::remove_file(&path);
                    return fail(ErrorCode::Internal, "could not write the file");
                }
                written = Some((path, name));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return fail(ErrorCode::Internal, "could not create the file"),
        }
    }
    let Some((path, name)) = written else {
        return fail(ErrorCode::Internal, "too many files with this name");
    };
    state.audit(
        &actor.id,
        "attachment.save",
        if preview { "previewed" } else { "saved" },
        Some(message_id),
        Some(&format!(
            "source={} bytes={}",
            message.source.as_name(),
            bytes.len()
        )),
    );
    Response::AttachmentSaved {
        path: path.to_string_lossy().into_owned(),
        name,
        bytes: bytes.len(),
    }
}

/// Forget the local copy of a mail source and read it again from the start. Only mail sources
/// (chats keep their own history cursor); nothing is changed at the provider. The audit has the
/// source and a count, never a subject.
fn connector_resync(state: &State, actor: &Principal, source: &str) -> Response {
    let source = Source::from(source.to_owned());
    if !matches!(source, Source::Gmail | Source::Icloud) {
        return err(
            ErrorCode::Validation,
            "only Gmail and iCloud can be re-read",
        );
    }
    if !state.registered_sources().contains(&source) {
        return err(
            ErrorCode::Validation,
            "no connector is registered for this source",
        );
    }
    match state.store.purge_source(&source) {
        Ok(threads) => {
            state.audit(
                &actor.id,
                "connector.resync",
                "purged",
                Some(&source.as_name()),
                Some(&format!("threads={threads}")),
            );
            state.publish(Event::Notice {
                message: format!(
                    "{}: local copy cleared; reading the inbox again",
                    source.as_name()
                ),
            });
            state.wake_sync(&source);
            Response::Done
        }
        Err(e) => map_store(&e),
    }
}

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
    if !engaged {
        // First sync now, not at the next tick (the loop of a source that has none ignores it).
        state.wake_sync(&source);
    }
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
    let Payload::Draft { to, body, .. } = &proposal.payload else {
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
        ..Default::default()
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
    if let Payload::Draft { attachments, .. } = &payload {
        if !attachments.is_empty() {
            // only the human's own drafts carry files: an agent must not be able to name one
            if principal.kind != PrincipalKind::Human {
                return err(ErrorCode::Denied, "attachments_human_only");
            }
            if let Err(why) = check_attachments(state, attachments) {
                return err(ErrorCode::Validation, why);
            }
        }
    }
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

fn note_person(
    state: &State,
    principal: &Principal,
    person_id: &str,
    text: &str,
    key: &str,
    now: i64,
) -> Response {
    if key.trim().is_empty() {
        return err(ErrorCode::Validation, "idempotency key is required");
    }
    let person = match state.store.get_person(person_id) {
        Ok(Some(p)) => p,
        Ok(None) => return err(ErrorCode::NotFound, "person not found"),
        Err(e) => return map_store(&e),
    };
    if !state.remember_note_key(&principal.id, key) {
        return Response::Appended {
            path: String::new(),
            bytes: 0,
        };
    }
    let root = &state.config.notes_root;
    let path = match person_note_path(root, &person) {
        Ok(p) => p,
        Err(AppendError::Denied(d)) => return note_denied(state, &principal.id, "people/", d),
        Err(AppendError::Io(m)) => return err(ErrorCode::Internal, &m),
    };
    match guarded_note_write(state, &principal.id, &path, now, || {
        append_person_entry(
            root,
            &path,
            &person,
            &principal.id,
            text,
            now,
            &state.config.notes_settings.person_notes,
        )
    }) {
        Ok(bytes) => Response::Appended { path, bytes },
        Err(response) => *response,
    }
}

/// The checks every back-reference request passes before anything is queued or written: a valid
/// identifier, a usable topic, and an object that exists. Returns the target path.
fn check_reference(
    state: &State,
    args: &haste_proto::NoteReferenceArgs,
) -> Result<String, Box<Response>> {
    if args.idempotency_key.trim().is_empty() {
        return Err(Box::new(err(
            ErrorCode::Validation,
            "idempotency key is required",
        )));
    }
    if !valid_reference_id(&args.id) {
        return Err(Box::new(err(
            ErrorCode::Validation,
            "the identifier is not valid",
        )));
    }
    let Some(path) = reference_path(&args.topic, args.project.as_deref()) else {
        return Err(Box::new(err(
            ErrorCode::Validation,
            "a topic label is required",
        )));
    };
    let exists = match args.kind {
        haste_proto::RefKind::Thread => state.store.get_thread(&args.id).map(|t| t.is_some()),
        haste_proto::RefKind::Reminder => state.store.get_reminder(&args.id).map(|r| r.is_some()),
        haste_proto::RefKind::Event => state.store.get_event(&args.id).map(|e| e.is_some()),
    };
    match exists {
        Ok(true) => Ok(path),
        Ok(false) => Err(Box::new(err(
            ErrorCode::NotFound,
            "the referenced object does not exist",
        ))),
        Err(e) => Err(Box::new(map_store(&e))),
    }
}

fn note_reference(
    state: &State,
    principal: &Principal,
    args: &haste_proto::NoteReferenceArgs,
    now: i64,
) -> Response {
    let path = match check_reference(state, args) {
        Ok(p) => p,
        Err(response) => return *response,
    };
    if !state.remember_note_key(&principal.id, &args.idempotency_key) {
        return Response::Appended { path, bytes: 0 };
    }
    let root = &state.config.notes_root;
    match guarded_note_write(state, &principal.id, &path, now, || {
        append_reference(
            root,
            &path,
            &principal.id,
            args.kind.as_str(),
            &args.id,
            now,
        )
    }) {
        Ok(bytes) => Response::Appended { path, bytes },
        Err(response) => *response,
    }
}

/// Human-only: read a note. The same lexical, hidden-path, symlink and containment rules apply as
/// for writes, so `note show` cannot be aimed at anything outside the notes folder.
fn note_show(state: &State, actor: &Principal, path: &str) -> Response {
    let target = match resolve_note_path(&state.config.notes_root, path) {
        Ok(t) => t,
        Err(NotePathError::Denied(d)) => {
            state.audit(
                &actor.id,
                "note.show",
                "denied",
                Some(path),
                Some(d.as_str()),
            );
            return err(ErrorCode::Denied, d.as_str());
        }
        Err(NotePathError::Io(m)) => return err(ErrorCode::Internal, &m),
    };
    match std::fs::read_to_string(&target) {
        Ok(text) => {
            state.audit(&actor.id, "note.show", "read", Some(path), None);
            Response::Note {
                path: path.to_owned(),
                text,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            err(ErrorCode::NotFound, "note not found")
        }
        Err(e) => err(ErrorCode::Internal, &e.kind().to_string()),
    }
}

/// Human-only: the `note.append` audit rows. The audit has no text, so neither does this.
fn note_log(state: &State, since: Option<i64>) -> Response {
    match read_recent(&state.config.audit_path(), NOTE_LOG_SCAN) {
        Ok(records) => Response::NoteLog {
            rows: records
                .into_iter()
                .filter(|r| r.action == "note.append" && since.is_none_or(|t| r.ts >= t))
                .map(|r| NoteLogRow {
                    ts: r.ts,
                    principal: r.principal,
                    outcome: r.outcome,
                    path: r.object,
                    detail: r.detail,
                })
                .collect(),
        },
        Err(e) => err(ErrorCode::Internal, &e.to_string()),
    }
}

/// Audit rows scanned (newest) when building the note log.
const NOTE_LOG_SCAN: usize = 5000;

/// Approval mode: hold the append as a proposal; nothing is written until the human approves.
fn queue_note(
    state: &State,
    principal: &Principal,
    entry: NoteEntry,
    key: &str,
    now: i64,
) -> Response {
    if key.trim().is_empty() {
        return err(ErrorCode::Validation, "idempotency key is required");
    }
    propose(
        state,
        principal,
        Payload::NoteAppend { entry },
        None,
        key.to_owned(),
        None,
        now,
    )
}

fn reason_of(response: &Response) -> String {
    match response {
        Response::Error { error } => error.message.clone(),
        _ => "refused".to_owned(),
    }
}

/// Run an approved note proposal under its original principal, with the same checks, budget and
/// audit as an automatic append, then close the proposal: `sent` on success, or `rejected` with
/// the executor's refusal reason (nothing was written).
fn run_note_proposal(
    state: &State,
    proposal: &haste_core::Proposal,
    now: i64,
) -> haste_core::Proposal {
    let Payload::NoteAppend { entry } = &proposal.payload else {
        return proposal.clone();
    };
    let who = proposal.principal_id.as_str();
    let root = &state.config.notes_root;
    let outcome: Result<u64, String> = match entry {
        NoteEntry::Text { path, text } => guarded_note_write(state, who, path, now, || {
            append_note(root, path, who, text, now)
        })
        .map_err(|r| reason_of(&r)),
        NoteEntry::Person { person_id, text } => match state.store.get_person(person_id) {
            Ok(Some(person)) => match person_note_path(root, &person) {
                Ok(path) => guarded_note_write(state, who, &path, now, || {
                    append_person_entry(
                        root,
                        &path,
                        &person,
                        who,
                        text,
                        now,
                        &state.config.notes_settings.person_notes,
                    )
                })
                .map_err(|r| reason_of(&r)),
                Err(AppendError::Denied(d)) => {
                    note_denied(state, who, "people/", d);
                    Err(d.as_str().to_owned())
                }
                Err(AppendError::Io(m)) => Err(m),
            },
            _ => Err("person_not_found".to_owned()),
        },
        NoteEntry::Reference {
            kind,
            id,
            topic,
            project,
        } => match reference_path(topic, project.as_deref()) {
            Some(path) => guarded_note_write(state, who, &path, now, || {
                append_reference(root, &path, who, kind, id, now)
            })
            .map_err(|r| reason_of(&r)),
            None => Err("topic_missing".to_owned()),
        },
    };
    state
        .store
        .finish_note_proposal(&proposal.id, outcome, now)
        .unwrap_or_else(|_| proposal.clone())
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

/// Everything a send needs, resolved once from the proposal and the store.
struct ResolvedSend {
    /// The digest of what the human is shown (recipients for mail; chat, name and text for chats).
    digest: String,
    source: Source,
    thread_external_id: String,
    recipients: Vec<String>,
    subject: String,
    body: String,
    approval_draft_id: Option<String>,
    attachments: Vec<haste_core::connector::OutAttachment>,
    stale: bool,
}

/// Resolve who a send goes to and what it says. Refusals carry a fixed reason label, never an
/// address: the recipient is `to` if the draft names one, otherwise the last message in the
/// thread that is not from the connector's own address.
fn resolve_send(
    state: &State,
    proposal: &haste_core::Proposal,
) -> Result<ResolvedSend, Box<Response>> {
    let fail = |code: ErrorCode, msg: &str| Box::new(err(code, msg));
    let Payload::Draft {
        to,
        body,
        attachments: attachment_names,
    } = &proposal.payload
    else {
        return Err(fail(
            ErrorCode::IllegalTransition,
            "only drafts can be sent",
        ));
    };
    let Some(thread_id) = proposal.source_thread_id.clone() else {
        return Err(fail(ErrorCode::Validation, "no_source_thread"));
    };
    let thread = match state.store.get_thread(&thread_id) {
        Ok(Some((t, _))) => t,
        Ok(None) => return Err(fail(ErrorCode::NotFound, "source thread not found")),
        Err(e) => return Err(Box::new(map_store(&e))),
    };
    let Some(connector) = state.connector_for(&thread.source) else {
        return Err(fail(
            ErrorCode::Validation,
            "no connector registered for this source",
        ));
    };
    let Some(scope) = SendScope::of(&thread.source) else {
        return Err(fail(ErrorCode::Validation, "cannot send to this source"));
    };
    let max_body = match scope {
        SendScope::Mail => MAX_SEND_BODY_BYTES,
        SendScope::Chat => CHAT_MAX_SEND_BYTES,
    };
    // a file may go out without any text (chats), never an empty mail
    if (body.trim().is_empty() && (attachment_names.is_empty() || scope == SendScope::Mail))
        || body.len() > max_body
    {
        return Err(fail(ErrorCode::Validation, "body_length"));
    }
    if scope == SendScope::Chat && attachment_names.len() > 1 {
        return Err(fail(ErrorCode::Validation, "chat_one_attachment"));
    }
    let attachments = load_attachments(state, attachment_names)
        .map_err(|why| fail(ErrorCode::Validation, why))?;
    if scope == SendScope::Chat {
        // A reply into the existing chat the thread is: no recipient list, no subject. The human
        // confirms the chat name and the text; the digest covers both.
        let stale = state
            .store
            .get_proposal(&proposal.id)
            .map(|p| p.stale)
            .unwrap_or(false);
        return Ok(ResolvedSend {
            digest: with_attachments_digest(
                chat_send_digest(&thread.external_id, &thread.subject, body),
                &attachments,
            ),
            source: thread.source,
            recipients: vec![thread.subject.clone()],
            subject: String::new(),
            thread_external_id: thread.external_id,
            body: body.clone(),
            approval_draft_id: None,
            attachments,
            stale,
        });
    }
    let recipients = match to {
        Some(t) => vec![t.trim().to_owned()],
        None => {
            let own = connector.own_address().map(|a| a.to_lowercase());
            let messages = state
                .store
                .messages_for_thread(&thread.id)
                .map_err(|e| Box::new(map_store(&e)))?;
            let target = messages
                .iter()
                .rev()
                .filter_map(|m| m.sender_identifier.clone())
                .find(|s| Some(s.to_lowercase()) != own);
            match target {
                Some(t) => vec![t],
                None => return Err(fail(ErrorCode::Validation, "no_recipient")),
            }
        }
    };
    validate_recipients(&recipients).map_err(|why| fail(ErrorCode::Validation, why))?;
    let subject = reply_subject(&thread.subject);
    validate_subject(&subject).map_err(|why| fail(ErrorCode::Validation, why))?;
    let stale = state
        .store
        .get_proposal(&proposal.id)
        .map(|p| p.stale)
        .unwrap_or(false);
    Ok(ResolvedSend {
        digest: with_attachments_digest(recipients_digest(&recipients), &attachments),
        source: thread.source,
        thread_external_id: thread.external_id,
        recipients,
        subject,
        body: body.clone(),
        approval_draft_id: proposal
            .result
            .as_deref()
            .and_then(|r| r.strip_prefix("draft:"))
            .map(str::to_owned),
        attachments,
        stale,
    })
}

/// The folder the TUI copies attached files into; a send reads only from here.
fn attachments_dir(state: &State) -> std::path::PathBuf {
    state.config.data_dir.join("attachments")
}

/// Check the attachment names of a draft: a few plain file names that exist as regular files
/// (never links) in the attachments folder, within the size cap. Returns each file's size.
/// Failures carry a fixed reason label, never a name.
fn check_attachments(state: &State, names: &[String]) -> Result<Vec<u64>, &'static str> {
    use haste_core::connector::{MAX_SEND_ATTACHMENTS, MAX_SEND_ATTACHMENT_BYTES};
    if names.len() > MAX_SEND_ATTACHMENTS {
        return Err("attachments_count");
    }
    let dir = attachments_dir(state);
    let mut sizes = Vec::new();
    let mut total: u64 = 0;
    for name in names {
        let plain = !name.is_empty()
            && name.chars().count() <= 120
            && !name.starts_with('.')
            && !name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | '"'));
        if !plain {
            return Err("attachment_name");
        }
        let meta = std::fs::symlink_metadata(dir.join(name)).map_err(|_| "attachment_missing")?;
        if !meta.is_file() {
            return Err("attachment_missing");
        }
        total += meta.len();
        sizes.push(meta.len());
    }
    if total > MAX_SEND_ATTACHMENT_BYTES as u64 {
        return Err("attachments_size");
    }
    Ok(sizes)
}

/// Read the attached files for a send (after [`check_attachments`]).
fn load_attachments(
    state: &State,
    names: &[String],
) -> Result<Vec<haste_core::connector::OutAttachment>, &'static str> {
    check_attachments(state, names)?;
    let dir = attachments_dir(state);
    names
        .iter()
        .map(|name| {
            let data = std::fs::read(dir.join(name)).map_err(|_| "attachment_missing")?;
            Ok(haste_core::connector::OutAttachment {
                mime: haste_core::send::attachment_mime(name, ""),
                name: name.clone(),
                data,
            })
        })
        .collect()
}

/// Fold the attached files (names and sizes) into the digest the human confirms, so the send
/// goes out with exactly the files that were shown.
fn with_attachments_digest(
    digest: String,
    attachments: &[haste_core::connector::OutAttachment],
) -> String {
    use sha2::{Digest, Sha256};
    if attachments.is_empty() {
        return digest;
    }
    let mut hasher = Sha256::new();
    hasher.update(digest.as_bytes());
    for a in attachments {
        hasher.update([0_u8]);
        hasher.update(a.name.as_bytes());
        hasher.update([0_u8]);
        hasher.update(Sha256::digest(&a.data));
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read-only: who a send would go to. Changes nothing and audits no content.
fn send_preview(state: &State, proposal_id: &str, now: i64) -> Response {
    let proposal = match state.store.get_proposal(proposal_id) {
        Ok(p) => p,
        Err(e) => return map_store(&e),
    };
    if proposal.state != haste_core::proposal::ProposalState::Approved {
        return err(
            ErrorCode::IllegalTransition,
            "only approved proposals can be sent",
        );
    }
    let resolved = match resolve_send(state, &proposal) {
        Ok(r) => r,
        Err(response) => return *response,
    };
    if state
        .store
        .kill_switch_engaged(&resolved.source)
        .unwrap_or(true)
    {
        return err(ErrorCode::Denied, "kill_switch_engaged");
    }
    let remaining = state.store.sends_remaining(proposal_id, now).unwrap_or(0);
    Response::SendPreview {
        preview: SendPreviewView {
            source: resolved.source.as_name(),
            recipients_digest: resolved.digest.clone(),
            recipients: resolved.recipients,
            subject: resolved.subject,
            body_bytes: resolved.body.len(),
            attachments: resolved
                .attachments
                .iter()
                .map(|a| haste_proto::AttachmentInfo {
                    name: a.name.clone(),
                    bytes: a.data.len(),
                })
                .collect(),
            stale: resolved.stale,
            sends_remaining: remaining,
        },
    }
}

/// The human sends an approved draft. One attempt, ever: `approved -> executing`, one guarded
/// connector call, then `sent`; back to `approved` only when nothing can have been delivered;
/// otherwise it stays `executing` for a human check. No address reaches the audit or a reply.
fn send(
    state: &State,
    actor: &Principal,
    proposal_id: &str,
    ack: bool,
    digest: &str,
    now: i64,
) -> Response {
    let refuse = |reason: &str, response: Response| {
        state.audit(
            &actor.id,
            "proposal.send",
            "refused",
            Some(proposal_id),
            Some(reason),
        );
        response
    };
    let proposal = match state.store.get_proposal(proposal_id) {
        Ok(p) => p,
        Err(e) => return map_store(&e),
    };
    let resolved = match resolve_send(state, &proposal) {
        Ok(r) => r,
        Err(response) => {
            let reason = reason_of(&response);
            return refuse(&reason, *response);
        }
    };
    if resolved.digest != digest {
        return refuse(
            "recipients_changed",
            err(ErrorCode::Validation, "recipients_changed"),
        );
    }
    let Some(connector) = state.connector_for(&resolved.source) else {
        return refuse(
            "no_connector",
            err(
                ErrorCode::Validation,
                "no connector registered for this source",
            ),
        );
    };
    if let Err(e) = state
        .store
        .begin_send(proposal_id, ack, now, &resolved.recipients)
    {
        let reason = match &e {
            StoreError::Validation(m) => m.clone(),
            StoreError::StaleNeedsAcknowledgement => "stale".to_owned(),
            _ => "not_sendable".to_owned(),
        };
        return refuse(&reason, map_store(&e));
    }
    state.publish(Event::ProposalChanged {
        proposal_id: proposal_id.to_owned(),
        state: haste_core::proposal::ProposalState::Executing,
    });
    let draft = DraftToSend {
        thread_id: Some(resolved.thread_external_id.clone()),
        to: resolved.recipients.first().cloned(),
        body: resolved.body.clone(),
        recipients: resolved.recipients.clone(),
        subject: Some(resolved.subject.clone()),
        approval_draft_id: resolved.approval_draft_id.clone(),
        attachments: resolved.attachments.clone(),
    };
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state,
    };
    let outcome = tokio::runtime::Handle::current().block_on(guard.send(&draft));
    match outcome {
        Ok(receipt) => {
            let result = format!("sent:{}", receipt.external_id);
            match state.store.finish_send(proposal_id, &result, now) {
                Ok(p) => {
                    // the count of files only, never a name
                    let files = resolved.attachments.len();
                    let detail = (files > 0).then(|| format!("attachments={files}"));
                    state.audit(
                        &actor.id,
                        "proposal.send",
                        "sent",
                        Some(proposal_id),
                        detail.as_deref(),
                    );
                    if receipt.external_id.contains("copy_failed") {
                        state.audit(
                            &actor.id,
                            "send.copy_failed",
                            "failed",
                            Some(proposal_id),
                            None,
                        );
                    }
                    state.publish(Event::ProposalChanged {
                        proposal_id: proposal_id.to_owned(),
                        state: p.state,
                    });
                    Response::Proposal { proposal: p }
                }
                // delivered but not recorded: keep it executing so nobody sends it twice
                Err(e) => {
                    state.audit(
                        &actor.id,
                        "proposal.send",
                        "needs_check",
                        Some(proposal_id),
                        Some("recorded_failed"),
                    );
                    map_store(&e)
                }
            }
        }
        Err(error) => match classify_send(&error) {
            SendOutcome::NoEffect => {
                let (reason, code) = match &error {
                    CallError::KillSwitchEngaged => ("kill_switch_engaged", ErrorCode::Denied),
                    CallError::BreakerOpened(_) => ("circuit_breaker", ErrorCode::Denied),
                    CallError::CapabilityMissing(_) => ("connector_cannot_send", ErrorCode::Denied),
                    CallError::Connector(ConnectorError::Transient(_)) => {
                        ("connection_failed", ErrorCode::Internal)
                    }
                    _ => ("send_refused", ErrorCode::Internal),
                };
                let _ = state.store.fail_send_no_effect(proposal_id, reason, now);
                state.audit(
                    &actor.id,
                    "proposal.send",
                    "failed",
                    Some(proposal_id),
                    Some(reason),
                );
                state.publish(Event::ProposalChanged {
                    proposal_id: proposal_id.to_owned(),
                    state: haste_core::proposal::ProposalState::Approved,
                });
                err(code, reason)
            }
            SendOutcome::Ambiguous | SendOutcome::Sent => {
                let _ = state
                    .store
                    .mark_send_needs_check(proposal_id, "outcome_unknown", now);
                state.audit(
                    &actor.id,
                    "proposal.send",
                    "needs_check",
                    Some(proposal_id),
                    Some("outcome_unknown"),
                );
                state.publish(Event::Notice {
                    message: format!("a send needs a check: proposal {proposal_id} may or may not have been delivered"),
                });
                err(
                    ErrorCode::Internal,
                    "the outcome is unknown; the proposal stays 'executing' until you check the mailbox",
                )
            }
        },
    }
}

/// The first words of a message body on one line (a list preview).
fn snippet(body: &str) -> String {
    // the reactions line is not part of what was said
    let (text, _) = haste_core::reactions::take(body);
    // nor are the attachment lines (the list shows a paperclip for those)
    let text: String = text
        .lines()
        .filter(|l| haste_core::attachment::parse_line(l).is_none() && l.trim() != "[attachment]")
        .collect::<Vec<_>>()
        .join(" ");
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(160).collect()
}

#[cfg(test)]
mod snippet_tests {
    use super::snippet;

    #[test]
    fn the_preview_leaves_out_reactions_and_attachment_lines() {
        let body = "Please see attached\n[attachment: S26.pdf (application/pdf, 10 bytes) #TOKEN]\n[reactions: 👍]";
        assert_eq!(snippet(body), "Please see attached");
        assert_eq!(snippet("[attachment: a.png (image/png, 5 bytes)]"), "");
    }
}
