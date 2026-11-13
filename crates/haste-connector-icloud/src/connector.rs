//! The iCloud `Connector`: read-only, polled, one blocking IMAP session per cycle.

use std::sync::Arc;

use haste_core::connector::{
    ArchiveTarget, AttachmentRequest, Batch, BoxFuture, Capabilities, Connector, ConnectorError,
    Cursor, DraftToSend, SendReceipt,
};
use haste_core::model::Source;

use crate::config::IcloudConfig;
use crate::secrets::SecretStore;
use crate::session::{ImapFailure, Opener};
use crate::smtp::{submit, SmtpFailure, SmtpTransport, TlsSmtp};
use crate::sync::{sync_all, IcloudCursor};

/// Receives `(outcome, detail)` once per cycle. The daemon turns it into an audit entry.
/// Details are counts and folder names only.
pub type CycleAudit = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct IcloudConnector {
    config: IcloudConfig,
    secrets: Arc<dyn SecretStore>,
    opener: Arc<dyn Opener>,
    audit: CycleAudit,
    /// How mail is submitted; production uses STARTTLS, tests a loopback fake.
    smtp: Arc<dyn SmtpTransport>,
}

impl IcloudConnector {
    #[must_use]
    pub fn new(
        config: IcloudConfig,
        secrets: Arc<dyn SecretStore>,
        opener: Arc<dyn Opener>,
        audit: CycleAudit,
    ) -> Self {
        Self {
            config,
            secrets,
            opener,
            audit,
            smtp: Arc::new(TlsSmtp),
        }
    }

    /// Use another SMTP transport (tests point this at a loopback fake).
    #[must_use]
    pub fn with_smtp(mut self, smtp: Arc<dyn SmtpTransport>) -> Self {
        self.smtp = smtp;
        self
    }
}

/// Map an SMTP failure to the connector error that drives the proposal's next state:
/// nothing delivered (`Transient`/`Other`/`Checkpoint`) or possibly delivered (`Ambiguous`).
fn smtp_error(failure: SmtpFailure) -> ConnectorError {
    match failure {
        SmtpFailure::Connect(m) => ConnectorError::Transient(format!("smtp: {m}")),
        SmtpFailure::AuthRejected => ConnectorError::Checkpoint(
            "iCloud rejected the login; renew the app-specific password".into(),
        ),
        SmtpFailure::Refused(m) => ConnectorError::Other(format!("send refused: {m}")),
        SmtpFailure::Ambiguous(m) => ConnectorError::Ambiguous(format!("smtp: {m}")),
    }
}

/// Authentication failure is terminal (breaker trips, kill switch engages, human notified);
/// network trouble is transient; anything else is a plain error.
fn to_connector_error(failure: ImapFailure) -> ConnectorError {
    match failure {
        ImapFailure::Auth => ConnectorError::Checkpoint(
            "iCloud rejected the login; renew the app-specific password".into(),
        ),
        ImapFailure::Transient(m) => ConnectorError::Transient(m),
        ImapFailure::Protocol(m) => ConnectorError::Other(m),
    }
}

/// Short, secret-free label for the audit entry of a failed cycle.
fn failure_class(error: &ConnectorError) -> (&'static str, String) {
    match error {
        ConnectorError::Checkpoint(m) => ("auth_or_setup", m.clone()),
        ConnectorError::Challenge(m) => ("challenge", m.clone()),
        ConnectorError::Transient(m) => ("transient", m.clone()),
        ConnectorError::Other(m) => ("error", m.clone()),
        ConnectorError::Unsupported(m) => ("unsupported", m.clone()),
        ConnectorError::Ambiguous(m) => ("ambiguous", m.clone()),
        ConnectorError::Authorization(m) => ("authorization", m.clone()),
    }
}

