//! The only IMAP surface the connector can use: LOGIN, EXAMINE, UID SEARCH, UID FETCH (always
//! `BODY.PEEK`), LOGOUT, and a single write, APPEND, used only to file the copy of a message the
//! human just sent. There is no method that could issue STORE, COPY, MOVE, EXPUNGE, CREATE,
//! DELETE or RENAME, and no free-form command string.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use imap_proto::types::{BodyStructure, SectionPath};

use crate::config::IcloudConfig;

pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

/// How an IMAP failure should be treated by the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImapFailure {
    /// LOGIN rejected: terminal. The message never carries server text or credentials.
    Auth,
    /// Network, timeout, TLS or BYE: bounded retries within the cycle budget.
    Transient(String),
    /// Anything else (for example a missing folder). Never retried.
    Protocol(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderInfo {
    pub uid_validity: u32,
    pub exists: u32,
}

/// The only searches the connector makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchQuery {
    All,
    /// UIDs strictly above this one (`UID n+1:*`; the caller filters the `*` quirk).
    UidAbove(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentMeta {
    pub name: Option<String>,
    pub content_type: String,
    pub size: u32,
    /// The IMAP section of the part (`2`, `2.1`) and its transfer encoding, enough to fetch the
    /// file again: written into the stored line as `#<section>|<encoding>`.
    pub section: String,
    pub encoding: String,
}

/// Where a text body lives (`text/plain`, else `text/html`), taken from BODYSTRUCTURE so no
/// attachment is ever downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPart {
    pub section: Vec<u32>,
    /// `text/html` (reduced to text after fetching) rather than `text/plain`.
    pub html: bool,
    pub charset: String,
    pub encoding: String,
    pub octets: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderFetch {
    pub uid: u32,
    pub header: Vec<u8>,
    /// The part to read: `text/plain` if present, else `text/html`.
    pub text: Option<TextPart>,
    pub attachments: Vec<AttachmentMeta>,
    /// The `\Seen` flag is set on the server.
    pub seen: bool,
}

pub trait MailSession {
    /// # Errors
    /// See [`ImapFailure`].
    fn examine(&mut self, folder: &str) -> Result<FolderInfo, ImapFailure>;
    /// # Errors
    /// See [`ImapFailure`].
    fn uid_search(&mut self, query: SearchQuery) -> Result<Vec<u32>, ImapFailure>;
    /// Headers plus BODYSTRUCTURE for these UIDs. No body part is requested.
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<HeaderFetch>, ImapFailure>;
    /// The first `max` bytes of one text part. Attachments are never requested.
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn fetch_text_part(
        &mut self,
        uid: u32,
        part: &TextPart,
        max: u32,
    ) -> Result<Option<Vec<u8>>, ImapFailure>;
    /// File a copy of a message just sent into `folder` (marked `\Seen`). The only write the
    /// connector ever makes over IMAP; never used by the sync path.
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn append(&mut self, folder: &str, message: &[u8]) -> Result<(), ImapFailure>;
    /// Open `folder` read-write (for the human's archive only; sync always uses `examine`).
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn select(&mut self, folder: &str) -> Result<FolderInfo, ImapFailure>;
    /// UIDs of the messages in the open folder whose `Message-ID` header contains `id`. The id
    /// is refused when it holds a quote, a backslash or a control character.
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn uid_search_message_id(&mut self, id: &str) -> Result<Vec<u32>, ImapFailure>;
    /// UIDs of the messages in the open folder whose subject contains `subject` (the server
    /// matches a substring, case-insensitively). Refused when it holds a quote, a backslash or a
    /// control character.
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn uid_search_subject(&mut self, subject: &str) -> Result<Vec<u32>, ImapFailure>;
    /// Move messages of the open folder into `target` (`UID MOVE`: nothing is deleted).
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn uid_move(&mut self, uids: &[u32], target: &str) -> Result<(), ImapFailure>;
    /// Set or clear the `\Seen` flag of messages of the open folder (`UID STORE`; nothing is
    /// deleted). Servers and test doubles that cannot do it keep the default refusal.
    ///
    /// # Errors
    /// See [`ImapFailure`].
    fn uid_set_seen(&mut self, _uids: &[u32], _seen: bool) -> Result<(), ImapFailure> {
        Err(ImapFailure::Protocol("flags cannot be changed".into()))
    }
    fn logout(&mut self);
}

/// Opens an authenticated session. Production uses implicit TLS; tests use loopback plaintext.
pub trait Opener: Send + Sync {
    /// # Errors
    /// [`ImapFailure::Auth`] when LOGIN is rejected.
    fn open(
        &self,
        config: &IcloudConfig,
        password: &str,
    ) -> Result<Box<dyn MailSession>, ImapFailure>;
}

/// Which IMAP step failed. Only these fixed labels reach errors and the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Greeting,
    Login,
    Examine,
    Select,
    Search,
    FetchHeaders,
    FetchBody,
    Append,
    Move,
    Store,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Greeting => "greeting",
            Self::Login => "login",
            Self::Examine => "examine",
            Self::Select => "select",
            Self::Search => "uid search",
            Self::FetchHeaders => "fetch headers",
            Self::FetchBody => "fetch body",
            Self::Append => "append",
            Self::Move => "move",
            Self::Store => "store flags",
        }
    }
}

