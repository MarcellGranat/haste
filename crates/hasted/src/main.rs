//! `hasted` entry point. Runs until interrupted; launchd restarts it.

use std::path::PathBuf;
use std::sync::Arc;

use hasted::herdr::HerdrConfig;
use hasted::notifier::{HerdrNotifier, Notifier};
use hasted::{start, DaemonConfig};

const DEFAULT_BASE: &str = "Library/Application Support/haste";

fn base_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HASTE_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(DEFAULT_BASE)
}

/// One log line per connector: started, not set up yet, or failed (the reason class only).
fn log_start<E>(name: &'static str, result: Result<bool, E>) -> Result<bool, E> {
    let state = match &result {
        Ok(true) => "started",
        Ok(false) => "not_set_up",
        Err(_) => "failed",
    };
    let level = if result.is_err() {
        haste_core::log::Level::Error
    } else {
        haste_core::log::Level::Info
    };
    haste_core::log::event(
        level,
        "daemon",
        "connector_start",
        &[
            haste_core::log::l("connector", name),
            haste_core::log::l("state", state),
        ],
    );
    result
}

fn enabled_features() -> &'static str {
    if cfg!(feature = "beeper") {
        "beeper"
    } else {
        "base"
    }
}

#[tokio::main]
async fn main() {
    let base = base_dir();
    // Logging first, so a failure to start is recorded too. A custom HASTE_HOME is logged as a
    // label (a stray variable pointing at a temporary folder once hid the real daemon).
    let _ = haste_core::log::init(
        &base,
        "hasted",
        haste_core::log::level_from_env(haste_core::log::Level::Info),
        Some(haste_core::log::Level::Warn),
    );
    haste_core::log::install_panic_hook();
    haste_core::log::info(
        "daemon",
        "starting",
        &[
            haste_core::log::id("version", env!("CARGO_PKG_VERSION")),
            haste_core::log::n("pid", std::process::id()),
            haste_core::log::l(
                "home",
                if std::env::var_os("HASTE_HOME").is_some() {
                    "custom"
                } else {
                    "default"
                },
            ),
            haste_core::log::l("beeper_feature", enabled_features()),
            haste_core::log::l(
                "herdr",
                if std::env::var("HASTE_HERDR").as_deref() == Ok("0") {
                    "off"
                } else {
                    "on"
                },
            ),
        ],
    );
    // Herdr observation is on unless HASTE_HERDR=0. The notifier shares the socket-health flag
    // with the supervisor so it knows when to fall back to the plain CLI.
    let herdr = (std::env::var("HASTE_HERDR").as_deref() != Ok("0")).then(HerdrConfig::from_env);
    let notifier: Arc<dyn Notifier> = match &herdr {
        Some(h) => Arc::new(HerdrNotifier::new(
            h.socket.clone(),
            h.runner.clone(),
            Arc::clone(&h.local_up),
        )),
        None => Arc::new(HerdrNotifier::default()),
    };
    let mut config = DaemonConfig::new(&base, notifier);
    config.herdr = herdr;
    // The notes folder: HASTE_NOTES_ROOT (a developer override), then notes.json, then
    // <data dir>/notes. The daemon never creates it; a missing folder denies note actions.
    match hasted::notes::NotesSettings::load(&base) {
        Ok(settings) => {
            config.notes_root = hasted::notes::notes_root(&base, &settings);
            config.notes_settings = settings;
        }
        Err(e) => {
            haste_core::log::error("daemon", "notes_settings_invalid", &[]);
            eprintln!("hasted: {e}");
            std::process::exit(1);
        }
    }
    match start(config).await {
        Ok(handle) => {
            haste_core::log::info("daemon", "listening", &[]);
            eprintln!("hasted: listening on {}", handle.socket_path.display());
            if hasted::report_interrupted_executions(&handle.state) > 0 {
                eprintln!(
                    "hasted: some reminder proposals need a check (see `haste proposal list`)"
                );
            }
            if hasted::start_reminders(&handle.state, &base) {
                eprintln!(
                    "hasted: reminders connector registered (kill switch decides when it runs)"
                );
            } else {
                eprintln!("hasted: reminders helper not found (build it: crates/haste-connector-reminders/helper/build.sh)");
            }
            #[cfg(feature = "icloud")]
            match log_start("icloud", hasted::start_icloud(&handle.state, &base)) {
                Ok(true) => eprintln!("hasted: icloud connector started"),
                Ok(false) => eprintln!(
                    "hasted: icloud not authorized yet (run `haste connector icloud auth`)"
                ),
                Err(e) => eprintln!("hasted: icloud connector not started: {e}"),
            }
            #[cfg(feature = "beeper")]
            match log_start("beeper", hasted::start_beeper(&handle.state, &base)) {
                Ok(true) => eprintln!(
                    "hasted: beeper connectors registered (whatsapp, messenger, imessage; kill switches decide when they run)"
                ),
                Ok(false) => eprintln!(
                    "hasted: beeper not set up yet (run `haste connector beeper auth`, then write beeper.json)"
                ),
                Err(e) => eprintln!("hasted: beeper connectors not started: {e}"),
            }
            #[cfg(feature = "gcal")]
            match log_start("gcal", hasted::start_gcal(&handle.state, &base)) {
                Ok(true) => eprintln!(
                    "hasted: gcal connector registered (kill switch decides when it runs)"
                ),
                Ok(false) => {
                    eprintln!("hasted: gcal not authorized yet (run `haste connector gcal auth`)")
                }
                Err(e) => eprintln!("hasted: gcal connector not started: {e}"),
            }
            #[cfg(feature = "gmail")]
            match log_start("gmail", hasted::start_gmail(&handle.state, &base)) {
                Ok(true) => eprintln!("hasted: gmail connector started"),
                Ok(false) => {
                    eprintln!("hasted: gmail not authorized yet (run `haste connector gmail auth`)")
                }
                Err(e) => eprintln!("hasted: gmail connector not started: {e}"),
            }
            // Ctrl-C and `kill`/`pkill` both stop it cleanly, and both are logged.
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    haste_core::log::info("daemon", "signal", &[haste_core::log::l("which", "interrupt")]);
                }
                _ = async {
                    match term.as_mut() {
                        Some(t) => { t.recv().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    haste_core::log::info("daemon", "signal", &[haste_core::log::l("which", "terminate")]);
                }
            }
            haste_core::log::info("daemon", "shutting_down", &[]);
            handle.shutdown().await;
            haste_core::log::info("daemon", "stopped", &[]);
        }
        Err(e) => {
            haste_core::log::error("daemon", "cannot_start", &[]);
            eprintln!("hasted: cannot start: {e}");
            std::process::exit(1);
        }
    }
}
