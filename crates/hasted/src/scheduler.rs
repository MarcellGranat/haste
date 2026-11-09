//! Background tasks: proposal expiry and bounded result delivery.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use haste_core::connector::{CallError, Connector, Cursor, Guarded};
use haste_core::model::Source;
use haste_core::time::now_secs;
use haste_proto::Event;
use tokio::time::interval;

use crate::state::State;

/// Expire `pending` proposals whose TTL has elapsed.
pub async fn expiry_loop(state: Arc<State>) {
    let mut ticker = interval(state.config.expiry_tick);
    loop {
        ticker.tick().await;
        run_expiry(&state).await;
    }
}

/// One expiry pass; exposed so tests can drive it deterministically.
pub async fn run_expiry(state: &Arc<State>) -> usize {
    let st = Arc::clone(state);
    let expired = tokio::task::spawn_blocking(move || st.store.expire_due(now_secs())).await;
    let Ok(Ok(expired)) = expired else { return 0 };
    for p in &expired {
        state.audit(
            &p.principal_id,
            "proposal.expire",
            "expired",
            Some(&p.id),
            None,
        );
        state.publish(Event::ProposalChanged {
            proposal_id: p.id.clone(),
            state: p.state,
        });
    }
    expired.len()
}

/// Deliver proposal results to agents, at most three attempts each, with backoff.
pub async fn delivery_loop(state: Arc<State>) {
    // The first pass comes one period after start, not immediately: delivery is retried on a
    // timer anyway, and an immediate pass at startup races anything driving delivery by hand.
    let period = state.config.delivery_tick;
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    let mut last_attempt: HashMap<String, Instant> = HashMap::new();
    loop {
        ticker.tick().await;
        run_delivery(&state, &mut last_attempt).await;
    }
}

/// One delivery pass. `last_attempt` tracks backoff per proposal.
pub async fn run_delivery(
    state: &Arc<State>,
    last_attempt: &mut HashMap<String, Instant>,
) -> usize {
    let st = Arc::clone(state);
    let Ok(Ok(due)) = tokio::task::spawn_blocking(move || st.store.pending_deliveries()).await
    else {
        return 0;
    };
    let mut attempted = 0;
    for (proposal, attempts) in due {
        let wait = state.config.delivery_backoff * 2_u32.saturating_pow(attempts);
        if attempts > 0
            && last_attempt
                .get(&proposal.id)
                .is_some_and(|t| t.elapsed() < wait)
        {
            continue;
        }
        if haste_core::herdr::agent_name_for(&proposal.principal_id).is_err() {
            // No valid herdr agent name: nothing is sent and delivery is not retried.
            let st = Arc::clone(state);
            let id = proposal.id.clone();
            let _ = tokio::task::spawn_blocking(move || st.store.give_up_delivery(&id)).await;
            state.audit(
                &proposal.principal_id,
                "herdr.prompt",
                "refused_invalid_name",
                Some(&proposal.id),
                None,
            );
            continue;
        }
        let message = format!(
            "haste: proposal {} is {}{}. Check with `haste proposal status {}`.",
            proposal.id,
            proposal.state.as_str(),
            proposal
                .result
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default(),
            proposal.id,
        );
        let st = Arc::clone(state);
        let agent = proposal.principal_id.clone();
        // A remote agent is prompted through the hub; a local one over the socket.
        let machine = proposal
            .origin_machine
            .clone()
            .filter(|m| m != haste_core::herdr::LOCAL_MACHINE);
        let result = tokio::task::spawn_blocking(move || match machine {
            Some(m) => st.config.notifier.deliver_remote(&m, &agent, &message),
            None => st.config.notifier.deliver(&agent, &message),
        })
        .await;
        let delivered = matches!(result, Ok(Ok(())));
        last_attempt.insert(proposal.id.clone(), Instant::now());
        attempted += 1;
        let st = Arc::clone(state);
        let id = proposal.id.clone();
        let used =
            tokio::task::spawn_blocking(move || st.store.record_delivery_attempt(&id, delivered))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
        if delivered {
            state.audit(
                &proposal.principal_id,
                "herdr.prompt",
                "delivered",
                Some(&proposal.id),
                None,
            );
        } else {
            state.audit(
                &proposal.principal_id,
                "herdr.prompt",
                "failed",
                Some(&proposal.id),
                Some(&format!("attempt={used}")),
            );
        }
    }
    attempted
}

