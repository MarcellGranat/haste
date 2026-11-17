//! The `Connector` implementation for Gmail: read and draft only.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use haste_core::connector::{
    ArchiveTarget, AttachmentRequest, Batch, BoxFuture, Capabilities, Connector, ConnectorError,
    Cursor, DraftReceipt, DraftToSend, SendReceipt,
};
use haste_core::model::Source;
use serde::{Deserialize, Serialize};

use crate::auth::{OAuthClient, TokenSource};
use crate::client::{ApiPart, GmailClient};
use crate::consts::{
    API_BASE, BACKFILL_DAYS, BACKFILL_MAX_THREADS, HTTP_TIMEOUT, THREAD_GET_PACING, THREAD_RETRIES,
    THREAD_RETRY_BASE, TOKEN_URL,
};
use crate::keychain::TokenStore;
use crate::mapper::{addresses, map_thread};
use crate::mime::{build_plain_text, build_send_message, encode_raw, reply_subject};

/// Everything needed to run the connector except the refresh token (which is in the Keychain).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GmailConfig {
    pub client_id: String,
    pub client_secret: String,
    /// The Gmail address; also the Keychain account name.
    pub account: String,
}

impl GmailConfig {
    #[must_use]
    pub fn path(base: &Path) -> PathBuf {
        base.join("gmail.json")
    }

    /// # Errors
    /// I/O or JSON errors as text.
    pub fn load(base: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(Self::path(base))
            .map_err(|e| format!("cannot read gmail.json: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid gmail.json: {e}"))
    }

    /// Write with mode `0600`.
    ///
    /// # Errors
    /// I/O errors as text.
    pub fn save(&self, base: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(Self::path(base))
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())
    }
}

/// How many threads in a row may be refused before the rest of a cycle is left for later.
const MAX_SKIPPED_IN_A_ROW: u32 = 3;
/// How many unread-able threads the cursor remembers.
const MAX_PENDING_THREADS: usize = 100;

