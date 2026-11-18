//! The haste daemon: owns the store, policy gate, audit log and connector access.

pub mod dispatch;
pub mod filters;
pub mod herdr;
pub mod notes;
pub mod notifier;
pub mod scheduler;
pub mod state;

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use haste_core::audit::AuditWriter;
use haste_proto::{
    encode_frame, parse_header, ErrorCode, Event, FrameError, Request, RequestBody, Response, Topic,
};
use haste_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use state::{DaemonConfig, State};

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Running daemon. Dropping it does not stop it; call [`DaemonHandle::shutdown`].
pub struct DaemonHandle {
    pub socket_path: PathBuf,
    pub state: Arc<State>,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl DaemonHandle {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks {
            task.abort();
            let _ = task.await;
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn ensure_private_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        let mode = fs::metadata(path)?.mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} has mode {mode:o}; expected 700 or stricter",
                    path.display()
                ),
            ));
        }
        return Ok(());
    }
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(path)
}

fn write_private_file(path: &Path, contents: &str) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)?;
    f.write_all(contents.as_bytes())
}

/// Start the daemon: private directories, migrations, audit, socket, background tasks.
///
/// # Errors
/// Refuses to start when the socket directory is too permissive, the database schema is
/// newer than supported, or the audit chain is broken.
pub async fn start(config: DaemonConfig) -> io::Result<DaemonHandle> {
    ensure_private_dir(&config.run_dir)?;
    ensure_private_dir(&config.data_dir)?;
    let store = Store::open(&config.db_path()).map_err(|e| io::Error::other(e.to_string()))?;
    if let Some(token) = store
        .bootstrap_human()
        .map_err(|e| io::Error::other(e.to_string()))?
    {
        write_private_file(&config.human_token_path(), &token)?;
    }
    let audit = AuditWriter::open(&config.audit_path())?;
    let socket_path = config.socket_path();
    let _ = fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;
    let state = Arc::new(State::new(store, audit, config));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut tasks = vec![
        tokio::spawn(accept_loop(listener, Arc::clone(&state), shutdown_rx)),
        tokio::spawn(scheduler::expiry_loop(Arc::clone(&state))),
        tokio::spawn(scheduler::delivery_loop(Arc::clone(&state))),
        tokio::spawn(scheduler::scheduled_send_loop(Arc::clone(&state))),
    ];
    if let Some(cfg) = state.config.herdr.clone() {
        tasks.extend(herdr::start(&state, &cfg));
    }
    Ok(DaemonHandle {
        socket_path,
        state,
        shutdown: shutdown_tx,
        tasks,
    })
}

async fn accept_loop(
    listener: UnixListener,
    state: Arc<State>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => { tokio::spawn(handle_connection(Arc::clone(&state), stream)); }
                    Err(e) => eprintln!("hasted: accept failed: {e}"),
                }
            }
            _ = shutdown.changed() => break,
        }
    }
}

enum Incoming {
    Closed,
    Oversized(usize),
    Body(Vec<u8>),
}

async fn read_incoming(reader: &mut OwnedReadHalf) -> Incoming {
    let mut header = [0_u8; 4];
    if reader.read_exact(&mut header).await.is_err() {
        return Incoming::Closed;
    }
    match parse_header(header) {
        Ok(len) => {
            let mut body = vec![0_u8; len];
            if reader.read_exact(&mut body).await.is_err() {
                return Incoming::Closed;
            }
            Incoming::Body(body)
        }
        Err(FrameError::TooLarge(n)) => Incoming::Oversized(n),
        Err(_) => Incoming::Closed,
    }
}

async fn write_response(writer: &mut OwnedWriteHalf, response: &Response) -> bool {
    match encode_frame(response) {
        Ok(bytes) => writer.write_all(&bytes).await.is_ok(),
        Err(_) => false,
    }
}

