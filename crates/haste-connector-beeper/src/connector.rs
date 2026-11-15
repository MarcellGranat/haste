//! The Beeper `Connector`: one registration per network (`whatsapp`, `messenger`, `imessage`),
//! each with its own kill switch and cursor. Reads only selected chats of on-device accounts;
//! sends only for the human, one request, never retried.

use std::collections::BTreeMap;
use std::sync::Arc;

use haste_core::connector::{
    AttachmentRequest, Batch, BoxFuture, Capabilities, Connector, ConnectorError, Cursor,
    DraftToSend, HistoryRequest, ReactionRequest, SendReceipt,
};
use haste_core::model::{Message, Source, Thread};
use serde::{Deserialize, Serialize};

use crate::client::BeeperClient;
use crate::config::BeeperConfig;
use crate::consts::{
    BEEPER_BACKFILL_PER_CHAT, BEEPER_HISTORY_EXTRA, BEEPER_HISTORY_MAX_PAGES,
    BEEPER_MAX_CHATS_PER_CYCLE, BEEPER_MAX_PAGES_PER_CHAT, BEEPER_MAX_SEND_BYTES, BEEPER_PAGE_SIZE,
    DAY_SECS,
};
pub use crate::mapper::BeeperSource;
use crate::mapper::{map_message, Mapped};
use crate::wire::{key_newer, WireAccount};

/// Receives `(outcome, detail)` once per cycle. Details are counts and fixed labels only: never a
/// chat name, message text, handle or account id.
pub type CycleAudit = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// Unix seconds; tests substitute a fixed clock.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Where each chat's read continues: chat id to the newest sort key stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeeperCursor {
    /// Layout of what was stored. An older cursor makes the next cycle read each chat's recent
    /// history again, which refreshes fields added since (sender names, who wrote what).
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub chats: BTreeMap<String, String>,
}

/// Bump when the stored message fields change and old chats should be read again.
pub const CURSOR_VERSION: u32 = 5;

impl BeeperCursor {
    #[must_use]
    pub fn decode(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_default()
    }

    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

pub struct BeeperConnector {
    which: BeeperSource,
    client: Arc<BeeperClient>,
    config: BeeperConfig,
    audit: CycleAudit,
    clock: Clock,
}

impl BeeperConnector {
    #[must_use]
    pub fn new(
        which: BeeperSource,
        client: Arc<BeeperClient>,
        config: &BeeperConfig,
        audit: CycleAudit,
    ) -> Self {
        Self {
            which,
            client,
            config: config.clone(),
            audit,
            clock: Arc::new(haste_core::time::now_secs),
        }
    }

    /// Replace the clock (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// An account may be used only if its connection is on-device, or if the human has allowed
    /// cloud for that account in `beeper.json` (`allow_cloud`). A cloud connection is refused
    /// unless allowed, and attesting on-device never overrides what Beeper reports; when the API
    /// does not say, the human's attestation stands in, and without it the account is refused
    /// too. Never a silent skip.
    fn check_on_device(&self, account: &WireAccount) -> Result<&'static str, ConnectorError> {
        match account.provider().map(|c| c.to_lowercase()) {
            Some(c) if c.contains("cloud") => {
                if self
                    .config
                    .account(&account.id)
                    .is_some_and(|a| a.allow_cloud)
                {
                    Ok("cloud_allowed")
                } else {
                    Err(ConnectorError::Other(
                        "refused: cloud connection; set allow_cloud for this account in beeper.json to use it"
                            .into(),
                    ))
                }
            }
            Some(c) if c.contains("device") || c == "local" => Ok("on_device"),
            // `platform-sdk` and `self-hosted` do not say the account is on this device
            _ if self
                .config
                .account(&account.id)
                .is_some_and(|a| a.on_device_attested) =>
            {
                Ok("attested")
            }
            _ => Err(ConnectorError::Other(
                "refused: connection type unknown; attest on-device in beeper.json".into(),
            )),
        }
    }