/// The cursor is the history id, then (after a `;`) the ids of the threads that could not be
/// read and are asked for again, separated by commas.
fn split_cursor(cursor: &str) -> (String, Vec<String>) {
    match cursor.split_once(';') {
        Some((history, rest)) => (
            history.to_owned(),
            rest.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        None => (cursor.to_owned(), Vec::new()),
    }
}

fn join_cursor(history: &str, pending: &[String]) -> String {
    if pending.is_empty() {
        history.to_owned()
    } else {
        let kept: Vec<&str> = pending
            .iter()
            .take(MAX_PENDING_THREADS)
            .map(String::as_str)
            .collect();
        format!("{history};{}", kept.join(","))
    }
}

pub struct GmailConnector {
    client: GmailClient,
    /// The connector's own address: the From of a send, and never a reply target.
    account: String,
}

impl GmailConnector {
    /// Production endpoints.
    #[must_use]
    pub fn from_config(config: &GmailConfig, store: Arc<dyn TokenStore>) -> Self {
        Self::new(config, API_BASE, TOKEN_URL, store)
    }

    /// Custom endpoints (tests point these at a local fake server).
    #[must_use]
    pub fn new(
        config: &GmailConfig,
        api_base: &str,
        token_url: &str,
        store: Arc<dyn TokenStore>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("http client");
        let tokens = TokenSource::new(
            http.clone(),
            token_url,
            OAuthClient {
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
            },
            &config.account,
            store,
        );
        Self {
            client: GmailClient::new(http, api_base, Arc::new(tokens)),
            account: config.account.clone(),
        }
    }

    async fn backfill(&self) -> Result<Batch, ConnectorError> {
        // Read the history id first so changes during the backfill are picked up next cycle.
        let history_id = self.client.profile().await?.history_id;
        let mut ids: Vec<String> = Vec::new();
        let mut page: Option<String> = None;
        'pages: loop {
            let list = self
                .client
                .list_threads(BACKFILL_DAYS, page.as_deref())
                .await?;
            for t in list.threads {
                if ids.len() >= BACKFILL_MAX_THREADS {
                    break 'pages;
                }
                ids.push(t.id);
            }
            match list.next_page_token {
                Some(next) => page = Some(next),
                None => break,
            }
        }
        let (mut batch, complete) = self
            .fetch_threads(ids.iter().map(String::as_str), true)
            .await?;
        // Only a full backfill moves to incremental sync. A partial one keeps what it read (the
        // newest threads) and the next cycle tries again.
        batch.next_cursor = complete.then_some(Cursor(history_id));
        Ok(batch)
    }

    /// Read one thread, waiting and retrying when Gmail says to slow down.
    async fn get_thread_patiently(
        &self,
        id: &str,
    ) -> Result<Option<crate::client::ApiThread>, ConnectorError> {
        let mut wait = THREAD_RETRY_BASE;
        for _ in 0..THREAD_RETRIES {
            match self.client.get_thread(id).await {
                Err(ConnectorError::Transient(_)) => {
                    tokio::time::sleep(wait).await;
                    wait *= 2;
                }
                other => return other,
            }
        }
        self.client.get_thread(id).await
    }

    async fn fetch_threads<'a>(
        &self,
        ids: impl Iterator<Item = &'a str>,
        partial_ok: bool,
    ) -> Result<(Batch, bool), ConnectorError> {
        let mut batch = Batch::default();
        for id in ids {
            tokio::time::sleep(THREAD_GET_PACING).await;
            // A thread deleted since it was listed is simply skipped.
            let read = match self.get_thread_patiently(id).await {
                // Rate limited for good: keep what was read (a backfill only), stop here.
                Err(ConnectorError::Transient(_)) if partial_ok && !batch.threads.is_empty() => {
                    return Ok((batch, false));
                }
                other => other?,
            };
            // Only the default inbox is loaded; other folders and tabs are skipped.
            if let Some(api) = read.filter(crate::mapper::is_primary_inbox) {
                let mapped = map_thread(&api);
                batch
                    .unread
                    .push((mapped.thread.external_id.clone(), mapped.labels.unread));
                batch.threads.push(mapped.thread);
                batch.messages.extend(mapped.messages);
            }
        }
        Ok((batch, true))
    }

    /// Like [`Self::fetch_threads`], but a thread Gmail keeps refusing does not stop the others:
    /// it is left out and returned for a later cycle. After [`MAX_SKIPPED_IN_A_ROW`] refusals in
    /// a row the rest is left for later too, so a real throttle is not hammered.
    async fn fetch_threads_skipping<'a>(
        &self,
        ids: impl Iterator<Item = &'a str>,
    ) -> Result<(Batch, Vec<String>), ConnectorError> {
        let mut batch = Batch::default();
        let mut later: Vec<String> = Vec::new();
        let mut in_a_row = 0;
        for id in ids {
            if in_a_row >= MAX_SKIPPED_IN_A_ROW {
                later.push(id.to_owned());
                continue;
            }
            tokio::time::sleep(THREAD_GET_PACING).await;
            let read = match self.get_thread_patiently(id).await {
                Err(ConnectorError::Transient(_)) => {
                    in_a_row += 1;
                    later.push(id.to_owned());
                    continue;
                }
                other => other?,
            };
            in_a_row = 0;
            if let Some(api) = read.filter(crate::mapper::is_primary_inbox) {
                let mapped = map_thread(&api);
                batch
                    .unread
                    .push((mapped.thread.external_id.clone(), mapped.labels.unread));
                batch.threads.push(mapped.thread);
                batch.messages.extend(mapped.messages);
            }
        }
        Ok((batch, later))
    }

    async fn delta(&self, cursor: Cursor) -> Result<Batch, ConnectorError> {
        let (start, pending) = split_cursor(&cursor.0);
        // threads that could not be read last time are asked for again
        let mut changed: BTreeSet<String> = pending.into_iter().collect();
        let mut latest: Option<String> = None;
        let mut page: Option<String> = None;
        loop {
            let Some(list) = self.client.list_history(&start, page.as_deref()).await? else {
                // History expired: one bounded backfill resets the cursor. No loop.
                return self.backfill().await;
            };
            for record in list.history {
                changed.extend(
                    record
                        .messages
                        .into_iter()
                        .map(|m| m.thread_id)
                        .filter(|t| !t.is_empty()),
                );
            }
            if list.history_id.is_some() {
                latest = list.history_id;
            }
            match list.next_page_token {
                Some(next) => page = Some(next),
                None => break,
            }
        }
        if changed.is_empty() {
            return Ok(Batch {
                next_cursor: Some(cursor),
                ..Batch::default()
            });
        }
        let (mut batch, later) = self
            .fetch_threads_skipping(changed.iter().map(String::as_str))
            .await?;
        // the history moves on; the threads left out ride along in the cursor to be read again
        batch.next_cursor = Some(Cursor(join_cursor(&latest.unwrap_or(start), &later)));
        Ok(batch)
    }

    /// A human send: the recipient, subject and body the human confirmed, exactly. One
    /// `messages.send` request, never retried; the approval-time draft is removed afterwards
    /// (best effort, once).
    async fn attachment(&self, request: &AttachmentRequest) -> Result<Vec<u8>, ConnectorError> {
        if request.reference.is_empty() {
            return Err(ConnectorError::Other(
                "this attachment was stored before it could be fetched: run `haste connector resync gmail`".into(),
            ));
        }
        let max = haste_core::connector::MAX_FETCH_ATTACHMENT_BYTES;
        let first = self
            .client
            .get_attachment(&request.message_external_id, &request.reference, max)
            .await;
        // Gmail attachment ids are not stable: one stored earlier can be refused (400, or 404).
        // Read the message again, find the same file by name, and try once with its fresh id.
        let stale =
            matches!(&first, Err(ConnectorError::Other(m)) if m == "http 400" || m == "http 404");
        if !stale || request.name.is_empty() || request.thread_external_id.is_empty() {
            return first;
        }
        let Some(thread) = self.client.get_thread(&request.thread_external_id).await? else {
            return first;
        };
        let Some(payload) = thread
            .messages
            .iter()
            .find(|m| m.id == request.message_external_id)
            .and_then(|m| m.payload.as_ref())
        else {
            return first;
        };
        let mut found = Vec::new();
        fresh_references(payload, &request.name, &mut found);
        let fresh = found
            .iter()
            .find(|(bytes, _)| *bytes == request.bytes)
            .or_else(|| found.first())
            .map(|(_, id)| id.clone());
        match fresh {
            Some(id) if id != request.reference => {
                self.client
                    .get_attachment(&request.message_external_id, &id, max)
                    .await
            }
            _ => first,
        }
    }

    async fn set_read(
        &self,
        target: &haste_core::connector::ReadTarget,
    ) -> Result<(), ConnectorError> {
        self.client
            .set_thread_unread(&target.thread_external_id, !target.read)
            .await
    }

    async fn archive(&self, target: &ArchiveTarget) -> Result<(), ConnectorError> {
        self.client.archive_thread(&target.thread_external_id).await
    }

    async fn send_message(&self, draft: &DraftToSend) -> Result<SendReceipt, ConnectorError> {
        let refused = |why: &str| ConnectorError::Other(format!("send refused: {why}"));
        let to = draft
            .recipients
            .first()
            .ok_or_else(|| refused("no recipient"))?;
        let subject = draft
            .subject
            .as_deref()
            .ok_or_else(|| refused("no subject"))?;
        // Threading headers come from the thread; this lookup happens before anything is sent.
        let (mut in_reply_to, mut references) = (None, None);
        if let Some(id) = &draft.thread_id {
            if let Some(thread) = self.client.get_thread_headers(id).await? {
                let header = |part: &ApiPart, name: &str| {
                    part.headers
                        .iter()
                        .find(|h| h.name.eq_ignore_ascii_case(name))
                        .map(|h| h.value.clone())
                };
                if let Some(last) = thread
                    .messages
                    .iter()
                    .filter_map(|m| m.payload.as_ref())
                    .next_back()
                {
                    in_reply_to = header(last, "Message-ID");
                    references = match (header(last, "References"), &in_reply_to) {
                        (Some(r), Some(id)) => Some(format!("{r} {id}")),
                        (None, Some(id)) => Some(id.clone()),
                        (r, None) => r,
                    };
                }
            }
        }
        let domain = self.account.rsplit('@').next().unwrap_or("localhost");
        let mime = build_send_message(
            &self.account,
            to,
            subject,
            &draft.body,
            &haste_core::send::format_rfc2822(haste_core::time::now_secs()),
            &haste_core::send::new_message_id(domain),
            in_reply_to.as_deref(),
            references.as_deref(),
            &draft.attachments,
        )
        .map_err(refused)?;
        let id = self
            .client
            .send_message(&encode_raw(&mime), draft.thread_id.as_deref())
            .await?;
        if let Some(draft_id) = &draft.approval_draft_id {
            let _ = self.client.delete_draft(draft_id).await;
        }
        Ok(SendReceipt { external_id: id })
    }

    async fn draft(&self, draft: &DraftToSend) -> Result<DraftReceipt, ConnectorError> {
        let thread = match &draft.thread_id {
            Some(id) => self.client.get_thread_headers(id).await?,
            None => None,
        };
        let header = |part: &ApiPart, name: &str| {
            part.headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case(name))
                .map(|h| h.value.clone())
        };
        let mut subject = "(no subject)".to_owned();
        let (mut in_reply_to, mut references, mut fallback_to) = (None, None, None);
        if let Some(thread) = &thread {
            let parts: Vec<ApiPart> = thread
                .messages
                .iter()
                .filter_map(|m| m.payload.clone())
                .collect();
            if let Some(s) = parts.iter().find_map(|p| header(p, "Subject")) {
                subject = reply_subject(&s);
            }
            if let Some(last) = parts.last() {
                in_reply_to = header(last, "Message-ID");
                references = match (header(last, "References"), &in_reply_to) {
                    (Some(r), Some(id)) => Some(format!("{r} {id}")),
                    (None, Some(id)) => Some(id.clone()),
                    (r, None) => r,
                };
                fallback_to = header(last, "Reply-To")
                    .or_else(|| header(last, "From"))
                    .and_then(|h| addresses(&h).into_iter().next());
            }
        }
        let to = draft
            .to
            .clone()
            .or(fallback_to)
            .ok_or_else(|| ConnectorError::Other("draft has no recipient".into()))?;
        let mime = build_plain_text(
            &to,
            &subject,
            &draft.body,
            in_reply_to.as_deref(),
            references.as_deref(),
        );
        let created = self
            .client
            .create_draft(&encode_raw(&mime), draft.thread_id.as_deref())
            .await?;
        Ok(DraftReceipt {
            draft_id: created.id,
        })
    }
}