async fn handle_connection(state: Arc<State>, stream: UnixStream) {
    let (mut reader, mut writer) = stream.into_split();
    loop {
        let body = match read_incoming(&mut reader).await {
            Incoming::Closed => return,
            Incoming::Oversized(n) => {
                state.audit(
                    "unknown",
                    "frame",
                    "oversized",
                    None,
                    Some(&format!("announced_bytes={n}")),
                );
                return;
            }
            Incoming::Body(b) => b,
        };
        let request: Request = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(_) => {
                if !write_response(
                    &mut writer,
                    &Response::error(ErrorCode::Validation, "malformed request"),
                )
                .await
                {
                    return;
                }
                continue;
            }
        };
        let topics = match &request.body {
            RequestBody::Subscribe { topics } => Some(topics.clone()),
            _ => None,
        };
        // Subscribe before dispatch so no event is missed between ack and streaming.
        let events = topics.as_ref().map(|_| state.events.subscribe());
        let st = Arc::clone(&state);
        let response = tokio::task::spawn_blocking(move || dispatch::dispatch(&st, request))
            .await
            .unwrap_or_else(|_| Response::error(ErrorCode::Internal, "handler failed"));
        let subscribed = matches!(response, Response::Subscribed);
        if !write_response(&mut writer, &response).await {
            return;
        }
        if let (true, Some(topics), Some(rx)) = (subscribed, topics, events) {
            stream_events(&mut reader, &mut writer, rx, &topics).await;
            return;
        }
    }
}

fn wanted(event: &Event, topics: &[Topic]) -> bool {
    match event {
        Event::ThreadChanged { .. } => topics.contains(&Topic::Threads),
        Event::ProposalChanged { .. } => topics.contains(&Topic::Proposals),
        Event::Notice { .. } => topics.contains(&Topic::Notices),
    }
}

async fn stream_events(
    reader: &mut OwnedReadHalf,
    writer: &mut OwnedWriteHalf,
    mut rx: tokio::sync::broadcast::Receiver<Event>,
    topics: &[Topic],
) {
    let mut probe = [0_u8; 1];
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(e) if wanted(&e, topics) => {
                    if !write_response(writer, &Response::Event { event: e }).await { return; }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
            // A read returning 0 or an error means the client went away.
            r = reader.read(&mut probe) => if matches!(r, Ok(0) | Err(_)) { return },
        }
    }
}

/// How often the Reminders mirror is refreshed.
pub const REMINDERS_SYNC_INTERVAL_SECS: u64 = 60;
/// Where the daemon looks for the helper unless `HASTE_REMINDERS_HELPER` says otherwise:
/// next to the daemon binary, then in the data directory.
pub const REMINDERS_HELPER_NAME: &str = "haste-reminders-helper";

/// Register the Reminders connector and start its scheduled sync. The connector starts with the
/// kill switch engaged the first time (no call runs until the human releases it).
///
/// Returns `false` when no helper binary can be found.
pub fn start_reminders(state: &Arc<State>, base: &Path) -> bool {
    use haste_connector_reminders::ProcessHelper;

    let Some(bin) = find_reminders_helper(base) else {
        return false;
    };
    start_reminders_with(
        state,
        Arc::new(ProcessHelper::new(bin)),
        REMINDERS_SYNC_INTERVAL_SECS,
    );
    true
}

/// Where the helper binary is, if anywhere.
#[must_use]
pub fn find_reminders_helper(base: &Path) -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("HASTE_REMINDERS_HELPER") {
        return Some(std::path::PathBuf::from(p));
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(REMINDERS_HELPER_NAME)));
    // hasted runs from an app bundle, so the helper is also looked up in the cargo bin folder
    let cargo_bin = std::env::var_os("HOME").map(|h| {
        std::path::PathBuf::from(h)
            .join(".cargo/bin")
            .join(REMINDERS_HELPER_NAME)
    });
    [beside, Some(base.join(REMINDERS_HELPER_NAME)), cargo_bin]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