    /// The selected accounts of this source, each checked on-device, with how it was established.
    async fn selected_accounts(&self) -> Result<Vec<(WireAccount, &'static str)>, ConnectorError> {
        let mut out = Vec::new();
        for account in self.client.accounts().await? {
            if self.config.account(&account.id).is_none() {
                continue; // not selected: never read
            }
            let network = BeeperSource::from_network(&account.network).or_else(|| {
                account
                    .bridge_type()
                    .and_then(|t| BeeperSource::from_network(&t))
            });
            match network {
                Some(s) if s == self.which => {}
                Some(_) => continue, // another registration handles it
                None => {
                    return Err(ConnectorError::Other("refused: unsupported network".into()));
                }
            }
            let how = match self.check_on_device(&account) {
                Ok(h) => h,
                Err(e) => {
                    haste_core::log::warn(
                        "beeper",
                        "account_refused",
                        &[
                            haste_core::log::id("network", self.which.name()),
                            haste_core::log::id(
                                "provider",
                                &account.provider().unwrap_or_else(|| "unreported".into()),
                            ),
                        ],
                    );
                    return Err(e);
                }
            };
            haste_core::log::debug(
                "beeper",
                "account_accepted",
                &[
                    haste_core::log::id("network", self.which.name()),
                    haste_core::log::id("how", how),
                ],
            );
            out.push((account, how));
        }
        Ok(out)
    }

    /// Read one chat's new messages: pages are bounded, nothing already stored is returned again,
    /// and the first read is limited to the history window and the backfill cap.
    async fn read_chat(
        &self,
        chat_id: &str,
        after: Option<&str>,
        now: i64,
    ) -> Result<(Vec<Mapped>, usize), ConnectorError> {
        let window_start = now - i64::from(self.config.history_days) * DAY_SECS;
        let mut found: Vec<Mapped> = Vec::new();
        let mut skipped = 0;
        let mut cursor: Option<String> = None;
        let timer = haste_core::log::Timer::start();
        let mut pages = 0_usize;
        let mut seen = 0_usize;
        for _ in 0..BEEPER_MAX_PAGES_PER_CHAT {
            pages += 1;
            let (items, more, next) = self
                .client
                .messages(chat_id, BEEPER_PAGE_SIZE, cursor.as_deref())
                .await?;
            let mut any_new = false;
            seen += items.len();
            for wire in &items {
                let Some(mapped) = map_message(chat_id, &self.which.source(), wire) else {
                    skipped += 1;
                    continue;
                };
                let is_new = match after {
                    Some(k) => key_newer(&mapped.sort_key, k),
                    None => mapped.timestamp >= window_start,
                };
                if is_new {
                    any_new = true;
                    found.push(mapped);
                }
            }
            match (more, next) {
                (true, Some(n)) if any_new => cursor = Some(n),
                _ => break,
            }
        }
        if after.is_none() && found.len() > BEEPER_BACKFILL_PER_CHAT {
            found.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
            found.truncate(BEEPER_BACKFILL_PER_CHAT);
        }
        // why a chat read came back empty is the usual question: how many the API returned, how
        // many were new, how many skipped as not conversation text
        haste_core::log::info(
            "beeper",
            "chat_read",
            &[
                haste_core::log::id("network", self.which.name()),
                haste_core::log::l("first_read", if after.is_none() { "yes" } else { "no" }),
                haste_core::log::n("pages", pages),
                haste_core::log::n("returned", seen),
                haste_core::log::n("new", found.len()),
                haste_core::log::n("skipped", skipped),
                haste_core::log::n("ms", timer.ms()),
            ],
        );
        Ok((found, skipped))
    }