impl Connector for GmailConnector {
    fn source(&self) -> Source {
        Source::Gmail
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            draft: true,
            // Consulted only by the human send path: agents are denied `Action::Send` by the gate.
            send: true,
            archive: true,
            ..Default::default()
        }
    }

    fn own_address(&self) -> Option<String> {
        Some(self.account.clone())
    }

    fn fetch(&self, cursor: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        Box::pin(async move {
            match cursor {
                None => self.backfill().await,
                Some(c) => self.delta(c).await,
            }
        })
    }

    fn fetch_attachment<'a>(
        &'a self,
        request: &'a AttachmentRequest,
    ) -> BoxFuture<'a, Result<Vec<u8>, ConnectorError>> {
        Box::pin(self.attachment(request))
    }

    fn set_thread_read<'a>(
        &'a self,
        target: &'a haste_core::connector::ReadTarget,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(self.set_read(target))
    }

    fn archive_thread<'a>(
        &'a self,
        target: &'a ArchiveTarget,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(self.archive(target))
    }

    fn send<'a>(
        &'a self,
        draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        Box::pin(self.send_message(draft))
    }

    fn create_draft<'a>(
        &'a self,
        draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<DraftReceipt, ConnectorError>> {
        Box::pin(self.draft(draft))
    }
}

