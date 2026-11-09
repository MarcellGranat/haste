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

#[tokio::main]
async fn main() {
    let base = base_dir();
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
    if let Some(notes) = std::env::var_os("HASTE_NOTES_ROOT") {
        config.notes_root = PathBuf::from(notes);
    }
    match start(config).await {
        Ok(handle) => {
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
            match hasted::start_icloud(&handle.state, &base) {
                Ok(true) => eprintln!("hasted: icloud connector started"),
                Ok(false) => eprintln!(
                    "hasted: icloud not authorized yet (run `haste connector icloud auth`)"
                ),
                Err(e) => eprintln!("hasted: icloud connector not started: {e}"),
            }
            #[cfg(feature = "gcal")]
            match hasted::start_gcal(&handle.state, &base) {
                Ok(true) => eprintln!(
                    "hasted: gcal connector registered (kill switch decides when it runs)"
                ),
                Ok(false) => {
                    eprintln!("hasted: gcal not authorized yet (run `haste connector gcal auth`)")
                }
                Err(e) => eprintln!("hasted: gcal connector not started: {e}"),
            }
            #[cfg(feature = "gmail")]
            match hasted::start_gmail(&handle.state, &base) {
                Ok(true) => eprintln!("hasted: gmail connector started"),
                Ok(false) => {
                    eprintln!("hasted: gmail not authorized yet (run `haste connector gmail auth`)")
                }
                Err(e) => eprintln!("hasted: gmail connector not started: {e}"),
            }
            let _ = tokio::signal::ctrl_c().await;
            handle.shutdown().await;
        }
        Err(e) => {
            eprintln!("hasted: cannot start: {e}");
            std::process::exit(1);
        }
    }
}