/// Start the Reminders connector with any helper (tests use a fake).
pub fn start_reminders_with(
    state: &Arc<State>,
    helper: Arc<dyn haste_connector_reminders::Helper>,
    every_secs: u64,
) {
    use haste_connector_reminders::{CycleAudit, RemindersConnector};

    let audit_state = Arc::clone(state);
    // One audit entry per cycle: counts only, never a title, note or list name.
    let audit: CycleAudit = Arc::new(move |outcome, detail| {
        audit_state.audit(
            "connector",
            "reminders.sync",
            outcome,
            Some("reminders"),
            Some(detail),
        );
    });
    if let Ok(mut slot) = state.reminders_helper.write() {
        *slot = Some(Arc::clone(&helper));
    }
    let connector: Arc<dyn haste_core::connector::Connector> = Arc::new(
        RemindersConnector::new(helper, audit)
            .with_settings(state.config.data_dir.join("reminders.json")),
    );
    state.register_connector(Arc::clone(&connector));
    tokio::spawn(scheduler::sync_loop(
        Arc::clone(state),
        connector,
        std::time::Duration::from_secs(every_secs),
    ));
}

/// Proposals left `executing` by an earlier run are never re-run; they are listed for a check.
pub fn report_interrupted_executions(state: &Arc<State>) -> usize {
    let n = state.store.executing_proposals().map_or(0, |p| p.len());
    if n > 0 {
        state.audit(
            "system",
            "proposal.execute",
            "needs_check",
            None,
            Some(&format!("count={n}")),
        );
    }
    n
}

/// Load `gmail.json` and the Keychain refresh token, register the Gmail connector and start
/// its scheduled sync. Returns `Ok(false)` when Gmail has not been authorized yet.
///
/// # Errors
/// An unreadable or invalid `gmail.json`.
#[cfg(feature = "gmail")]
pub fn start_gmail(state: &Arc<State>, base: &Path) -> Result<bool, String> {
    use haste_connector_gmail::consts::GMAIL_SYNC_INTERVAL_SECS;
    use haste_connector_gmail::keychain::KeychainStore;
    use haste_connector_gmail::{GmailConfig, GmailConnector};

    if !GmailConfig::path(base).exists() {
        return Ok(false);
    }
    let config = GmailConfig::load(base)?;
    let connector: Arc<dyn haste_core::connector::Connector> = Arc::new(
        GmailConnector::from_config(&config, Arc::new(KeychainStore))
            .with_tabs(haste_connector_gmail::mapper::Tabs::load(base))
            // a diagnostic: the file `gmail_probe` in the data folder turns it on for one start
            .with_probe(base.join("gmail_probe").exists()),
    );
    state.register_connector(Arc::clone(&connector));
    tokio::spawn(scheduler::sync_loop(
        Arc::clone(state),
        connector,
        std::time::Duration::from_secs(GMAIL_SYNC_INTERVAL_SECS),
    ));
    Ok(true)
}

/// Register the three Beeper connectors (WhatsApp, Messenger, iMessage), each engaged on first
/// install, and start each one's polling loop. The token is read lazily on the first request, and
/// no request is made while a source's kill switch is engaged, so a fresh install reads nothing.
#[cfg(feature = "beeper")]
pub fn start_beeper_with(
    state: &Arc<State>,
    client: Arc<haste_connector_beeper::client::BeeperClient>,
    config: &haste_connector_beeper::BeeperConfig,
    every_secs: u64,
) {
    use haste_connector_beeper::{BeeperConnector, BeeperSource, CycleAudit};

    for which in BeeperSource::ALL {
        let audit_state = Arc::clone(state);
        let name = which.name();
        // One audit entry per cycle: counts and fixed labels only, never a chat name or text.
        let audit: CycleAudit = Arc::new(move |outcome, detail| {
            audit_state.audit(
                "connector",
                &format!("{name}.sync"),
                outcome,
                Some(name),
                Some(detail),
            );
        });
        let connector: Arc<dyn haste_core::connector::Connector> = Arc::new(BeeperConnector::new(
            which,
            Arc::clone(&client),
            config,
            audit,
        ));
        state.register_connector(Arc::clone(&connector));
        tokio::spawn(scheduler::sync_loop(
            Arc::clone(state),
            connector,
            std::time::Duration::from_secs(every_secs),
        ));
    }
}