/// The error *kind* only; the payload (which can echo server text) is never included.
fn kind(error: &imap::Error) -> &'static str {
    match error {
        imap::Error::No(_) => "server refused the command",
        imap::Error::Bad(_) => "server rejected the command syntax",
        imap::Error::Parse(_) => "could not parse the server response",
        imap::Error::Validate(_) => "invalid argument",
        imap::Error::Io(_) => "i/o error",
        imap::Error::ConnectionLost => "connection lost",
        imap::Error::Tls(_) | imap::Error::TlsHandshake(_) => "tls error",
        _ => "unexpected response",
    }
}

fn classify(error: &imap::Error, stage: Stage) -> ImapFailure {
    let detail = format!("{}: {}", stage.label(), kind(error));
    match error {
        imap::Error::No(_) | imap::Error::Bad(_) if stage == Stage::Login => ImapFailure::Auth,
        imap::Error::Io(_)
        | imap::Error::ConnectionLost
        | imap::Error::Tls(_)
        | imap::Error::TlsHandshake(_) => ImapFailure::Transient(detail),
        _ => ImapFailure::Protocol(detail),
    }
}

/// What BODYSTRUCTURE says about a message's text parts and attachments.
#[derive(Debug, Default)]
struct Found {
    plain: Option<TextPart>,
    html: Option<TextPart>,
    attachments: Vec<AttachmentMeta>,
}

impl Found {
    /// `text/plain` wins; `text/html` is used only when there is no plain part.
    fn into_parts(self) -> (Option<TextPart>, Vec<AttachmentMeta>) {
        (self.plain.or(self.html), self.attachments)
    }
}

/// Walk BODYSTRUCTURE: find the first `text/plain` and `text/html` parts and list everything
/// else as attachment metadata. A non-multipart message has its single part at section `1`.
fn summarize(bs: &BodyStructure<'_>, path: &[u32], found: &mut Found) {
    let param = |params: &Option<Vec<(&str, &str)>>, key: &str| {
        params
            .as_ref()
            .and_then(|p| p.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)))
            .map(|(_, v)| (*v).to_owned())
    };
    match bs {
        BodyStructure::Multipart { bodies, .. } => {
            for (i, child) in bodies.iter().enumerate() {
                let mut p = path.to_vec();
                p.push(u32::try_from(i).unwrap_or(0) + 1);
                summarize(child, &p, found);
            }
        }
        BodyStructure::Text { common, other, .. } | BodyStructure::Basic { common, other, .. } => {
            let ty = format!(
                "{}/{}",
                common.ty.ty.to_lowercase(),
                common.ty.subtype.to_lowercase()
            );
            let is_attachment = common
                .disposition
                .as_ref()
                .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
            let part = |html: bool| TextPart {
                html,
                section: if path.is_empty() {
                    vec![1]
                } else {
                    path.to_vec()
                },
                charset: param(&common.ty.params, "charset").unwrap_or_else(|| "utf-8".into()),
                encoding: match other.transfer_encoding {
                    imap_proto::types::ContentEncoding::Base64 => "base64",
                    imap_proto::types::ContentEncoding::QuotedPrintable => "quoted-printable",
                    _ => "7bit",
                }
                .to_owned(),
                octets: other.octets,
            };
            if ty == "text/plain" && !is_attachment && found.plain.is_none() {
                found.plain = Some(part(false));
            } else if ty == "text/html" && !is_attachment && found.html.is_none() {
                found.html = Some(part(true));
            } else if is_attachment || !ty.starts_with("text/") {
                found.attachments.push(AttachmentMeta {
                    name: common
                        .disposition
                        .as_ref()
                        .and_then(|d| param(&d.params, "filename"))
                        .or_else(|| param(&common.ty.params, "name")),
                    content_type: ty,
                    size: other.octets,
                    section: section_of(path),
                    encoding: encoding_of(&other.transfer_encoding),
                });
            }
        }
        BodyStructure::Message { common, other, .. } => found.attachments.push(AttachmentMeta {
            name: None,
            content_type: format!(
                "{}/{}",
                common.ty.ty.to_lowercase(),
                common.ty.subtype.to_lowercase()
            ),
            size: other.octets,
            section: section_of(path),
            encoding: "7bit".to_owned(),
        }),
    }
}