/// Undo a MIME transfer encoding on a fetched part: base64 and quoted-printable; anything else
/// is already the file.
fn decode_transfer(raw: &[u8], encoding: &str) -> Result<Vec<u8>, ConnectorError> {
    use base64::Engine;
    match encoding.to_ascii_lowercase().as_str() {
        "base64" => {
            let clean: Vec<u8> = raw
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            base64::engine::general_purpose::STANDARD
                .decode(clean)
                .map_err(|_| ConnectorError::Other("attachment could not be decoded".into()))
        }
        "quoted-printable" => {
            let mut out = Vec::with_capacity(raw.len());
            let mut i = 0;
            while i < raw.len() {
                if raw[i] == b'=' {
                    if raw.get(i + 1) == Some(&b'\r') && raw.get(i + 2) == Some(&b'\n') {
                        i += 3;
                        continue;
                    }
                    if raw.get(i + 1) == Some(&b'\n') {
                        i += 2;
                        continue;
                    }
                    let hex = raw
                        .get(i + 1..i + 3)
                        .and_then(|h| std::str::from_utf8(h).ok());
                    if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                }
                out.push(raw[i]);
                i += 1;
            }
            Ok(out)
        }
        _ => Ok(raw.to_vec()),
    }
}

fn run_cycle(
    config: &IcloudConfig,
    secrets: &dyn SecretStore,
    opener: &dyn Opener,
    audit: &CycleAudit,
    cursor: Option<Cursor>,
) -> Result<Batch, ConnectorError> {
    let attempt = || -> Result<Batch, ConnectorError> {
        let password = secrets
            .get(&config.address)
            .map_err(ConnectorError::Other)?
            .ok_or_else(|| {
                ConnectorError::Checkpoint(
                    "no app-specific password stored; run the iCloud auth verb".into(),
                )
            })?;
        let cursor = cursor
            .map(|c| IcloudCursor::decode(&c.0))
            .unwrap_or_default();
        let mut session = opener.open(config, &password).map_err(to_connector_error)?;
        let result = sync_all(session.as_mut(), config, &cursor);
        session.logout();
        let outcome = result.map_err(to_connector_error)?;
        let detail = outcome
            .reports
            .iter()
            .map(|r| r.describe())
            .collect::<Vec<_>>()
            .join(";");
        audit("ok", &detail);
        Ok(outcome.batch)
    };
    let result = attempt();
    if let Err(e) = &result {
        // The reason is a fixed label (stage and error kind, or a Keychain status code).
        let (class, reason) = failure_class(e);
        audit(class, &reason);
    }
    result
}