    /// Messages of one chat older than the stored ones: pages are walked from the newest, the
    /// ones already stored are passed over, and up to [`BEEPER_HISTORY_EXTRA`] older ones are kept.
    async fn read_history(&self, request: &HistoryRequest) -> Result<Batch, ConnectorError> {
        let chat_id = request.thread_external_id.as_str();
        let mut found: Vec<Mapped> = Vec::new();
        let mut older = 0_usize;
        let mut cursor: Option<String> = None;
        for _ in 0..BEEPER_HISTORY_MAX_PAGES {
            let (items, more, next) = self
                .client
                .messages(chat_id, BEEPER_PAGE_SIZE, cursor.as_deref())
                .await?;
            for wire in &items {
                if let Some(mapped) = map_message(chat_id, &self.which.source(), wire) {
                    // messages already stored are returned too: they come back with their
                    // current reactions, and storing them again changes nothing else
                    if request.oldest_known_at == 0 || mapped.timestamp <= request.oldest_known_at {
                        older += 1;
                    }
                    found.push(mapped);
                }
            }
            if older >= BEEPER_HISTORY_EXTRA {
                break;
            }
            match (more, next) {
                (true, Some(n)) => cursor = Some(n),
                _ => break,
            }
        }
        let mut batch = Batch::default();
        if found.is_empty() {
            return Ok(batch);
        }
        let mut participants: Vec<String> = Vec::new();
        for m in &found {
            if let Some(s) = &m.message.sender_identifier {
                if !participants.contains(s) {
                    participants.push(s.clone());
                }
            }
        }
        batch.threads.push(Thread {
            id: String::new(),
            source: self.which.source(),
            external_id: chat_id.to_owned(),
            subject: String::new(), // the stored title stays
            participants,
            last_activity_at: request.last_activity_at,
            version: 0,
        });
        batch.messages = found.into_iter().map(|m| m.message).collect();
        Ok(batch)
    }

    async fn run_cycle(&self, cursor: Option<Cursor>) -> Result<Batch, ConnectorError> {
        let state = cursor
            .map(|c| BeeperCursor::decode(&c.0))
            .unwrap_or_default();
        let now = (self.clock)();
        let result = self.cycle(state, now).await;
        if let Err(e) = &result {
            (self.audit)(
                match e {
                    ConnectorError::Authorization(_) => "permission_needed",
                    ConnectorError::Transient(_) => "transient",
                    ConnectorError::Checkpoint(_) => "auth_or_setup",
                    _ => "refused_or_error",
                },
                // a fixed label, never the error text (it could echo nothing private, but the
                // rule is the same everywhere: counts and labels only)
                match e {
                    ConnectorError::Authorization(_) => "beeper_unavailable_or_token",
                    ConnectorError::Other(m) if m.contains("cloud") => "cloud_connection",
                    ConnectorError::Other(m) if m.contains("unknown") => "connection_type_unknown",
                    ConnectorError::Other(m) if m.contains("unsupported") => "unsupported_network",
                    _ => "error",
                },
            );
        }
        result
    }

