//! One-time Google Calendar consent helper, started by `haste connector gcal auth` (human build).
//!
//! It runs the OAuth flow for the two calendar scopes, stores the refresh token in the macOS
//! Keychain under `com.haste.gcal` (the Gmail item is never touched), keeps the OAuth client in
//! the same service under a separate account, and writes the non-secret `gcal.json`. It prints
//! the calendars it found so you can choose which to sync, and never prints a token.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use haste_connector_gcal::consts::{
    API_BASE, CONSENT_TIMEOUT, HTTP_TIMEOUT, KEYCHAIN_SERVICE, SCOPES,
};
use haste_connector_gcal::{ClientCredentials, GcalConfig};
use haste_google_oauth::auth::{run_consent, OAuthClient, OAuthEndpoints};
use haste_google_oauth::keychain::{KeychainStore, TokenStore};
use haste_google_oauth::{AUTH_URL, TOKEN_URL};
use haste_proto::{read_frame, write_frame, Request, RequestBody, Response};

fn fail(message: &str) -> ExitCode {
    eprintln!("haste-gcal-auth: {message}");
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

/// The OAuth client: `HASTE_GCAL_*` first, then the Gmail variables (same Google client).
fn client_from_env() -> Option<(String, String)> {
    let get = |a: &str, b: &str| std::env::var(a).or_else(|_| std::env::var(b)).ok();
    Some((
        get("HASTE_GCAL_CLIENT_ID", "HASTE_GMAIL_CLIENT_ID")?,
        get("HASTE_GCAL_CLIENT_SECRET", "HASTE_GMAIL_CLIENT_SECRET")?,
    ))
}

/// Best effort: ask the running daemon to audit the authorization.
fn tell_daemon(home: &Path) -> Result<(), String> {
    let token = std::fs::read_to_string(home.join("human.token")).map_err(|e| e.to_string())?;
    let mut stream =
        UnixStream::connect(home.join("run").join("hasted.sock")).map_err(|e| e.to_string())?;
    let request = Request::new(
        token.trim(),
        RequestBody::ConnectorAuthorized {
            source: "gcal".into(),
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
    let Some((client_id, client_secret)) = client_from_env() else {
        return fail(
            "set HASTE_GMAIL_CLIENT_ID and HASTE_GMAIL_CLIENT_SECRET (or the HASTE_GCAL_ ones)",
        );
    };
    let http = match reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
        Ok(h) => h,
        Err(e) => return fail(&e.to_string()),
    };
    let endpoints = OAuthEndpoints {
        auth_url: AUTH_URL.into(),
        token_url: TOKEN_URL.into(),
    };
    let client = OAuthClient {
        client_id: client_id.clone(),
        client_secret: client_secret.clone(),
    };

    println!("Opening your browser. Google will warn that the app is unverified: that is expected");
    println!("for your own project. Choose Advanced, then continue, and approve the two calendar scopes.");
    let open = |url: &str| {
        println!("If the browser does not open, visit:\n{url}");
        let _ = Command::new("open").arg(url).status();
    };
    let consent =
        match run_consent(&endpoints, &client, &SCOPES, &http, open, CONSENT_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => return fail(&e.to_string()),
        };

    // Which calendars exist (the primary one names the account).
    let response = http
        .get(format!("{API_BASE}/users/me/calendarList"))
        .bearer_auth(&consent.access_token)
        .send()
        .await;
    let list: serde_json::Value = match response {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(v) => v,
            Err(_) => return fail("could not read the calendar list"),
        },
        Ok(r) => {
            return fail(&format!(
                "calendar list failed with http {} (is the Calendar API enabled in your Google Cloud project?)",
                r.status().as_u16()
            ))
        }
        Err(e) => return fail(&e.without_url().to_string()),
    };
    let items = list["items"].as_array().cloned().unwrap_or_default();
    let account = items
        .iter()
        .find(|c| c["primary"].as_bool() == Some(true))
        .and_then(|c| c["id"].as_str())
        .map(str::to_owned);
    let Some(account) = account else {
        return fail("no primary calendar found; cannot name the account");
    };

    let store = KeychainStore::new(KEYCHAIN_SERVICE);
    if let Err(e) = store.set(&account, &consent.refresh_token) {
        return fail(&e);
    }
    let credentials = ClientCredentials {
        client_id,
        client_secret,
    };
    if let Err(e) = credentials.save(&store) {
        return fail(&e);
    }
    // keep a calendar choice made earlier; a first run selects nothing yet
    let mut config = GcalConfig::load(&home).unwrap_or_default();
    config.account = account.clone();
    if let Err(e) = config.save(&home) {
        return fail(&e);
    }
    match tell_daemon(&home) {
        Ok(()) => println!("The daemon recorded the authorization."),
        Err(e) => {
            eprintln!("note: could not tell the daemon ({e}); the authorization is saved anyway")
        }
    }

    println!("Authorized {account}. Calendars found ({}):", items.len());
    for c in &items {
        println!(
            "  {}  {}",
            c["id"].as_str().unwrap_or("?"),
            c["summary"].as_str().unwrap_or("")
        );
    }
    println!(
        "\nChoose which to sync: put their ids in the \"calendars\" list of {}",
        GcalConfig::path(&home).display()
    );
    println!("Then restart hasted and run `haste connector release gcal`.");
    ExitCode::SUCCESS
}