impl Connector for IcloudConnector {
    fn source(&self) -> Source {
        Source::Icloud
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            draft: false,
            // Consulted only by the human send path: agents are denied `Action::Send` by the gate.
            send: true,
            // the human's archive moves mail to the Archive folder; agents have no path to it
            archive: true,
            ..Default::default()
        }
    }

    fn fetch(&self, cursor: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        Box::pin(async move {
            let config = self.config.clone();
            let secrets = Arc::clone(&self.secrets);
            let opener = Arc::clone(&self.opener);
            let audit = Arc::clone(&self.audit);
            tokio::task::spawn_blocking(move || {
                run_cycle(&config, secrets.as_ref(), opener.as_ref(), &audit, cursor)
            })
            .await
            .unwrap_or_else(|_| Err(ConnectorError::Transient("sync task failed".into())))
        })
    }

    fn own_address(&self) -> Option<String> {
        Some(self.config.address.clone())
    }

    /// Fetch one attachment for the human: find the message in the inbox by its `Message-ID`, read
    /// only that MIME part (`BODY.PEEK`, read-only `EXAMINE`) and decode its transfer encoding.
    fn fetch_attachment<'a>(
        &'a self,
        request: &'a AttachmentRequest,
    ) -> BoxFuture<'a, Result<Vec<u8>, ConnectorError>> {
        Box::pin(async move {
            let Some((section, encoding)) = request.reference.split_once('|') else {
                return Err(ConnectorError::Other(
                    "this attachment was stored before it could be fetched: run `haste connector resync icloud`".into(),
                ));
            };
            let section: Vec<u32> = section.split('.').filter_map(|n| n.parse().ok()).collect();
            if section.is_empty() {
                return Err(ConnectorError::Other(
                    "unusable attachment reference".into(),
                ));
            }
            let password = match self.secrets.get(&self.config.address) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return Err(ConnectorError::Checkpoint(
                        "no app-specific password stored; run the iCloud auth verb".into(),
                    ))
                }
                Err(_) => return Err(ConnectorError::Other("keychain unavailable".into())),
            };
            let (config, opener) = (self.config.clone(), Arc::clone(&self.opener));
            let message_id = request.message_external_id.clone();
            let encoding = encoding.to_owned();
            let max = haste_core::connector::MAX_FETCH_ATTACHMENT_BYTES;
            let octets = u32::try_from(if request.bytes == 0 {
                max as u64
            } else {
                request.bytes.min(max as u64)
            })
            .unwrap_or(u32::MAX);
            tokio::task::spawn_blocking(move || {
                let mut session = opener
                    .open(&config, &password)
                    .map_err(to_connector_error)?;
                let result = (|| {
                    let folder = crate::config::DEFAULT_FOLDER;
                    let info = session.examine(folder).map_err(to_connector_error)?;
                    let mut parts = message_id.rsplitn(3, ':');
                    let direct = match (parts.next(), parts.next(), parts.next()) {
                        (Some(u), Some(v), Some(n)) if n == folder => {
                            u.parse::<u32>().ok().zip(v.parse::<u32>().ok())
                        }
                        _ => None,
                    };
                    let uid = match direct {
                        Some((uid, validity)) if validity == info.uid_validity => Some(uid),
                        Some(_) => None,
                        None => session
                            .uid_search_message_id(message_id.trim_matches(['<', '>']))
                            .map_err(to_connector_error)?
                            .first()
                            .copied(),
                    };
                    let uid = uid.ok_or_else(|| {
                        ConnectorError::Other(
                            "the message is not in the inbox any more (archived or moved)".into(),
                        )
                    })?;
                    let part = crate::session::TextPart {
                        html: false,
                        section,
                        charset: "utf-8".into(),
                        encoding: encoding.clone(),
                        octets,
                    };
                    let raw = session
                        .fetch_text_part(uid, &part, u32::try_from(max).unwrap_or(u32::MAX))
                        .map_err(to_connector_error)?
                        .ok_or_else(|| ConnectorError::Other("the attachment is empty".into()))?;
                    decode_transfer(&raw, &encoding)
                })();
                session.logout();
                result
            })
            .await
            .unwrap_or_else(|_| Err(ConnectorError::Other("attachment task failed".into())))
        })
    }

    /// The human's archive: `UID MOVE` the thread's INBOX messages into the Archive folder, one
    /// session, one attempt. Messages of other folders and ids that do not parse are left alone.
    fn archive_thread<'a>(
        &'a self,
        target: &'a ArchiveTarget,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async move {
            let password = match self.secrets.get(&self.config.address) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return Err(ConnectorError::Checkpoint(
                        "no app-specific password stored; run the iCloud auth verb".into(),
                    ))
                }
                Err(_) => return Err(ConnectorError::Other("keychain unavailable".into())),
            };
            let (config, opener) = (self.config.clone(), Arc::clone(&self.opener));
            let ids = target.message_external_ids.clone();
            tokio::task::spawn_blocking(move || {
                let mut session = opener
                    .open(&config, &password)
                    .map_err(to_connector_error)?;
                let result = (|| {
                    let folder = crate::config::DEFAULT_FOLDER;
                    let info = session.select(folder).map_err(to_connector_error)?;
                    // a message is found by its Message-ID header; an id of the form
                    // `INBOX:<uidvalidity>:<uid>` (a message without one) names its UID directly
                    let mut uids: Vec<u32> = Vec::new();
                    for id in &ids {
                        let mut parts = id.rsplitn(3, ':');
                        let (uid, validity, name) = (parts.next(), parts.next(), parts.next());
                        let direct = match (uid, validity, name) {
                            (Some(u), Some(v), Some(n)) if n == folder => {
                                u.parse::<u32>().ok().zip(v.parse::<u32>().ok())
                            }
                            _ => None,
                        };
                        match direct {
                            Some((uid, validity)) if validity == info.uid_validity => {
                                uids.push(uid)
                            }
                            Some(_) => {}
                            None => uids.extend(
                                session
                                    .uid_search_message_id(id.trim_matches(['<', '>']))
                                    .map_err(to_connector_error)?,
                            ),
                        }
                    }
                    uids.sort_unstable();
                    uids.dedup();
                    if uids.is_empty() {
                        return Err(ConnectorError::Other(
                            "no inbox messages of this thread to archive".into(),
                        ));
                    }
                    session
                        .uid_move(&uids, &config.archive_folder)
                        .map_err(to_connector_error)
                })();
                session.logout();
                result
            })
            .await
            .unwrap_or_else(|_| Err(ConnectorError::Other("archive task failed".into())))
        })
    }

    /// A human send: compose, submit over SMTP exactly once, then file one copy in the Sent
    /// folder. Nothing is retried. A failed copy never undoes the send: it is reported in the
    /// receipt (`smtp;copy_failed`) and the message is not submitted again.
    fn send<'a>(
        &'a self,
        draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        Box::pin(async move {
            let refused = |why: &str| ConnectorError::Other(format!("send refused: {why}"));
            let to = draft
                .recipients
                .first()
                .ok_or_else(|| refused("no recipient"))?
                .clone();
            let subject = draft.subject.clone().ok_or_else(|| refused("no subject"))?;
            let password = match self.secrets.get(&self.config.address) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return Err(ConnectorError::Checkpoint(
                        "no app-specific password stored; run the iCloud auth verb".into(),
                    ))
                }
                Err(_) => return Err(refused("keychain unavailable")),
            };
            let domain = self
                .config
                .address
                .rsplit('@')
                .next()
                .unwrap_or("localhost");
            let message = haste_core::send::compose_plain_text(
                &self.config.address,
                &to,
                &subject,
                &draft.body,
                &haste_core::send::format_rfc2822(haste_core::time::now_secs()),
                &haste_core::send::new_message_id(domain),
                None,
                None,
                &draft.attachments,
            )
            .map_err(refused)?;
            // One text for both the submission and the Sent copy, ending in a line break (as
            // SMTP data always does), so the copy is byte-identical to what was sent.
            let message = if message.ends_with("\r\n") {
                message
            } else {
                message + "\r\n"
            };
            let (config, smtp, opener) = (
                self.config.clone(),
                Arc::clone(&self.smtp),
                Arc::clone(&self.opener),
            );
            tokio::task::spawn_blocking(move || {
                submit(
                    smtp.as_ref(),
                    &config,
                    &password,
                    &config.address,
                    &to,
                    &message,
                )
                .map_err(smtp_error)?;
                // Accepted. Whatever happens to the copy, the message is not submitted again.
                let mut receipt = "smtp".to_owned();
                if config.append_sent {
                    let filed = opener.open(&config, &password).and_then(|mut session| {
                        let result = session.append(&config.sent_folder, message.as_bytes());
                        session.logout();
                        result
                    });
                    if filed.is_err() {
                        receipt.push_str(";copy_failed");
                    }
                }
                Ok(SendReceipt {
                    external_id: receipt,
                })
            })
            .await
            // a panicked task may have submitted: the safe reading is "unknown"
            .unwrap_or_else(|_| Err(ConnectorError::Ambiguous("send task failed".into())))
        })
    }
}
