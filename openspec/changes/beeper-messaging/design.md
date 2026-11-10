## Context

Beeper Desktop runs on the hub Mac and exposes a local HTTP API (documented at developers.beeper.com/desktop-api). Its reference lists `GET /v1/accounts`, `GET /v1/accounts/{accountID}`, `GET /v1/bridges`, `GET /v1/bridges/{bridgeID}/logins`, `GET /v1/chats`, `GET /v1/chats/search`, `GET /v1/chats/{chatID}`, `GET /v1/chats/{chatID}/messages`, `GET /v1/messages/search`, `POST /v1/chats/{chatID}/messages` (send), `GET /v1/info`. The docs say:

- The API runs only while Beeper Desktop runs, and by default it is not reachable from other devices. Default base URL and port are `http://localhost:23373` (from the brief; to be confirmed in the spike).
- The bearer token is created in Beeper Desktop settings. The auth header format is not on the reference page; the spike records it.
- Search and fetch are local and unlimited. Sending may get the user's network account suspended.
- Message history may be incomplete right after an account is added, because Beeper indexes in the background.
- Beeper recommends On-Device Connections. Beeper Cloud keeps encrypted copies and bridges server-side, and its privacy policy allows collection of messages and credentials. This change refuses cloud connections.

The existing code provides the pattern. Each connector implements the `Connector` trait and is called only through `Guarded` (kill switch, capability flags, circuit breaker, bounded transient retry, audit). `Source` already has `Whatsapp`, `Messenger` and `Imessage`. The Google connectors keep their token in a Keychain service with a separate auth helper, and `haste-gcal`'s client and config split is the template. The reminders connector shows the executing state for non-idempotent writes. `email-send` extends that to sends (`begin_send`, `SendOutcome`, `recipients_digest`, `MAX_SENDS_PER_HOUR`), and this change reuses those primitives rather than defining its own lifecycle.

Constraints from the user: on-device only; token only in the Keychain; localhost only; kill switch engaged on first install; selected chats and accounts only; scope tags and the machine filter apply; bounded reads; bodies only as bounded local text; per-hour and per-day send caps; one recipient chat per send; human confirmation with a digest; at-most-once; replies only into existing selected chats; no attachments in v1; poll sync first; Beeper not running or a rejected token is a permission state that never trips the breaker; ToS risk acknowledged in the README.

## Goals / Non-Goals

**Goals**
- Read WhatsApp, Messenger and iMessage chats the human selected, through Beeper, into the unified `Thread` and `Message` model.
- Let the human, and only the human, send one reply into an existing selected chat, with a confirmation that names the chat and shows the text.
- Refuse cloud connections, non-loopback URLs and any unselected chat or account, in code and not only in documentation.
- Keep the token, message text and chat names out of audit, logs and errors.

**Non-Goals (v1)**
- Starting new chats, group creation, attachments, reactions, edits, deletes, read receipts or typing indicators.
- Sending from an agent in any form. Agents propose text only.
- WebSocket sync (later option), account setup or login flows through the API (`bridges/.../login-sessions`), and any Beeper Cloud use.
- Reading `chat.db` directly (that stays with the parked `imessage-connector`).

## Decisions

### D1. One connector crate, three registrations, one per source
A `Connector` reports one `Source`. The crate exposes `BeeperConnector` built for a `BeeperSource` (`Whatsapp`, `Messenger`, `Imessage`). Each registration shares one HTTP client and token, and has its own kill switch, cursor and audit label. A single `beeper` switch would stop iMessage when only WhatsApp is misbehaving; separate switches let the human isolate a network. The CLI names are `whatsapp`, `messenger` and `imessage`. Alternative rejected: one `beeper` connector returning mixed sources. It breaks the one-source-per-connector assumption in the store and the guard.

Open question Q3 covers whether a Beeper-wide switch should also exist. Recommendation: no, the three switches are enough, and the human can engage all three at once.

### D2. Loopback only, enforced in code
The base URL is a constant `BEEPER_BASE_URL = "http://127.0.0.1:23373"`. `beeper.json` may override it only with a `http://127.0.0.1`, `http://localhost` or `[::1]` URL; any other host, scheme with credentials, or path makes the config load fail with a validation error and the connector never starts. The HTTP client disables redirects, so a redirect cannot move the token elsewhere. A test asserts that a non-loopback URL is refused before any connection.

