//! One-time Gmail consent helper, started by `haste connector gmail auth` (human build).
//!
//! It runs the OAuth flow, stores the refresh token in the macOS Keychain, writes
//! `gmail.json` (client id/secret and account, mode 0600) and tells the daemon so the event
//! is audited. It never prints a token.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use haste_connector_gmail::auth::{run_consent, OAuthClient, OAuthEndpoints};
use haste_connector_gmail::client::Profile;
use haste_connector_gmail::consts::{API_BASE, AUTH_URL, CONSENT_TIMEOUT, HTTP_TIMEOUT, TOKEN_URL};
use haste_connector_gmail::keychain::{KeychainStore, TokenStore};
use haste_connector_gmail::GmailConfig;
use haste_proto::{read_frame, write_frame, Request, RequestBody, Response};

fn fail(message: &str) -> ExitCode {
    eprintln!("haste-gmail-auth: {message}");
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
fn tell_daemon(home: &std::path::Path) -> Result<(), String> {
    let token = std::fs::read_to_string(home.join("human.token")).map_err(|e| e.to_string())?;
    let mut stream =
        UnixStream::connect(home.join("run").join("hasted.sock")).map_err(|e| e.to_string())?;
    let request = Request::new(
        token.trim(),
        RequestBody::ConnectorAuthorized {
            source: "gmail".into(),
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
    let (Ok(client_id), Ok(client_secret)) = (
        std::env::var("HASTE_GMAIL_CLIENT_ID"),
        std::env::var("HASTE_GMAIL_CLIENT_SECRET"),
    ) else {
        return fail("set HASTE_GMAIL_CLIENT_ID and HASTE_GMAIL_CLIENT_SECRET");
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
    println!(
        "for your own project. Choose Advanced, then continue, and approve the two Gmail scopes."
    );
    let open = |url: &str| {
        println!("If the browser does not open, visit:\n{url}");
        let _ = Command::new("open").arg(url).status();
    };
    let consent = match run_consent(&endpoints, &client, &http, open, CONSENT_TIMEOUT).await {
        Ok(c) => c,
        Err(e) => return fail(&e.to_string()),
    };

    let profile = http
        .get(format!("{API_BASE}/gmail/v1/users/me/profile"))
        .bearer_auth(&consent.access_token)
        .send()
        .await;
    let profile: Profile = match profile {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(p) => p,
            Err(_) => return fail("could not read the Gmail profile"),
        },
        Ok(r) => {
            return fail(&format!(
                "profile request failed with http {}",
                r.status().as_u16()
            ))
        }
        Err(e) => return fail(&e.without_url().to_string()),
    };

    if let Err(e) = KeychainStore.set(&profile.email_address, &consent.refresh_token) {
        return fail(&e);
    }
    let config = GmailConfig {
        client_id,
        client_secret,
        account: profile.email_address.clone(),
    };
    if let Err(e) = config.save(&home) {
        return fail(&e);
    }
    match tell_daemon(&home) {
        Ok(()) => println!("The daemon recorded the authorization."),
        Err(e) => {
            eprintln!("note: could not tell the daemon ({e}); the authorization is saved anyway")
        }
    }
    println!("Authorized {}.", profile.email_address);
    println!("Restart hasted to start syncing, and run `haste connector release gmail` if it was engaged.");
    ExitCode::SUCCESS
}