/// What one sync cycle stored.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub threads: usize,
    pub messages: usize,
    pub reminders: usize,
    pub events: usize,
}

/// One scheduled cycle for any connector: fetch through the guard (capability check, kill
/// switch, breaker, bounded retries, audit), store the batch, then save the cursor. The
/// cursor is saved only after the batch is stored, so a failure re-fetches rather than skips.
///
/// # Errors
/// The guard's [`CallError`]; store failures are reported as a connector `Other` error.
pub async fn sync_once(
    state: &Arc<State>,
    connector: &Arc<dyn Connector>,
) -> Result<SyncReport, CallError> {
    let source = connector.source();
    let st = Arc::clone(state);
    let src = source.clone();
    let cursor = tokio::task::spawn_blocking(move || st.store.connector_cursor(&src))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .map(Cursor);
    let guard = Guarded {
        connector: connector.as_ref(),
        hooks: state.as_ref(),
    };
    let batch = guard.fetch(cursor).await?;

    let st = Arc::clone(state);
    let stored = tokio::task::spawn_blocking(move || store_batch(&st, &source, batch)).await;
    match stored {
        Ok(Ok((report, changed))) => {
            for id in changed {
                state.publish(Event::ThreadChanged { thread_id: id });
            }
            Ok(report)
        }
        _ => Err(CallError::Connector(
            haste_core::connector::ConnectorError::Other("could not store the batch".into()),
        )),
    }
}

fn store_batch(
    state: &State,
    source: &Source,
    batch: haste_core::connector::Batch,
) -> Result<(SyncReport, Vec<String>), haste_store::StoreError> {
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut changed = Vec::new();
    for t in &batch.threads {
        // A delta batch can hold only the newest messages of a thread (IMAP), so keep what the
        // stored thread already knows: its original subject and every participant seen so far.
        let (subject, participants) = match state.store.find_thread(&t.source, &t.external_id)? {
            Some(existing) => {
                let mut all = existing.participants.clone();
                for p in &t.participants {
                    if !all.contains(p) {
                        all.push(p.clone());
                    }
                }
                let subject = if existing.subject.is_empty() {
                    t.subject.clone()
                } else {
                    existing.subject
                };
                (subject, all)
            }
            None => (t.subject.clone(), t.participants.clone()),
        };
        let stored = state.store.upsert_thread(
            &t.source,
            &t.external_id,
            &subject,
            &participants,
            t.last_activity_at,
        )?;
        ids.insert(t.external_id.clone(), stored.id.clone());
        changed.push(stored.id);
    }
    let mut messages = 0;
    for m in &batch.messages {
        // A batch's messages carry the provider's thread id; resolve it to the stored thread.
        let Some(thread_id) = ids.get(&m.thread_id) else {
            continue;
        };
        let mut m = m.clone();
        m.thread_id = thread_id.clone();
        state.store.upsert_message(&m)?;
        messages += 1;
    }
    if !batch.reminders.is_empty() || batch.sweep.is_some() {
        state
            .store
            .upsert_reminders(&batch.reminders, batch.sweep.as_ref())?;
        // Only a complete sweep may mark absent reminders missing.
        if let Some(sweep) = batch.sweep.as_ref().filter(|s| s.complete) {
            state.store.finish_sweep(&sweep.id, now_secs())?;
        }
    }
    if !batch.events.is_empty() || !batch.calendar_sweeps.is_empty() {
        state
            .store
            .upsert_events(&batch.events, &batch.calendar_sweeps)?;
        // Each complete listing may mark only its own calendar's absent events missing.
        for sweep in &batch.calendar_sweeps {
            state
                .store
                .finish_calendar_sweep(&sweep.calendar, &sweep.id, now_secs())?;
        }
    }
    if let Some(Cursor(c)) = &batch.next_cursor {
        state.store.set_connector_cursor(source, Some(c))?;
    }
    Ok((
        SyncReport {
            threads: batch.threads.len(),
            messages,
            reminders: batch.reminders.len(),
            events: batch.events.len(),
        },
        changed,
    ))
}

/// Run [`sync_once`] forever at a fixed interval. Outcomes are audited by the guard.
pub async fn sync_loop(
    state: Arc<State>,
    connector: Arc<dyn Connector>,
    every: std::time::Duration,
) {
    let mut ticker = interval(every);
    loop {
        ticker.tick().await;
        let _ = sync_once(&state, &connector).await;
    }
}