### D3. Token: Keychain only, no environment variable
The token is stored in the macOS Keychain under service `com.haste.beeper`, account `api-token`, through the same `KeychainStore` wrapper the Google connectors use. `haste connector beeper auth` (human build only) reads it from the terminal without echo, checks it with `GET /v1/info` (or the cheapest authenticated call the spike picks), then stores it. The file `beeper.json` never holds it. No `HASTE_BEEPER_TOKEN` variable exists in any build: launchd does not pass the human's shell environment, and an environment token would be readable by `ps` for other processes of the same user. The daemon reads the token once at startup, keeps it in memory only, and never logs or audits it. This matches the existing threat model (a same-user process can read files either way, so the Keychain ACL is the real boundary; the spike confirms the daemon's Keychain access prompt behaviour).

### D4. On-device enforcement
The connector reads accounts with `GET /v1/accounts` and, for each selected account, refuses to run if the account's connection type is cloud. The field name is unknown until the spike (Q1). Two cases:
- The API exposes the connection type: the connector refuses with `refused: cloud connection` for that account and syncs no chat of it. The check runs before every cycle and before every send, not only at first use, so switching an account to cloud later stops it.
- The API does not expose it: the human attests per account in `beeper.json` (`"on_device_attested": true` with the account id). The attestation is a human statement, recorded in the audit line as a label `attested`, and a spike task (Q1) must either find the field or keep the attestation as the permanent mechanism. The connector never assumes on-device when the field is missing and no attestation exists.

A cloud or unknown state is a refusal with a reason, never a silent skip.

### D5. Selection in `beeper.json` (non-secret)
`beeper.json` in the data directory, mode `0600`, no secrets:

```json
{
  "accounts": [
    { "id": "<beeper account id>", "on_device_attested": true }
  ],
  "chats": {
    "allow": ["<chat id>"],
    "deny": []
  },
  "history_days": 30
}
```

No account or chat is read unless it is named. `deny` wins over `allow`. An empty `allow` reads nothing. The file is validated at load (unknown keys are an error, as in `notes.json`). Chat ids are the Beeper ids; the human can list them with `haste beeper chats` (human build), which prints ids, network and counts, never message text.

### D6. Mapping into the unified model
- Account network to source: the account's network field (exact strings from the spike) maps to `Whatsapp`, `Messenger` or `Imessage`. Other networks are not mapped and any selected account on an unmapped network is refused with `unsupported network`.
- Chat to `Thread`: `source` from the mapping, `external_id` equal to the Beeper chat id. The display title is the chat name as given by Beeper. The title is stored locally only, never audited.
- Message to `Message` in its thread: `external_id` equal to the Beeper message id, `sent_at` from Beeper's timestamp, direction from the sender flag.
- Participants to `Person` with the Beeper user id as an identifier. Phone numbers and emails are stored only if Beeper returns them and only in the local store.
- Bodies: stored as text truncated to `BEEPER_BODY_MAX_BYTES` (8 KiB) at a UTF-8 boundary, with a marker when truncated. Non-text message types (media, system events) store a fixed label such as `[attachment]`, never the attachment name.

### D7. Sync: poll, bounded, cursor per chat
Each source polls on its own interval (`BEEPER_POLL_SECS = 300`). Per cycle, for each selected chat (at most `BEEPER_MAX_CHATS_PER_CYCLE = 50`), the connector lists messages newer than the chat's cursor in pages of `BEEPER_PAGE_SIZE = 50`, stopping at `BEEPER_MAX_PAGES_PER_CHAT = 4`. The cursor is a map `chat id -> last sort key` encoded as JSON in the connector cursor (the same pattern as the calendar cursor). The first sync for a chat reads only messages within `history_days` (default 30, cap 90) and never more than `BEEPER_BACKFILL_PER_CHAT = 200` messages. Because history can be incomplete right after an account is added, a chat with fewer messages than expected is not an error. Message upserts are idempotent by external id. The sort key comes from the API (the spike confirms it is sortable and stable).

### D8. Human-only send into existing selected chats
Replies use the existing `Draft` proposal. A proposal's `source_thread_id` names a chat thread and `to` stays empty. On approval the connector creates nothing at Beeper: there is no provider draft in the API, so the Beeper connector declares `draft = false`, and the approval path skips provider draft creation for Beeper sources (the approval code path is confirmed in the apply step).

