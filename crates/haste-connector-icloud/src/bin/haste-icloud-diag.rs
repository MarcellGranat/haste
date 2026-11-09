//! Diagnostic: print the BODYSTRUCTURE of recent messages with every quoted string that is not a
//! standard MIME term masked as "X". Shows message *shape* (types, encodings, sizes), never
//! subjects, names, addresses or text. Asks for the password without echo; never prints it.
//!
//! Usage: haste-icloud-diag --address you@icloud.com [--from-uid 32] [--folder INBOX]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::ExitCode;

const SAFE: &[&str] = &[
    "text",
    "plain",
    "html",
    "calendar",
    "csv",
    "css",
    "multipart",
    "mixed",
    "alternative",
    "related",
    "report",
    "signed",
    "encrypted",
    "parallel",
    "digest",
    "application",
    "pdf",
    "octet-stream",
    "zip",
    "json",
    "ics",
    "pkcs7-signature",
    "x-pkcs7-signature",
    "pkcs7-mime",
    "image",
    "png",
    "jpeg",
    "gif",
    "message",
    "rfc822",
    "delivery-status",
    "disposition-notification",
    "charset",
    "utf-8",
    "us-ascii",
    "iso-8859-1",
    "iso-8859-15",
    "windows-1252",
    "windows-1250",
    "format",
    "flowed",
    "delsp",
    "yes",
    "no",
    "7bit",
    "8bit",
    "binary",
    "quoted-printable",
    "base64",
    "attachment",
    "inline",
    "filename",
    "name",
    "boundary",
    "method",
    "request",
    "reply",
    "report-type",
    "protocol",
    "micalg",
    "smime-type",
    "size",
    "creation-date",
    "modification-date",
];

/// Mask every quoted string that is not a plain MIME vocabulary word.
fn mask(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            out.push(c);
            continue;
        }
        let mut inner = String::new();
        while let Some(n) = chars.next() {
            match n {
                '\\' => {
                    chars.next();
                    inner.push('?');
                }
                '"' => break,
                other => inner.push(other),
            }
        }
        if SAFE.contains(&inner.to_lowercase().as_str()) {
            out.push_str(&format!("\"{inner}\""));
        } else {
            out.push_str("\"X\"");
        }
    }
    out
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() -> ExitCode {
    let (mut address, mut from_uid, mut folder) = (String::new(), 32_u32, "INBOX".to_owned());
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next()) {
            ("--address", Some(v)) => address = v,
            ("--from-uid", Some(v)) => from_uid = v.parse().unwrap_or(32),
            ("--folder", Some(v)) => folder = v,
            _ => {
                eprintln!("usage: haste-icloud-diag --address you@icloud.com [--from-uid N] [--folder NAME]");
                return ExitCode::FAILURE;
            }
        }
    }
    let Ok(password) = rpassword::prompt_password("App-specific password: ") else {
        return ExitCode::FAILURE;
    };
    let connector = native_tls::TlsConnector::new().expect("tls");
    let tcp = TcpStream::connect(("imap.mail.me.com", 993)).expect("connect");
    let stream = connector
        .connect("imap.mail.me.com", tcp)
        .expect("tls handshake");
    let mut reader = BufReader::new(stream);
    let mut greeting = String::new();
    let _ = reader.read_line(&mut greeting);

    let mut run = |tag: &str, command: &str, show: bool| -> bool {
        let line = format!("{tag} {command}\r\n");
        if reader.get_mut().write_all(line.as_bytes()).is_err() {
            return false;
        }
        loop {
            let mut resp = String::new();
            if reader.read_line(&mut resp).unwrap_or(0) == 0 {
                return false;
            }
            if let Some(status) = resp.strip_prefix(tag) {
                return status.trim_start().starts_with("OK");
            }
            if show && resp.contains("FETCH") {
                println!("{}", mask(resp.trim_end()));
            }
        }
    };
    if !run(
        "a1",
        &format!("LOGIN {} {}", quote(&address), quote(&password)),
        false,
    ) {
        eprintln!("login failed");
        return ExitCode::FAILURE;
    }
    run("a2", &format!("EXAMINE {}", quote(&folder)), false);
    println!("--- UID FETCH {from_uid}:* (UID BODYSTRUCTURE), masked ---");
    run(
        "a3",
        &format!("UID FETCH {from_uid}:* (UID BODYSTRUCTURE)"),
        true,
    );
    run("a4", "LOGOUT", false);
    ExitCode::SUCCESS
}