/// `2.1` for the part path `[2, 1]`; a single-part message's part is `1`.
fn section_of(path: &[u32]) -> String {
    if path.is_empty() {
        "1".to_owned()
    } else {
        path.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

fn encoding_of(e: &imap_proto::types::ContentEncoding<'_>) -> String {
    match e {
        imap_proto::types::ContentEncoding::Base64 => "base64",
        imap_proto::types::ContentEncoding::QuotedPrintable => "quoted-printable",
        _ => "7bit",
    }
    .to_owned()
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

struct Imap<T: Read + Write> {
    session: imap::Session<T>,
}

impl<T: Read + Write> Imap<T> {
    /// UIDs of the open folder's messages whose `Message-ID` equals `id` (case-insensitive),
    /// found by reading the headers. Slower than SEARCH; used only when SEARCH finds nothing.
    fn scan_message_id(&mut self, id: &str) -> Result<Vec<u32>, ImapFailure> {
        let fetches = self
            .session
            .uid_fetch("1:*", "(UID BODY.PEEK[HEADER])")
            .map_err(|e| classify(&e, Stage::Search))?;
        let want = id.trim_matches(['<', '>']);
        let mut found = Vec::new();
        for f in fetches.iter() {
            let (Some(uid), Some(header)) = (f.uid, f.header()) else {
                continue;
            };
            let text = String::from_utf8_lossy(header)
                .replace("\r\n ", " ")
                .replace("\r\n\t", " ");
            let hit = text.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.trim().eq_ignore_ascii_case("message-id")
                        && value
                            .trim()
                            .trim_matches(['<', '>'])
                            .eq_ignore_ascii_case(want)
                })
            });
            if hit {
                found.push(uid);
            }
        }
        Ok(found)
    }
}

impl<T: Read + Write> MailSession for Imap<T> {
    fn examine(&mut self, folder: &str) -> Result<FolderInfo, ImapFailure> {
        // EXAMINE opens read-only, so no flag can change even by accident.
        let mailbox = self
            .session
            .examine(folder)
            .map_err(|e| classify(&e, Stage::Examine))?;
        Ok(FolderInfo {
            uid_validity: mailbox
                .uid_validity
                .ok_or_else(|| ImapFailure::Protocol("no UIDVALIDITY".into()))?,
            exists: mailbox.exists,
        })
    }

    fn uid_search(&mut self, query: SearchQuery) -> Result<Vec<u32>, ImapFailure> {
        let text = match query {
            SearchQuery::All => "ALL".to_owned(),
            SearchQuery::UidAbove(n) => format!("UID {}:*", n.saturating_add(1)),
        };
        let mut uids: Vec<u32> = self
            .session
            .uid_search(text)
            .map_err(|e| classify(&e, Stage::Search))?
            .into_iter()
            .collect();
        uids.sort_unstable();
        Ok(uids)
    }

    fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<HeaderFetch>, ImapFailure> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let fetches = self
            .session
            .uid_fetch(uid_set(uids), "(UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER])")
            .map_err(|e| classify(&e, Stage::FetchHeaders))?;
        let mut out = Vec::new();
        for f in fetches.iter() {
            let (Some(uid), Some(header)) = (f.uid, f.header()) else {
                continue;
            };
            let mut found = Found::default();
            if let Some(bs) = f.bodystructure() {
                summarize(bs, &[], &mut found);
            }
            let (text, attachments) = found.into_parts();
            out.push(HeaderFetch {
                uid,
                header: header.to_vec(),
                text,
                attachments,
                seen: f
                    .flags()
                    .iter()
                    .any(|fl| matches!(fl, imap::types::Flag::Seen)),
            });
        }
        Ok(out)
    }

    fn fetch_text_part(
        &mut self,
        uid: u32,
        part: &TextPart,
        max: u32,
    ) -> Result<Option<Vec<u8>>, ImapFailure> {
        let section = part
            .section
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".");
        let query = format!("(BODY.PEEK[{section}]<0.{}>)", part.octets.min(max).max(1));
        let fetches = self
            .session
            .uid_fetch(uid.to_string(), query)
            .map_err(|e| classify(&e, Stage::FetchBody))?;
        let path = SectionPath::Part(part.section.clone(), None);
        Ok(fetches
            .iter()
            .find_map(|f| f.section(&path).map(<[u8]>::to_vec)))
    }

    fn append(&mut self, folder: &str, message: &[u8]) -> Result<(), ImapFailure> {
        self.session
            .append_with_flags(folder, message, &[imap::types::Flag::Seen])
            .map_err(|e| classify(&e, Stage::Append))?;
        Ok(())
    }

    fn select(&mut self, folder: &str) -> Result<FolderInfo, ImapFailure> {
        let mailbox = self
            .session
            .select(folder)
            .map_err(|e| classify(&e, Stage::Select))?;
        Ok(FolderInfo {
            uid_validity: mailbox
                .uid_validity
                .ok_or_else(|| ImapFailure::Protocol("no UIDVALIDITY".into()))?,
            exists: mailbox.exists,
        })
    }

    fn uid_search_message_id(&mut self, id: &str) -> Result<Vec<u32>, ImapFailure> {
        if id.is_empty() || id.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
            return Err(ImapFailure::Protocol("search: unusable message id".into()));
        }
        let mut uids: Vec<u32> = self
            .session
            .uid_search(format!("HEADER Message-ID \"{id}\""))
            .map_err(|e| classify(&e, Stage::Search))?
            .into_iter()
            .collect();
        if uids.is_empty() {
            // some servers answer a HEADER search with nothing for ids they hold: read the
            // inbox headers and compare the Message-ID here instead
            uids = self.scan_message_id(id)?;
        }
        uids.sort_unstable();
        Ok(uids)
    }

    fn uid_search_subject(&mut self, subject: &str) -> Result<Vec<u32>, ImapFailure> {
        if subject.is_empty()
            || subject
                .chars()
                .any(|c| c.is_control() || c == '"' || c == '\\')
        {
            return Err(ImapFailure::Protocol("search: unusable subject".into()));
        }
        let mut uids: Vec<u32> = self
            .session
            .uid_search(format!("SUBJECT \"{subject}\""))
            .map_err(|e| classify(&e, Stage::Search))?
            .into_iter()
            .collect();
        uids.sort_unstable();
        Ok(uids)
    }

    fn uid_move(&mut self, uids: &[u32], target: &str) -> Result<(), ImapFailure> {
        if uids.is_empty() {
            return Ok(());
        }
        self.session
            .uid_mv(uid_set(uids), target)
            .map_err(|e| classify(&e, Stage::Move))
    }

    fn uid_set_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), ImapFailure> {
        if uids.is_empty() {
            return Ok(());
        }
        let query = if seen {
            "+FLAGS.SILENT (\\Seen)"
        } else {
            "-FLAGS.SILENT (\\Seen)"
        };
        self.session
            .uid_store(uid_set(uids), query)
            .map(|_| ())
            .map_err(|e| classify(&e, Stage::Store))
    }

    fn logout(&mut self) {
        let _ = self.session.logout();
    }
}

fn login<T: Read + Write + 'static>(
    mut client: imap::Client<T>,
    config: &IcloudConfig,
    password: &str,
) -> Result<Box<dyn MailSession>, ImapFailure> {
    client
        .read_greeting()
        .map_err(|e| classify(&e, Stage::Greeting))?;
    match client.login(&config.address, password) {
        Ok(session) => Ok(Box::new(Imap { session })),
        // The returned error is classified; the client (and its credentials) is dropped.
        Err((e, _client)) => Err(classify(&e, Stage::Login)),
    }
}

fn tcp(config: &IcloudConfig) -> Result<TcpStream, ImapFailure> {
    let addr = (config.host.as_str(), config.port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .ok_or_else(|| ImapFailure::Transient("cannot resolve host".into()))?;
    let stream = TcpStream::connect_timeout(&addr, SOCKET_TIMEOUT)
        .map_err(|_| ImapFailure::Transient("cannot connect".into()))?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT)).ok();
    stream.set_write_timeout(Some(SOCKET_TIMEOUT)).ok();
    Ok(stream)
}

/// Implicit TLS on port 993 with certificate verification on. A bad certificate is
/// reported as a transient failure and the connection is refused.
#[derive(Default)]
pub struct TlsOpener;

impl Opener for TlsOpener {
    fn open(
        &self,
        config: &IcloudConfig,
        password: &str,
    ) -> Result<Box<dyn MailSession>, ImapFailure> {
        let connector = native_tls::TlsConnector::builder()
            .build()
            .map_err(|_| ImapFailure::Transient("tls setup failed".into()))?;
        let stream = tcp(config)?;
        let tls = connector.connect(&config.host, stream).map_err(|_| {
            ImapFailure::Transient("tls handshake failed (certificate not accepted?)".into())
        })?;
        login(imap::Client::new(tls), config, password)
    }
}