Sending follows the `email-send` lifecycle:
- `SendPreview { proposal_id }` (human only): returns the chat name, the network, the body byte length, the stale flag, and the sends remaining in the current hour and day. Changes no state.
- `Send { proposal_id, acknowledge_stale, recipients_digest }` (human only): `begin_send` moves `approved -> executing` in one transaction after checking the stale flag, the per-hour and per-day caps, and the selection and on-device checks. Then the connector makes one `POST /v1/chats/{chatID}/messages` request.

The digest is SHA-256 over the chat id, the displayed chat name and the SHA-256 of the body, in that order, hex encoded. If the daemon's fresh resolution differs, the send is refused with `recipients_changed` and nothing changes. The digest covers the chat and the text, so the human confirms the chat they see, not a chat id they cannot read.

Outcome classification (one attempt, no retry anywhere):

| Failure point | Classification | Proposal |
|---|---|---|
| Kill switch, selection, on-device or validation refusal before connecting | refused | `approved` |
| Beeper not running (connection refused) or token rejected (401/403) | `Authorization` (permission needed, never trips breaker) | `approved` |
| 4xx other than auth, such as a chat that no longer exists | `Other` `http 4xx` | `approved` |
| 429 or an account restriction response | `Checkpoint` (breaker: kill switch engaged, human told) | `approved` |
| Timeout or dropped connection after the request was written, or any 5xx | `Transient` tagged `ambiguous` | `executing` (needs check) |
| Success (2xx with a message id) | `Sent` | `sent`, result `sent:beeper:<message id>` |

The `ambiguous` tag reuses the `SendOutcome` enum from `email-send`. A 2xx without a message id is ambiguous, not sent, because the send cannot be proven.

### D9. Limits (named constants)
In `haste-connector-beeper::consts` (and not shared with mail):
- `BEEPER_MAX_SEND_BYTES: usize = 4 * 1024` (checked before `begin_send`)
- `BEEPER_MAX_SENDS_PER_HOUR: u32 = 10` (per account, that is per network; relaxed from 5 by the human on 2026-11-10)
- `BEEPER_MAX_SENDS_PER_DAY: u32 = 20` (across the three networks)
- `BEEPER_SEND_WINDOW_HOUR_SECS: i64 = 3600`, `BEEPER_SEND_WINDOW_DAY_SECS: i64 = 86_400`
- `BEEPER_REQUEST_TIMEOUT: Duration = 30 s`
- Read limits listed in D7.

The caps count rows of this source in `executing` or `sent` whose `send_started_at` falls in the window, so a crash cannot reset them. The numbers are conservative defaults for a personal account; the human may lower them in code, not in a config file, so an agent cannot raise them.

### D10. Errors and breaker
Beeper not running, a refused connection and a rejected token map to `Authorization`, the same class as a missing Reminders permission: waiting on a human, never retried in the cycle, never trips the breaker, and the status view says "Beeper Desktop not running" or "Beeper token rejected; run `haste connector beeper auth`". Account-level restrictions reported by Beeper (for example a network refusing the account) map to `Checkpoint`, which engages that source's switch and notifies the human. Transient failures keep the existing bounded retry (`TRANSIENT_ATTEMPTS_PER_CYCLE`) for reads only.

### D11. Audit and privacy
Per cycle: one entry with source, outcome, counts of new threads and messages, skipped chats by reason label, and cursor resets. Per send: `proposal.send` with `sent`, `refused`, `needs_check` or `failed` plus a reason label and the Beeper message id on success. Audit, logs and errors never contain chat names, message text, handles, phone numbers, token text or the account id. The `beeper.json` file path is never logged with its contents.

### D12. Scope and machine filter
Threads from Beeper get project tags through the existing machine-scope mechanism (herdr workspace labels as candidates, human-confirmed). The exhaustive per-principal scope test is extended to Beeper threads. Unselected chats never reach the store, so they cannot be seen by any principal.

### D13. Kill switch on first install
All three registrations set `default_engaged() = true`. With no persisted state, nothing runs, including the on-device check and the token read at startup. `haste connector release whatsapp|messenger|imessage` (human) releases one source. The token is read at startup only when at least one source is released, so a fresh install reads no token.

### D14. Terms of service
The README states that Beeper's own docs warn of account suspension for sending too many messages, that unofficial network integrations can be blocked, and that this connector is for personal use with a per-hour and per-day cap. The change does not claim the integration is allowed by the networks.

