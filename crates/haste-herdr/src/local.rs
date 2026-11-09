//! The local herdr machine over its unix socket: protocol check, subscription, snapshot, events.
//!
//! Order on every (re)connection, so no event can be lost or applied against stale state:
//! 1. take a snapshot on a short-lived connection and check the protocol (mismatch stops here,
//!    before anything is subscribed);
//! 2. subscribe on a second, long-lived connection;
//! 3. take a second snapshot, send it, then send the events that arrived in the meantime.
//!
//! Pane and workspace lifecycle events carry no data haste needs: they only trigger a fresh
//! snapshot (which is also how a renamed workspace is noticed).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc::Sender;
use tokio::time::{sleep, timeout, Instant};

use crate::backoff::{clock_jitter, Backoff};
use crate::protocol::{parse_snapshot, FeedError, Snapshot, StatusEvent, HERDR_PROTOCOL};

/// A connection must stream this long before its backoff is forgotten.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(300);
/// Wait this long after a lifecycle event before re-reading the snapshot (coalesces bursts).
const RESNAPSHOT_DEBOUNCE: Duration = Duration::from_millis(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RESUBSCRIBE_PAUSE: Duration = Duration::from_millis(200);

/// The default socket: `~/.config/herdr/herdr.sock`, or `HASTE_HERDR_SOCKET`.
#[must_use]
pub fn default_socket() -> PathBuf {
    if let Some(p) = std::env::var_os("HASTE_HERDR_SOCKET") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".config").join("herdr").join("herdr.sock")
}

/// Subscriptions that need no pane id. Any of them only means "the layout may have changed":
/// the feed re-reads the snapshot. None of them carries pane output.
pub const GLOBAL_SUBSCRIPTIONS: [&str; 7] = [
    "pane.created",
    "pane.closed",
    "pane.exited",
    "pane.agent_detected",
    "workspace.created",
    "workspace.closed",
    "workspace.renamed",
];

/// Herdr's agent status subscription is per pane (it requires a `pane_id`), so there is one
/// of these for every pane in the latest snapshot.
pub const STATUS_SUBSCRIPTION: &str = "pane.agent_status_changed";

/// The `subscriptions` list for a set of panes: the global ones plus one status subscription per
/// pane.
#[must_use]
pub fn subscriptions_for(panes: &BTreeSet<String>) -> Vec<Value> {
    let mut subs: Vec<Value> = GLOBAL_SUBSCRIPTIONS
        .iter()
        .map(|t| json!({ "type": t }))
        .collect();
    subs.extend(
        panes
            .iter()
            .map(|p| json!({ "type": STATUS_SUBSCRIPTION, "pane_id": p })),
    );
    subs
}

