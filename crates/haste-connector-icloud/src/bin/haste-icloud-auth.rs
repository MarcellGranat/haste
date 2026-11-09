//! One-time iCloud authorization helper, started by `haste connector icloud auth` (human build).
//!
//! It asks for the app-specific password without echo, checks it with a real LOGIN and
//! EXAMINE INBOX, then stores it in the macOS Keychain and writes `icloud.json` (mode 0600,
//! no secrets). It never prints the password.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use haste_connector_icloud::auth::authorize;
use haste_connector_icloud::config::{FolderConfig, IcloudConfig};
use haste_connector_icloud::secrets::KeychainStore;
use haste_connector_icloud::session::TlsOpener;
use haste_proto::{read_frame, write_frame, Request, RequestBody, Response};

fn fail(message: &str) -> ExitCode {
    eprintln!("haste-icloud-auth: {message}");
    ExitCode::FAILURE
}

struct Args {
    home: PathBuf,
    address: Option<String>,
    folders: Vec<String>,
    backfill: Option<usize>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        home: PathBuf::new(),
        address: None,
        folders: Vec::new(),
        backfill: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--home" => args.home = PathBuf::from(value()?),
            "--address" => args.address = Some(value()?),
            "--folder" => args.folders.push(value()?),
            "--backfill" => {
                args.backfill = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--backfill must be a number")?,
                )
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.home.as_os_str().is_empty() {
        return Err("--home is required".into());
    }
    Ok(args)
}

/// Best effort: ask the running daemon to audit the authorization.
fn tell_daemon(home: &std::path::Path) -> Result<(), String> {
    let token = std::fs::read_to_string(home.join("human.token")).map_err(|e| e.to_string())?;
    let mut stream =
        UnixStream::connect(home.join("run").join("hasted.sock")).map_err(|e| e.to_string())?;
    let request = Request::new(
        token.trim(),
        RequestBody::ConnectorAuthorized {
            source: "icloud".into(),
        },
    );
    write_frame(&mut stream, &request).map_err(|e| format!("{e:?}"))?;
    match read_frame::<_, Response>(&mut stream) {
        Ok(Response::Done) => Ok(()),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(format!("{e:?}")),
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => return fail(&e),
    };
    let address = match args.address {
        Some(a) => a,
        None => {
            print!("Apple ID (iCloud address): ");
            let _ = io::stdout().flush();
            let mut line = String::new();
            if io::stdin().read_line(&mut line).is_err() {
                return fail("could not read the address");
            }
            line.trim().to_owned()
        }
    };
    if !address.contains('@') {
        return fail("that does not look like an address");
    }
    println!(
        "Create an app-specific password at https://appleid.apple.com (Sign-In and Security)."
    );
    let password = match rpassword::prompt_password("App-specific password: ") {
        Ok(p) if !p.trim().is_empty() => p.trim().to_owned(),
        _ => return fail("no password entered"),
    };

    let mut config = IcloudConfig::new(&address);
    config.folders = args.folders.iter().map(|f| FolderConfig::new(f)).collect();
    if let Some(n) = args.backfill {
        config.backfill_count = n;
    }
    if let Err(e) = authorize(&TlsOpener, &KeychainStore, &args.home, &config, &password) {
        return fail(&e);
    }
    match tell_daemon(&args.home) {
        Ok(()) => println!("The daemon recorded the authorization."),
        Err(e) => {
            eprintln!("note: could not tell the daemon ({e}); the authorization is saved anyway")
        }
    }
    println!("Authorized {address}.");
    println!("Restart hasted to start syncing, and run `haste connector release icloud` if it was engaged.");
    ExitCode::SUCCESS
}