## Risks / Trade-offs

- **[Account suspension from sending]** -> Caps (D9), one recipient chat per send, human confirmation, reads-first. The caps are conservative; the human accepts the residual risk.
- **[Connection type not exposed by the API]** -> Human attestation per account (D4) plus a spike task. The attestation is a statement by the human, not a verified fact, and the README says so.
- **[Cloud accounts silently appear]** -> The on-device check runs before each cycle and each send (D4).
- **[Token in memory of the daemon]** -> Accepted: the daemon must hold the token while it runs. It is never written to disk, audit or logs. A same-user process can still read the Keychain item if its ACL allows it; the spike records the ACL.
- **[Incomplete history after adding an account]** -> Reads are bounded and tolerate gaps (D7). Missing history is not treated as an error.
- **[Ambiguous send]** -> `executing` with a human check, never re-sent (D8, D12 of email-send).
- **[Beeper API drift]** -> Field names come from the spike (structure only). The mapper reads the minimum fields and fails closed on a missing required one, with a `Other` error that names no content.
- **[Beeper Desktop not running most of the time]** -> Authorization state, no breaker, no retry loop.
- **[Two connectors claim Source::Imessage]** -> See Q5.

## Migration Plan

1. Spike group (tasks section 1) on this Mac: no code, outputs structure only. Results recorded in this file's Spike results section.
2. Apply `email-send` first (archived or merged). This change depends on its send lifecycle.
3. Add the crate, store changes and the human-only send path behind default-engaged switches. Ship with all three switches engaged.
4. Human configures `beeper.json` and runs the auth verb, then releases one source at a time.
5. Rollback: `haste connector engage <source>` stops one source; `launchctl bootout` stops the daemon. Delete the Keychain item `com.haste.beeper` to remove the token. No Beeper data is modified: the connector never writes to Beeper except the human-confirmed send.

## Open Questions

- **Q1. RESOLVED 2026-11-10 (see Spike results).** Connection type visibility: Does `GET /v1/accounts` or `GET /v1/bridges/{bridgeID}/logins` expose on-device versus cloud? If not, is the human attestation the permanent mechanism? (Spike S4.)
- **Q2. PARTLY RESOLVED.** Network field values: The exact `network` strings for WhatsApp, Messenger and iMessage on this Mac (for example whether iMessage is a separate bridge). (Spike S3.)
- **Q3. Beeper-wide switch.** Keep three per-source switches only (recommendation), or also a `beeper` switch that stops all three?
- **Q4. RESOLVED.** Auth header and default port: The reference page does not state the base URL, port or header format. The brief says `http://localhost:23373` and a bearer token; the spike must confirm both and the cheapest authenticated call. (Spike S2.)
- **Q5. iMessage overlap with `imessage-connector`.** Both would claim `Source::Imessage`. Options: (a) this change supersedes the parked `imessage-connector` and it is archived as superseded; (b) the Beeper source is named differently; (c) `imessage-connector` stays parked and the Beeper iMessage registration is skipped. Human decision needed before tasks 4 and 6 are written. Recommendation: (a), after the spike shows iMessage works through Beeper on this Mac.
- **Q6. RESOLVED.** Sort key: Whether message sort keys are strings or numbers, sortable, and stable across edits. (Spike S5.)
- **Q7. History window.** Default 30 days; is that the right bound for a personal inbox?
- **Q8. Digest content.** Whether the digest should also include the participants of the chat, given that group chats have many members. Recommendation: no, the chat id, displayed name and body hash are enough, and participants are not shown in the confirmation.
- **Q9. RESOLVED 2026-11-10: 10 per hour per account, 20 per day across all.** Caps: Confirm `BEEPER_MAX_SENDS_PER_HOUR = 5` and `BEEPER_MAX_SENDS_PER_DAY = 20`, and whether they should count sends across all three sources together. Recommendation: together, since one account suspension covers the user's whole network identity.
- **Q10. Account restriction response.** Which response body or status Beeper returns when a network restricts an account; the spike cannot trigger it without sending, so the mapping stays a best guess until a real restriction is seen. Until then any unexpected 403 that is not an auth failure is `Other`, and the human must watch the audit log.
- **Q11. WebSocket.** Whether to add it after the poll path is proven. Not in v1.

## Decisions after the spike

