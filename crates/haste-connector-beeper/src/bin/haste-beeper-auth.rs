//! One-time Beeper token helper, started by `haste connector beeper auth` (human build).
//!
//! It reads the API token from the terminal without echo, checks it against the local Beeper
//! Desktop API (loopback only), and stores it in the macOS Keychain only if the check passes: a
//! failed check stores nothing. It never prints or logs the token and never writes it to a file.
//! Afterwards it lists your accounts and chats so you can choose what to put in `beeper.json`.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use haste_connector_beeper::client::BeeperClient;
use haste_connector_beeper::consts::{BEEPER_BASE_URL, KEYCHAIN_SERVICE, TOKEN_ACCOUNT};
use haste_connector_beeper::mapper::BeeperSource;
use haste_connector_beeper::BeeperConfig;
use haste_google_oauth::keychain::{KeychainStore, MemoryStore, TokenStore};
use haste_proto::{read_frame, write_frame, Request, RequestBody, Response};

fn fail(message: &str) -> ExitCode {
    eprintln!("haste-beeper-auth: {message}");
    ExitCode::FAILURE
}

fn home_from_args() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--home" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

/// Best effort: ask the running daemon to audit the authorization.
fn tell_daemon(home: &Path) -> Result<(), String> {
    let token = std::fs::read_to_string(home.join("human.token")).map_err(|e| e.to_string())?;
    let mut stream =
        UnixStream::connect(home.join("run").join("hasted.sock")).map_err(|e| e.to_string())?;
    let request = Request::new(
        token.trim(),
        RequestBody::ConnectorAuthorized {
            source: "beeper".into(),
        },
    );
    write_frame(&mut stream, &request).map_err(|e| format!("{e:?}"))?;
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::Done) => Ok(()),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(format!("{e:?}")),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let Some(home) = home_from_args() else {
        return fail("--home is required");
    };
    let base = match BeeperConfig::load(&home) {
        Ok(c) => c.base().to_owned(),
        Err(e) => return fail(&e),
    };
    println!("Create an access token in Beeper Desktop (Settings, Developer) and paste it below.");
    println!("It is not shown, not logged and not written to any file.");
    let token = match rpassword::prompt_password("Beeper API token: ") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_owned(),
        _ => return fail("no token entered"),
    };

    // Check with the token held only in memory; store it in the Keychain only on success.
    let probe = Arc::new(MemoryStore::default());
    if probe.set(TOKEN_ACCOUNT, &token).is_err() {
        return fail("internal error");
    }
    let client = match BeeperClient::new(&base, probe) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    if let Err(e) = client.check().await {
        eprintln!("haste-beeper-auth: the token was not accepted: {e:?}");
        return fail("nothing was stored (is Beeper Desktop running, and is the token right?)");
    }
    if let Err(e) = KeychainStore::new(KEYCHAIN_SERVICE).set(TOKEN_ACCOUNT, &token) {
        return fail(&e);
    }
    println!("Token accepted and stored in the Keychain ({KEYCHAIN_SERVICE}).");
    match tell_daemon(&home) {
        Ok(()) => println!("The daemon recorded the authorization."),
        Err(e) => eprintln!("note: could not tell the daemon ({e}); the token is saved anyway"),
    }

    // Help choose what to read: accounts and chats, for your own terminal only.
    if base != BEEPER_BASE_URL {
        println!("(using the base URL from beeper.json)");
    }
    match client.accounts().await {
        Ok(accounts) => {
            println!(
                "\nAccounts ({}): id, network, how it is connected",
                accounts.len()
            );
            for a in &accounts {
                let supported =
                    BeeperSource::from_network(&a.network).map_or("unsupported", |s| s.name());
                println!(
                    "  {}  {}  ({supported})  {}",
                    a.id,
                    a.network,
                    a.connection
                        .as_deref()
                        .unwrap_or("connection type not reported: attest it in beeper.json")
                );
            }
        }
        Err(e) => eprintln!("could not list accounts: {e:?}"),
    }
    match client.chats().await {
        Ok(chats) => {
            println!("\nChats ({}): id, account, name", chats.len());
            for c in chats.iter().take(200) {
                println!("  {}  {}  {}", c.id, c.account_id, c.title);
            }
        }
        Err(e) => eprintln!("could not list chats: {e:?}"),
    }
    println!(
        "\nChoose what to read in {}: account ids, `on_device_attested` where needed, and the chat ids to allow.",
        BeeperConfig::path(&home).display()
    );
    println!("Then restart hasted and release one source with `haste connector release whatsapp|messenger|imessage`.");
    ExitCode::SUCCESS
}
