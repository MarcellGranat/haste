//! The iCloud `Connector`: read-only, polled, one blocking IMAP session per cycle.

use std::sync::Arc;

use haste_core::connector::{
    Batch, BoxFuture, Capabilities, Connector, ConnectorError, Cursor, DraftToSend, SendReceipt,
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
            archive: false,
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