- **Cloud accounts (2026-11-10).** The human chose per-account opt-in over a blanket refusal: `allow_cloud: true` on an account in `beeper.json` lets a cloud-bridged account be read and replied to; without it a cloud account is refused. Attestation never overrides a reported cloud connection. Rationale: the user's WhatsApp and Messenger accounts are both cloud-bridged and the user accepts that for them.

## Spike results

Recorded during section 1 (2026-11-10). Sources: the human's structure-only runs of `scripts/beeper_spike.py` against Beeper Desktop (app version not recorded here) and the local OpenAPI spec the app serves at `GET /v1/spec` (Beeper Client API 5.0.0, unauthenticated, no account data). Structure only: no names, text or ids.

- **Base URL, auth (Q4, S2).** `http://127.0.0.1:23373`, bearer token. `GET /v1/info` is NOT authenticated (200 with a wrong token); every data route answers `401 {message, code}` for a missing or wrong token. The token check therefore uses `GET /v1/accounts`. `/v1/info` also reports `server.remote_access` and `server.mcp_enabled` (not used yet).
- **Accounts (S3).** `{accountID, loginID, bridge{id,type,provider}, network, user, status, statusText, capabilities}`. `network` is a display name (`WhatsApp`; Beeper's own account is `Beeper`); `bridge.type` is the steadier key (`whatsapp`, `matrix`, ...). Observed here: one `Beeper` and one `WhatsApp` account, both `connected`. No Messenger or iMessage account exists in this Beeper yet (task 1.4 stays open).
- **On-device versus cloud (Q1, S4).** The API does report it: `bridge.provider` is one of `cloud`, `self-hosted`, `local`, `platform-sdk` (Beeper Desktop 4.2.785 and later). Decision: `local` is on-device; `cloud` is refused whatever the attestation says; `self-hosted`, `platform-sdk`, a missing field (older Beeper) and anything else need the human's `on_device_attested`. The attestation stays as the fallback, no longer the primary mechanism. Whether iMessage reports `local` or `platform-sdk` is still to be seen once an iMessage account exists.
- **This Mac's accounts (2026-11-10, S3/S4).** Four accounts, all `connected`: `Beeper` (bridge `matrix`, `cloud`), `Facebook` (bridge `facebookgo`, **`cloud`**), `WhatsApp` (bridge `whatsapp`, **`cloud`**) and `iMessage` (bridge `imessage`, **`platform-sdk`**). Chats name Messenger `Facebook/Messenger`; both spellings map to the messenger source. Consequence under the on-device-only rule: WhatsApp and Messenger are currently cloud-bridged and are refused until they are connected on-device in Beeper Desktop; iMessage reports `platform-sdk`, which is not `local`, so it needs the human's `on_device_attested` (the human decides whether Beeper's iMessage integration counts as on this Mac).
- **Chats and messages (S5, Q6).** Chat: `id, localChatID, accountID, network, title, type (single|group), lastActivity, participants, ...`. Message: `id, chatID, accountID, senderID, senderName, timestamp (RFC 3339), sortKey (string, unique, sortable), type (TEXT, NOTICE, IMAGE, VIDEO, VOICE, AUDIO, FILE, STICKER, LOCATION, REACTION), text, isSender, isHidden, isDeleted, attachments, ...`. Lists answer `{items, hasMore, oldestCursor, newestCursor}`; `cursor` with `direction=before` (the default) pages older, `after` newer; cursors are opaque. The chat list is newest-first and paginated (default 25, max 200), so selected chats are fetched by id instead of from the list. Reactions, notices, hidden and deleted messages are skipped.
- **Send (S6 partly).** `POST /v1/chats/{chatID}/messages` with `{text, replyToMessageID?, attachment?}` answers `{chatID, pendingMessageID}`. The message id is a pending id: delivery is asynchronous (the message's `sendStatus` reports SUCCESS, PENDING, FAIL_RETRIABLE, FAIL_PERMANENT). Whether to follow a send up with a status read is open (Q12).
- **Not-running error shape (S6).** Still to record by hand (task 1.7b); the connector already treats a refused connection as "not running".
- **Q12. Send follow-up.** The send answer is a pending id. Should the daemon read the message's `sendStatus` once after the send to turn an asynchronous failure into a visible result, or leave it to the human (the proposal stays `sent`)? Recommendation: leave it for v1 and mention it in the smoke check.