/// Plaintext transport for tests against a loopback fake server. Never used in production.
#[derive(Default)]
pub struct PlainOpener;

impl Opener for PlainOpener {
    fn open(
        &self,
        config: &IcloudConfig,
        password: &str,
    ) -> Result<Box<dyn MailSession>, ImapFailure> {
        login(imap::Client::new(tcp(config)?), config, password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_failures_are_auth_and_other_no_responses_are_not() {
        assert_eq!(
            classify(&imap::Error::No("anything".into()), Stage::Login),
            ImapFailure::Auth
        );
        assert_eq!(
            classify(&imap::Error::Bad("anything".into()), Stage::Login),
            ImapFailure::Auth
        );
        assert!(matches!(
            classify(&imap::Error::No("no such folder".into()), Stage::Examine),
            ImapFailure::Protocol(_)
        ));
        assert!(matches!(
            classify(&imap::Error::ConnectionLost, Stage::Search),
            ImapFailure::Transient(_)
        ));
    }

    #[test]
    fn failure_text_never_echoes_server_or_credentials() {
        let f = classify(
            &imap::Error::No("LOGIN failed for me@icloud.com password hunter2".into()),
            Stage::Login,
        );
        assert!(!format!("{f:?}").contains("hunter2"));
        let f = classify(
            &imap::Error::No("secret subject".into()),
            Stage::FetchHeaders,
        );
        assert!(!format!("{f:?}").contains("secret"));
    }

    #[test]
    fn failures_name_the_stage_and_kind_but_never_the_payload() {
        let f = classify(
            &imap::Error::Bad("FETCH 7 body: Hello from Ana".into()),
            Stage::FetchBody,
        );
        assert_eq!(
            f,
            ImapFailure::Protocol("fetch body: server rejected the command syntax".into())
        );
        let f = classify(&imap::Error::ConnectionLost, Stage::Examine);
        assert_eq!(f, ImapFailure::Transient("examine: connection lost".into()));
    }

    /// The shape iCloud returned for a mail whose text parts have no Content-Transfer-Encoding
    /// header: the encoding field is `NIL` (names masked). Stock imap-proto rejected this.
    const ICLOUD_NIL_ENCODING: &[u8] = b"* 29 FETCH (UID 32 BODYSTRUCTURE (((\"text\" \"plain\" (\"CHARSET\" \"UTF-8\") NIL NIL NIL 73 1 NIL NIL NIL NIL)(\"text\" \"html\" (\"CHARSET\" \"UTF-8\") NIL NIL NIL 149 1 NIL NIL NIL NIL) \"alternative\" (\"BOUNDARY\" \"X\") NIL NIL NIL)(\"image\" \"png\" (\"NAME\" \"X\") \"X\" NIL \"base64\" 163946 NIL (\"INLINE\" (\"FILENAME\" \"X\")) NIL NIL) \"related\" (\"BOUNDARY\" \"X\") NIL NIL NIL))\r\n";

    #[test]
    fn a_nil_transfer_encoding_from_icloud_parses_and_is_read_as_7bit() {
        let (_, response) = imap_proto::parse_response(ICLOUD_NIL_ENCODING).expect("must parse");
        let imap_proto::types::Response::Fetch(_, attrs) = response else {
            panic!("not a fetch")
        };
        let bs = attrs
            .iter()
            .find_map(|a| match a {
                imap_proto::types::AttributeValue::BodyStructure(b) => Some(b),
                _ => None,
            })
            .expect("bodystructure");
        let mut found = Found::default();
        summarize(bs, &[], &mut found);
        let attachments = found.attachments;
        let plain = found.plain.expect("text/plain found");
        assert_eq!(
            found.html.as_ref().map(|h| h.section.clone()),
            Some(vec![1, 2])
        );
        assert_eq!(plain.section, vec![1, 1]);
        assert_eq!(plain.encoding, "7bit");
        assert_eq!(plain.charset, "UTF-8");
        assert_eq!(attachments.len(), 1, "the inline image is metadata only");
        assert_eq!(attachments[0].content_type, "image/png");
        assert_eq!(attachments[0].size, 163_946);
    }

    #[test]
    fn uid_sets_are_comma_separated() {
        assert_eq!(uid_set(&[3, 4, 9]), "3,4,9");
    }
}
