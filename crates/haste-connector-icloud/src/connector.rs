//! The iCloud `Connector`: read-only, polled, one blocking IMAP session per cycle.

use std::sync::Arc;

use haste_core::connector::{
    Batch, BoxFuture, Capabilities, Connector, ConnectorError, Cursor, DraftToSend, SendReceipt,
};
use haste_core::model::Source;

use crate::config::IcloudConfig;
use crate::secrets::SecretStore;
use crate::session::{ImapFailure, Opener};
use crate::sync::{sync_all, IcloudCursor};

/// Receives `(outcome, detail)` once per cycle. The daemon turns it into an audit entry.
/// Details are counts and folder names only.
pub type CycleAudit = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct IcloudConnector {
    config: IcloudConfig,
    secrets: Arc<dyn SecretStore>,
    opener: Arc<dyn Opener>,
    audit: CycleAudit,
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
        }
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
            send: false,
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

    fn send<'a>(
        &'a self,
        _draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError::Other(
                "sending is not supported by the iCloud connector".into(),
            ))
        })
    }
}