    async fn cycle(&self, state: BeeperCursor, now: i64) -> Result<Batch, ConnectorError> {
        let accounts = self.selected_accounts().await?;
        let state = if state.v < CURSOR_VERSION {
            BeeperCursor::default()
        } else {
            state
        };
        let mut next = BeeperCursor {
            v: CURSOR_VERSION,
            ..BeeperCursor::default()
        };
        let mut batch = Batch::default();
        let (mut chats_read, mut skipped_chats, mut skipped_msgs) = (0_usize, 0_usize, 0_usize);
        if !accounts.is_empty() {
            // Chats named in beeper.json are requested by id: the chat list is paginated
            // newest-first, so a listing could miss a selected chat that is quiet. With
            // `allow_all` the newest chats of the selected accounts are listed as well.
            let mut selected: Vec<crate::wire::WireChat> = Vec::new();
            let mut listed = 0_usize;
            if self.config.chats.allow_all {
                for c in self.client.chats().await? {
                    if !self.config.selects_chat(&c.id)
                        || !accounts.iter().any(|(a, _)| a.id == c.account_id)
                    {
                        continue;
                    }
                    if selected.len() >= BEEPER_MAX_CHATS_PER_CYCLE {
                        listed += 1;
                    } else {
                        selected.push(c);
                    }
                }
                skipped_chats += listed;
            }
            let mut named: Vec<&String> = Vec::new();
            for id in &self.config.chats.allow {
                if self.config.selects_chat(id)
                    && !named.contains(&id)
                    && !selected.iter().any(|c| &c.id == id)
                {
                    named.push(id);
                }
            }
            let room = BEEPER_MAX_CHATS_PER_CYCLE.saturating_sub(selected.len());
            skipped_chats += named.len().saturating_sub(room);
            for id in named.into_iter().take(room) {
                match self.client.chat_if_exists(id).await? {
                    Some(c) if accounts.iter().any(|(a, _)| a.id == c.account_id) => {
                        selected.push(c);
                    }
                    Some(_) => {} // another network's chat or an unselected account
                    None => skipped_chats += 1,
                }
            }
            for chat in selected {
                let after = state.chats.get(&chat.id).cloned();
                let (mapped, skipped) = self.read_chat(&chat.id, after.as_deref(), now).await?;
                skipped_msgs += skipped;
                chats_read += 1;
                // keep the cursor where it was unless something newer arrived
                let newest = mapped.iter().map(|m| m.sort_key.clone()).reduce(|a, b| {
                    if key_newer(&b, &a) {
                        b
                    } else {
                        a
                    }
                });
                if let Some(k) = newest.clone().or(after) {
                    next.chats.insert(chat.id.clone(), k);
                }
                if mapped.is_empty() {
                    continue;
                }
                let mut participants: Vec<String> = Vec::new();
                for m in &mapped {
                    if let Some(s) = &m.message.sender_identifier {
                        if !participants.contains(s) {
                            participants.push(s.clone());
                        }
                    }
                }
                batch.threads.push(Thread {
                    id: String::new(),
                    source: self.which.source(),
                    external_id: chat.id.clone(),
                    subject: chat.title.clone(),
                    participants,
                    last_activity_at: mapped.iter().map(|m| m.timestamp).max().unwrap_or(0),
                    version: 0,
                });
                batch.messages.extend(
                    mapped
                        .into_iter()
                        .map(|m| m.message)
                        .collect::<Vec<Message>>(),
                );
            }
        }
        (self.audit)(
            "ok",
            &format!(
                "accounts={} chats={chats_read} messages={} skipped_chats={skipped_chats} skipped_messages={skipped_msgs}",
                accounts.len(),
                batch.messages.len()
            ),
        );
        batch.next_cursor = Some(Cursor(next.encode()));
        Ok(batch)
    }

