//! A small fake IMAP server on loopback for tests. It records every command line it receives
//! (LOGIN lines are recorded without arguments, so credentials never reach the log).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
pub struct TestPart {
    pub ty: &'static str,
    pub subtype: &'static str,
    pub params: Vec<(&'static str, String)>,
    pub encoding: &'static str,
    pub data: Vec<u8>,
    /// Rendered with an `attachment` disposition and this filename.
    pub attachment: Option<String>,
}

impl TestPart {
    #[must_use]
    pub fn plain(text: &str) -> Self {
        Self {
            ty: "text",
            subtype: "plain",
            params: vec![("charset", "utf-8".into())],
            encoding: "7bit",
            data: text.as_bytes().to_vec(),
            attachment: None,
        }
    }

    #[must_use]
    pub fn html(html: &str) -> Self {
        Self {
            ty: "text",
            subtype: "html",
            params: vec![("charset", "utf-8".into())],
            encoding: "7bit",
            data: html.as_bytes().to_vec(),
            attachment: None,
        }
    }

    #[must_use]
    pub fn pdf(name: &str, size: usize) -> Self {
        Self {
            ty: "application",
            subtype: "pdf",
            params: vec![("name", name.into())],
            encoding: "base64",
            data: vec![b'A'; size],
            attachment: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestMessage {
    pub uid: u32,
    /// Header lines without the terminating blank line.
    pub headers: String,
    pub parts: Vec<TestPart>,
}

impl TestMessage {
    #[must_use]
    pub fn simple(uid: u32, subject: &str, extra_headers: &str, body: &str) -> Self {
        Self {
            uid,
            headers: format!(
                "From: Ana <ana@example.org>\r\nTo: me@icloud.com\r\nSubject: {subject}\r\nMessage-ID: <m{uid}@example.org>\r\nDate: Tue, 14 Nov 2023 22:13:20 +0000\r\n{extra_headers}"
            ),
            parts: vec![TestPart::plain(body)],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FakeFolder {
    pub uid_validity: u32,
    pub messages: Vec<TestMessage>,
}

#[derive(Debug, Default)]
pub struct FakeState {
    pub folders: BTreeMap<String, FakeFolder>,
    /// The only password LOGIN accepts.
    pub password: String,
    /// Every command line received, with LOGIN arguments removed.
    pub commands: Vec<String>,
    pub login_attempts: u32,
    /// Accept and immediately drop this many connections (a transient failure).
    pub drop_next: u32,
}

pub struct FakeImap {
    pub port: u16,
    pub state: Arc<Mutex<FakeState>>,
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn part_structure(p: &TestPart) -> String {
    let params = if p.params.is_empty() {
        "NIL".to_owned()
    } else {
        format!(
            "({})",
            p.params
                .iter()
                .map(|(k, v)| format!("{} {}", quote(k), quote(v)))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let disposition = p.attachment.as_ref().map_or("NIL".to_owned(), |n| {
        format!("(\"attachment\" (\"filename\" {}))", quote(n))
    });
    let base = format!(
        "{} {} {} NIL NIL {} {}",
        quote(p.ty),
        quote(p.subtype),
        params,
        quote(p.encoding),
        p.data.len()
    );
    if p.ty == "text" {
        format!("({base} 1 NIL {disposition} NIL NIL)")
    } else {
        format!("({base} NIL {disposition} NIL NIL)")
    }
}

fn structure(m: &TestMessage) -> String {
    if m.parts.len() == 1 {
        part_structure(&m.parts[0])
    } else {
        format!(
            "({} \"mixed\")",
            m.parts.iter().map(part_structure).collect::<String>()
        )
    }
}

/// Parse `1,2,3` or `4:*` / `4:9` into UIDs present in the folder (with the `*` quirk).
fn select_uids(set: &str, present: &[u32]) -> Vec<u32> {
    let max = present.iter().copied().max().unwrap_or(0);
    let mut out = Vec::new();
    for item in set.split(',') {
        if let Some((a, b)) = item.split_once(':') {
            let a: u32 = a.parse().unwrap_or(1);
            let b: u32 = if b == "*" {
                max.max(a)
            } else {
                b.parse().unwrap_or(0)
            };
            let hits: Vec<u32> = present
                .iter()
                .copied()
                .filter(|u| *u >= a && *u <= b)
                .collect();
            if hits.is_empty() && b == max.max(a) && max > 0 && a > max {
                out.push(max); // the `*` quirk: the newest message is returned anyway
            }
            out.extend(hits);
        } else if let Ok(u) = item.parse::<u32>() {
            if present.contains(&u) {
                out.push(u);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn handle(stream: TcpStream, state: &Arc<Mutex<FakeState>>) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let _ = writer.write_all(b"* OK IMAP4rev1 fake ready\r\n");
    let mut selected: Option<String> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let cmd = line.trim_end().to_owned();
        let mut words = cmd.splitn(3, ' ');
        let tag = words.next().unwrap_or("").to_owned();
        let verb = words.next().unwrap_or("").to_uppercase();
        let rest = words.next().unwrap_or("").to_owned();
        let logged = if verb == "LOGIN" {
            format!("{tag} LOGIN")
        } else {
            cmd.clone()
        };
        let mut st = state.lock().expect("fake state");
        st.commands.push(
            logged
                .split_once(' ')
                .map_or(logged.clone(), |(_, r)| r.to_owned()),
        );
        let reply: Vec<u8> = match verb.as_str() {
            "LOGIN" => {
                st.login_attempts += 1;
                let supplied = rest
                    .split_once(' ')
                    .map_or("", |(_, p)| p)
                    .trim_matches('"')
                    .to_owned();
                if supplied == st.password {
                    format!("{tag} OK LOGIN completed\r\n").into_bytes()
                } else {
                    format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n").into_bytes()
                }
            }
            "EXAMINE" => {
                let name = rest.trim_matches('"').to_owned();
                match st.folders.get(&name) {
                    Some(f) => {
                        selected = Some(name);
                        format!(
                            "* FLAGS (\\Seen \\Answered)\r\n* {} EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY {}] valid\r\n* OK [UIDNEXT {}] next\r\n{tag} OK [READ-ONLY] EXAMINE completed\r\n",
                            f.messages.len(),
                            f.uid_validity,
                            f.messages.iter().map(|m| m.uid).max().unwrap_or(0) + 1
                        )
                        .into_bytes()
                    }
                    None => format!("{tag} NO [NONEXISTENT] no such mailbox\r\n").into_bytes(),
                }
            }
            "UID" if rest.to_uppercase().starts_with("SEARCH") => {
                let present: Vec<u32> = selected
                    .as_ref()
                    .and_then(|n| st.folders.get(n))
                    .map(|f| f.messages.iter().map(|m| m.uid).collect())
                    .unwrap_or_default();
                let query = rest[6..].trim().to_uppercase();
                let hits = match query.strip_prefix("UID ") {
                    Some(set) => select_uids(set, &present),
                    None => present,
                };
                let list = hits
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("* SEARCH {list}\r\n{tag} OK SEARCH completed\r\n").into_bytes()
            }
            "UID" if rest.to_uppercase().starts_with("FETCH") => {
                let args = rest[5..].trim();
                let (set, query) = args.split_once(' ').unwrap_or((args, ""));
                let folder = selected
                    .as_ref()
                    .and_then(|n| st.folders.get(n))
                    .cloned()
                    .unwrap_or_default();
                let present: Vec<u32> = folder.messages.iter().map(|m| m.uid).collect();
                let mut out: Vec<u8> = Vec::new();
                for uid in select_uids(set, &present) {
                    let Some((seq, m)) = folder
                        .messages
                        .iter()
                        .enumerate()
                        .find(|(_, m)| m.uid == uid)
                    else {
                        continue;
                    };
                    if query.contains("BODYSTRUCTURE") {
                        let header = format!("{}\r\n\r\n", m.headers);
                        out.extend_from_slice(
                            format!(
                                "* {} FETCH (UID {} BODYSTRUCTURE {} BODY[HEADER] {{{}}}\r\n",
                                seq + 1,
                                uid,
                                structure(m),
                                header.len()
                            )
                            .as_bytes(),
                        );
                        out.extend_from_slice(header.as_bytes());
                        out.extend_from_slice(b")\r\n");
                    } else if let Some(section) = query
                        .split("BODY.PEEK[")
                        .nth(1)
                        .and_then(|s| s.split(']').next())
                    {
                        let n: usize = section.parse().unwrap_or(1);
                        if let Some(part) = m.parts.get(n - 1) {
                            let limit: usize = query
                                .split_once("<0.")
                                .and_then(|(_, r)| r.split('>').next())
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(part.data.len());
                            let data = &part.data[..part.data.len().min(limit)];
                            out.extend_from_slice(
                                format!(
                                    "* {} FETCH (UID {} BODY[{}]<0> {{{}}}\r\n",
                                    seq + 1,
                                    uid,
                                    section,
                                    data.len()
                                )
                                .as_bytes(),
                            );
                            out.extend_from_slice(data);
                            out.extend_from_slice(b")\r\n");
                        }
                    }
                }
                out.extend_from_slice(format!("{tag} OK FETCH completed\r\n").as_bytes());
                out
            }
            "LOGOUT" => {
                let _ = writer
                    .write_all(format!("* BYE bye\r\n{tag} OK LOGOUT completed\r\n").as_bytes());
                return;
            }
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n").into_bytes(),
            // Anything else (STORE, APPEND, COPY, ...) is refused; the test inspects `commands`.
            _ => format!("{tag} BAD command not supported by the fake\r\n").into_bytes(),
        };
        drop(st);
        if writer.write_all(&reply).is_err() {
            return;
        }
    }
}

/// Start a fake server on `127.0.0.1:0`.
///
/// # Panics
/// If the loopback listener cannot be bound.
#[must_use]
pub fn spawn(state: FakeState) -> FakeImap {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let state = Arc::new(Mutex::new(state));
    let shared = Arc::clone(&state);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            {
                let mut st = shared.lock().expect("fake state");
                if st.drop_next > 0 {
                    st.drop_next -= 1;
                    continue; // dropping the stream closes the connection
                }
            }
            let shared = Arc::clone(&shared);
            thread::spawn(move || handle(stream, &shared));
        }
    });
    FakeImap { port, state }
}