/// `(size, attachment id)` of every part named `name`, in message order.
fn fresh_references(part: &ApiPart, name: &str, out: &mut Vec<(u64, String)>) {
    if part.filename == name {
        if let Some(id) = &part.body.attachment_id {
            out.push((part.body.size, id.clone()));
        }
    }
    for p in &part.parts {
        fresh_references(p, name, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ApiBody, ApiPart};

    fn file(name: &str, size: u64, id: &str) -> ApiPart {
        ApiPart {
            filename: name.into(),
            body: ApiBody {
                size,
                attachment_id: Some(id.into()),
                ..ApiBody::default()
            },
            ..ApiPart::default()
        }
    }

    #[test]
    fn a_stale_reference_is_found_again_by_file_name() {
        let root = ApiPart {
            parts: vec![
                file("a.pdf", 10, "NEW1"),
                ApiPart {
                    parts: vec![file("b.pdf", 20, "NEW2"), file("a.pdf", 30, "NEW3")],
                    ..ApiPart::default()
                },
            ],
            ..ApiPart::default()
        };
        let mut found = Vec::new();
        fresh_references(&root, "a.pdf", &mut found);
        assert_eq!(
            found,
            vec![(10, "NEW1".to_owned()), (30, "NEW3".to_owned())]
        );
    }
}