/// Load `beeper.json` and the Keychain token store, then register the three Beeper connectors.
/// Returns `Ok(false)` when Beeper has not been set up (no `beeper.json` selection yet).
///
/// # Errors
/// An invalid `beeper.json` (including a non-loopback base URL).
#[cfg(feature = "beeper")]
pub fn start_beeper(state: &Arc<State>, base: &Path) -> Result<bool, String> {
    use haste_connector_beeper::consts::{BEEPER_POLL_SECS, KEYCHAIN_SERVICE};
    use haste_connector_beeper::{client::BeeperClient, BeeperConfig};
    use haste_google_oauth::keychain::KeychainStore;

    if !BeeperConfig::path(base).exists() {
        return Ok(false);
    }
    let config = BeeperConfig::load(base)?;
    let client = BeeperClient::new(
        config.base(),
        Arc::new(KeychainStore::new(KEYCHAIN_SERVICE)),
    )
    .map_err(str::to_owned)?;
    start_beeper_with(state, Arc::new(client), &config, BEEPER_POLL_SECS);
    Ok(true)
}

/// Load `gcal.json` and the Keychain items, register the Google Calendar connector (engaged on
/// first install) and start its scheduled sync. Returns `Ok(false)` when Calendar has not been
/// authorized yet.
///
/// # Errors
/// An unreadable `gcal.json` or Keychain failure.
#[cfg(feature = "gcal")]
pub fn start_gcal(state: &Arc<State>, base: &Path) -> Result<bool, String> {
    use haste_connector_gcal::consts::{GCAL_SYNC_INTERVAL_SECS, KEYCHAIN_SERVICE};
    use haste_connector_gcal::{ClientCredentials, CycleAudit, GcalConfig, GcalConnector};
    use haste_google_oauth::keychain::KeychainStore;

    if !GcalConfig::path(base).exists() {
        return Ok(false);
    }
    let config = GcalConfig::load(base)?;
    let store = Arc::new(KeychainStore::new(KEYCHAIN_SERVICE));
    let credentials = ClientCredentials::load(store.as_ref())?;
    let audit_state = Arc::clone(state);
    // One audit entry per cycle: counts only, never a title, attendee or location.
    let audit: CycleAudit = Arc::new(move |outcome, detail| {
        audit_state.audit(
            "connector",
            "gcal.sync",
            outcome,
            Some("gcal"),
            Some(detail),
        );
    });
    let connector: Arc<dyn haste_core::connector::Connector> = Arc::new(
        GcalConnector::from_config(&config, &credentials, store, audit),
    );
    state.register_connector(Arc::clone(&connector));
    tokio::spawn(scheduler::sync_loop(
        Arc::clone(state),
        connector,
        std::time::Duration::from_secs(GCAL_SYNC_INTERVAL_SECS),
    ));
    Ok(true)
}

/// Load `icloud.json`, register the iCloud connector and start its scheduled sync. Returns
/// `Ok(false)` when iCloud has not been authorized yet.
///
/// # Errors
/// An unreadable or invalid `icloud.json`.
#[cfg(feature = "icloud")]
pub fn start_icloud(state: &Arc<State>, base: &Path) -> Result<bool, String> {
    use haste_connector_icloud::config::ICLOUD_SYNC_INTERVAL_SECS;
    use haste_connector_icloud::secrets::KeychainStore;
    use haste_connector_icloud::session::TlsOpener;
    use haste_connector_icloud::{CycleAudit, IcloudConfig, IcloudConnector};

    if !IcloudConfig::path(base).exists() {
        return Ok(false);
    }
    let config = IcloudConfig::load(base)?;
    let audit_state = Arc::clone(state);
    // One audit entry per cycle: folder names and counts only, never subjects or bodies.
    let audit: CycleAudit = Arc::new(move |outcome, detail| {
        audit_state.audit(
            "connector",
            "icloud.sync",
            outcome,
            Some("icloud"),
            Some(detail),
        );
    });
    let connector: Arc<dyn haste_core::connector::Connector> = Arc::new(IcloudConnector::new(
        config,
        Arc::new(KeychainStore),
        Arc::new(TlsOpener),
        audit,
    ));
    state.register_connector(Arc::clone(&connector));
    tokio::spawn(scheduler::sync_loop(
        Arc::clone(state),
        connector,
        std::time::Duration::from_secs(ICLOUD_SYNC_INTERVAL_SECS),
    ));
    Ok(true)
}
