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

/// `--find <text>`: list only chats whose name matches, using Beeper's search.
fn find_from_args() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--find" {
            return args.next();
        }
    }
    None
}

/// `--probe <chat id>`: print only counts about that chat's newest messages (never text, names
/// or ids), to see why a chat reads empty.
fn probe_from_args() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--probe" {
            return args.next();
        }
    }
    None
}

async fn probe_chat(client: &BeeperClient, chat_id: &str) {
    match client
        .messages(
            chat_id,
            haste_connector_beeper::consts::BEEPER_PAGE_SIZE,
            None,
        )
        .await
    {
        Err(e) => eprintln!("probe: could not read messages: {e:?}"),
        Ok((items, more, cursor)) => {
            let now = haste_core::time::now_secs();
            println!(
                "\nProbe: {} messages in the newest page, hasMore={more}, cursor={}",
                items.len(),
                cursor.is_some()
            );
            let mut kinds: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            let (mut mapped, mut no_ts, mut no_key, mut hidden, mut deleted, mut own) =
                (0, 0, 0, 0, 0, 0);
            let mut ages: Vec<i64> = Vec::new();
            for m in &items {
                *kinds
                    .entry(m.kind.clone().unwrap_or_else(|| "none".into()))
                    .or_default() += 1;
                match haste_core::time::parse_rfc3339(&m.timestamp) {
                    Some(t) => ages.push((now - t) / 86_400),
                    None => no_ts += 1,
                }
                if m.sort_key.is_none() {
                    no_key += 1;
                }
                hidden += usize::from(m.is_hidden);
                deleted += usize::from(m.is_deleted);
                own += usize::from(m.is_sender);
                if haste_connector_beeper::mapper::map_message(
                    chat_id,
                    &BeeperSource::Imessage.source(),
                    m,
                )
                .is_some()
                {
                    mapped += 1;
                }
            }
            println!("  types: {kinds:?}");
            println!(
                "  age in days (newest..oldest): {:?}..{:?}",
                ages.iter().min(),
                ages.iter().max()
            );
            println!("  usable after mapping: {mapped}; no timestamp: {no_ts}; no sort key: {no_key}; hidden: {hidden}; deleted: {deleted}; sent by you: {own}");
        }
    }
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
                    a.provider().unwrap_or_else(|| {
                        "connection type not reported: attest it in beeper.json".to_owned()
                    })
                );
            }
        }
        Err(e) => eprintln!("could not list accounts: {e:?}"),
    }
    let listed = match find_from_args() {
        Some(q) => client.search_chats(&q).await,
        None => client.chats().await,
    };
    match listed {
        Ok(chats) => {
            println!("\nChats ({}): id, account, name", chats.len());
            for c in chats.iter().take(200) {
                println!("  {}  {}  {}", c.id, c.account_id, c.title);
            }
        }
        Err(e) => eprintln!("could not list chats: {e:?}"),
    }
    if let Some(id) = probe_from_args() {
        probe_chat(&client, &id).await;
    }
    println!(
        "\nChoose what to read in {}: account ids, `on_device_attested` (when the connection is not reported) or `allow_cloud` (accept a cloud account) where needed, and the chat ids to allow.",
        BeeperConfig::path(&home).display()
    );
    println!("Then restart hasted and release one source with `haste connector release whatsapp|messenger|imessage|googlechat`.");
    ExitCode::SUCCESS
}