    /// A human send into an existing, selected chat of an on-device account of this source. The
    /// checks run before any message request; then exactly one `POST`, never retried.
    async fn send_text(&self, draft: &DraftToSend) -> Result<SendReceipt, ConnectorError> {
        let network = self.which.name();
        let refused = move |why: &'static str| {
            haste_core::log::warn(
                "beeper",
                "send_refused",
                &[
                    haste_core::log::id("network", network),
                    haste_core::log::l("reason", why),
                ],
            );
            ConnectorError::Other(format!("send refused: {why}"))
        };
        let chat_id = draft
            .thread_id
            .as_deref()
            .ok_or_else(|| refused("no chat"))?;
        if (draft.body.trim().is_empty() && draft.attachments.is_empty())
            || draft.body.len() > BEEPER_MAX_SEND_BYTES
        {
            return Err(refused("body_length"));
        }
        if draft.attachments.len() > 1 {
            return Err(refused("one attachment per chat message"));
        }
        if !self.config.selects_chat(chat_id) {
            return Err(refused("chat not selected"));
        }
        let chat = self.client.chat(chat_id).await?;
        let accounts = self.selected_accounts().await?;
        if !accounts.iter().any(|(a, _)| a.id == chat.account_id) {
            return Err(refused("account not selected or on another network"));
        }
        haste_core::log::info(
            "beeper",
            "send_start",
            &[
                haste_core::log::id("network", network),
                haste_core::log::n("body_bytes", draft.body.len()),
            ],
        );
        // the file goes to the local Beeper service first: that delivers nothing, so a failure
        // here is a plain refusal and nothing was sent
        let upload = match draft.attachments.first() {
            Some(a) => Some((
                self.client.upload_asset(&a.name, &a.mime, &a.data).await?,
                a.name.as_str(),
            )),
            None => None,
        };
        let attachment = upload.as_ref().map(|(id, name)| (id.as_str(), *name));
        let id = match self
            .client
            .send_message_with(chat_id, draft.body.trim_end(), attachment)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                // a fixed cause label, never the error text: why the outcome is unknown or refused
                let label = match &e {
                    ConnectorError::Ambiguous(m) if m.starts_with("http ") => "http_5xx",
                    ConnectorError::Ambiguous(m) if m.contains("no message id") => "no_message_id",
                    ConnectorError::Ambiguous(_) => "transport_or_timeout",
                    ConnectorError::Authorization(_) => "token_or_not_running",
                    ConnectorError::Checkpoint(_) => "rate_limited",
                    _ => "refused",
                };
                (self.audit)("send_failed", label);
                return Err(e);
            }
        };
        Ok(SendReceipt {
            external_id: format!("beeper:{id}"),
        })
    }
}

impl Connector for BeeperConnector {
    fn source(&self) -> Source {
        self.which.source()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            // no provider drafts at Beeper; `send` is consulted only by the human path (the gate
            // denies Action::Send to agents)
            draft: false,
            send: true,
            archive: false,
            ..Default::default()
        }
    }

    fn default_engaged(&self) -> bool {
        true
    }

    fn fetch(&self, cursor: Option<Cursor>) -> BoxFuture<'_, Result<Batch, ConnectorError>> {
        Box::pin(self.run_cycle(cursor))
    }

    fn send<'a>(
        &'a self,
        draft: &'a DraftToSend,
    ) -> BoxFuture<'a, Result<SendReceipt, ConnectorError>> {
        Box::pin(self.send_text(draft))
    }

    fn add_reaction<'a>(
        &'a self,
        request: &'a ReactionRequest,
    ) -> BoxFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async move {
            let chat_id = request.thread_external_id.as_str();
            if !haste_core::reactions::valid_key(&request.key) {
                return Err(ConnectorError::Other("reaction refused: key".into()));
            }
            // same gates as a send: a selected chat of an on-device account of this network
            if !self.config.selects_chat(chat_id) {
                return Err(ConnectorError::Other(
                    "reaction refused: chat not selected".into(),
                ));
            }
            let chat = self.client.chat(chat_id).await?;
            let accounts = self.selected_accounts().await?;
            if !accounts.iter().any(|(a, _)| a.id == chat.account_id) {
                return Err(ConnectorError::Other(
                    "reaction refused: account not selected or on another network".into(),
                ));
            }
            self.client
                .add_reaction(chat_id, &request.message_external_id, request.key.trim())
                .await
        })
    }

    fn fetch_thread_history<'a>(
        &'a self,
        request: &'a HistoryRequest,
    ) -> BoxFuture<'a, Result<Batch, ConnectorError>> {
        Box::pin(self.read_history(request))
    }

    fn fetch_attachment<'a>(
        &'a self,
        request: &'a AttachmentRequest,
    ) -> BoxFuture<'a, Result<Vec<u8>, ConnectorError>> {
        Box::pin(async move {
            if request.reference.is_empty() {
                return Err(ConnectorError::Other(
                    "this attachment was stored before it could be fetched".into(),
                ));
            }
            self.client
                .download_asset(
                    &request.reference,
                    haste_core::connector::MAX_FETCH_ATTACHMENT_BYTES,
                )
                .await
        })
    }
}