fn pane_set(snapshot: &Snapshot) -> BTreeSet<String> {
    snapshot.panes.iter().map(|p| p.pane_id.clone()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    /// Reported on every failed connection attempt; the supervisor reports a change only once.
    Unreachable,
    Mismatch {
        observed: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalMsg {
    State(LocalState),
    Snapshot(Snapshot),
    Event(StatusEvent),
    /// An event or snapshot that could not be understood and was dropped.
    Dropped,
}

fn io(e: impl std::fmt::Display) -> FeedError {
    FeedError::Io(e.to_string())
}

/// One request on a fresh connection; returns the first response line.
async fn one_shot(path: &Path, id: &str, method: &str, params: Value) -> Result<String, FeedError> {
    let work = async {
        let stream = UnixStream::connect(path).await.map_err(io)?;
        let (read, mut write) = stream.into_split();
        let line = json!({ "id": id, "method": method, "params": params }).to_string() + "\n";
        write.write_all(line.as_bytes()).await.map_err(io)?;
        let mut lines = BufReader::new(read).lines();
        loop {
            match lines.next_line().await.map_err(io)? {
                Some(l)
                    if l.contains(&format!("\"id\":\"{id}\""))
                        || l.contains(&format!("\"id\": \"{id}\"")) =>
                {
                    return Ok(l)
                }
                Some(_) => {}
                None => return Err(FeedError::Io("connection closed".into())),
            }
        }
    };
    timeout(REQUEST_TIMEOUT, work)
        .await
        .map_err(|_| FeedError::Io("request timed out".into()))?
}

async fn snapshot(path: &Path) -> Result<Snapshot, FeedError> {
    parse_snapshot(&one_shot(path, "snap", "session.snapshot", json!({})).await?)
}

/// Why a session ended.
enum End {
    Mismatch(u32),
    Closed,
    ReceiverGone,
    /// The set of panes changed, so the per-pane subscriptions must be renewed.
    Resubscribe,
}

async fn send(tx: &Sender<LocalMsg>, msg: LocalMsg) -> bool {
    tx.send(msg).await.is_ok()
}

async fn session(path: &Path, tx: &Sender<LocalMsg>, started: &mut Option<Instant>) -> End {
    // 1. protocol check before subscribing; the same snapshot tells us which panes to watch
    let first = match snapshot(path).await {
        Ok(s) => s,
        Err(FeedError::Mismatch { observed, .. }) => return End::Mismatch(observed),
        Err(_) => return End::Closed,
    };
    let subscribed = pane_set(&first);
    // 2. subscribe and remember events that arrive before the snapshot
    let stream = match UnixStream::connect(path).await {
        Ok(s) => s,
        Err(_) => return End::Closed,
    };
    let (read, mut write) = stream.into_split();
    let subs = subscriptions_for(&subscribed);
    let request =
        json!({ "id": "sub", "method": "events.subscribe", "params": { "subscriptions": subs } })
            .to_string()
            + "\n";
    if write.write_all(request.as_bytes()).await.is_err() {
        return End::Closed;
    }
    let mut lines = BufReader::new(read).lines();
    let ack = timeout(REQUEST_TIMEOUT, async {
        loop {
            match lines.next_line().await {
                Ok(Some(l)) if l.contains("subscription_started") => return Ok(()),
                Ok(Some(l)) if l.contains("\"error\"") => {
                    return Err(FeedError::Io("herdr refused the subscription".into()))
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(FeedError::Io("closed before subscribing".into())),
                Err(e) => return Err(io(e)),
            }
        }
    })
    .await;
    match ack {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return End::Closed,
        Err(_) => return End::Closed,
    }
    // 3. the snapshot goes out first; events already queued on the subscription come after it
    match snapshot(path).await {
        Ok(s) => {
            let panes_now = pane_set(&s);
            if !send(tx, LocalMsg::Snapshot(s)).await {
                return End::ReceiverGone;
            }
            if panes_now != subscribed {
                return End::Resubscribe; // panes appeared or went away while subscribing
            }
        }
        Err(FeedError::Mismatch { observed, .. }) => return End::Mismatch(observed),
        Err(FeedError::Malformed(_)) => {
            if !send(tx, LocalMsg::Dropped).await {
                return End::ReceiverGone;
            }
            return End::Closed;
        }
        Err(_) => return End::Closed,
    }
    *started = Some(Instant::now());

    let mut resnapshot_at: Option<Instant> = None;
    loop {
        let wait = async {
            match resnapshot_at {
                Some(at) => sleep(at.saturating_duration_since(Instant::now())).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(l)) => l,
                    Ok(None) => return End::Closed,
                    Err(_) => return End::Closed,
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    if !send(tx, LocalMsg::Dropped).await { return End::ReceiverGone; }
                    continue;
                };
                match value.get("event").and_then(Value::as_str) {
                    Some("pane.agent_status_changed") => {
                        let parsed = value.get("data").cloned().and_then(|d| serde_json::from_value::<StatusEvent>(d).ok());
                        let msg = parsed.map_or(LocalMsg::Dropped, LocalMsg::Event);
                        if !send(tx, msg).await { return End::ReceiverGone; }
                    }
                    Some(_) => resnapshot_at = Some(Instant::now() + RESNAPSHOT_DEBOUNCE),
                    None => {}
                }
            }
            () = wait => {
                resnapshot_at = None;
                match snapshot(path).await {
                    Ok(s) => {
                        let panes_now = pane_set(&s);
                        if !send(tx, LocalMsg::Snapshot(s)).await { return End::ReceiverGone; }
                        if panes_now != subscribed {
                            return End::Resubscribe;
                        }
                    }
                    Err(FeedError::Mismatch { observed, .. }) => return End::Mismatch(observed),
                    Err(FeedError::Malformed(_)) => if !send(tx, LocalMsg::Dropped).await { return End::ReceiverGone; },
                    Err(_) => return End::Closed,
                }
            }
        }
    }
}

/// Keep the local link up until the receiver goes away or the protocol does not match.
/// Failures back off exponentially (see [`Backoff`]); the backoff is forgotten only after a
/// connection has streamed for [`HEALTHY_AFTER`].
pub async fn run(
    path: PathBuf,
    tx: Sender<LocalMsg>,
    mut backoff: Backoff,
    healthy_after: Duration,
) {
    loop {
        let mut started = None;
        match session(&path, &tx, &mut started).await {
            End::ReceiverGone => return,
            // A different set of panes: subscribe again straight away (a short pause keeps a
            // constantly changing layout from spinning).
            End::Resubscribe => sleep(RESUBSCRIBE_PAUSE).await,
            End::Mismatch(observed) => {
                let _ = send(&tx, LocalMsg::State(LocalState::Mismatch { observed })).await;
                return; // retry only on a daemon restart
            }
            End::Closed => {
                if started.is_some_and(|s: Instant| s.elapsed() >= healthy_after) {
                    backoff.reset();
                }
                if !send(&tx, LocalMsg::State(LocalState::Unreachable)).await {
                    return;
                }
                sleep(backoff.next_delay(clock_jitter())).await;
            }
        }
    }
}

/// The pinned protocol, for callers that want to print it.
#[must_use]
pub const fn pinned() -> u32 {
    HERDR_PROTOCOL
}
