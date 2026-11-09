## Context

`core-policy-gate` is implemented: `haste-core` defines the `Connector` trait (`fetch`, `send`, `capabilities`), the `Guarded` wrapper (capability check, kill switch, circuit breaker, bounded transient retries), `Batch`/`Cursor` types, and the `Hooks` trait for persistence, audit and notification. `hasted` owns the SQLite store (single writer thread) and the scheduler. Thread and message types already carry `Source::Gmail`. Nothing calls an external service yet.

Constraints from the existing design:
- The daemon is the only writer and the only process that touches the store.
- Agents never receive `send`. Drafts reach Gmail only through an approved proposal.
- Tokens are never printed or stored in plaintext in the store or audit log.
- The circuit breaker treats `Challenge` and `Checkpoint` as terminal, with no retry.

## Goals / Non-Goals

**Goals:**
- A working `Connector` for one Gmail account with `read` and `draft` capabilities.
- Incremental sync using Gmail history IDs, so steady-state cycles are cheap.
- OAuth tokens kept in the macOS Keychain, with the daemon as the only reader.
- Draft creation (plain text) as the only write to Gmail, performed only after human approval.
- Human-only administration of connector kill switches (status, release, engage), usable by every connector.

**Non-Goals:**
- Sending mail (`send = false`), archive, label or delete operations in Gmail.
- Multiple accounts, Pub/Sub push, attachment content download.
- Changing the `Connector` trait signatures unless implementation proves it necessary.

## Decisions

**D1. Separate crate `haste-connector-gmail`, depending on `haste-core` only.**
Keeps HTTP, TLS and OAuth dependencies out of `haste-core`, `haste-store` and the thin clients. The `haste` CLI and `haste-tui` must still not depend on `rusqlite` or `haste-store`, so the connector is linked only into `hasted`. Alternative: a module inside `hasted`. Rejected because it would mix provider code into the daemon's dispatch logic and make the connector hard to test in isolation.

**D2. HTTP via `reqwest` with rustls, JSON via `serde`/`serde_json`.**
A blocking-free async client fits the existing tokio runtime. rustls avoids linking the system OpenSSL. Alternative: `ureq` (blocking), which is smaller but would need `spawn_blocking` around every fetch inside the async guard. Rejected for simplicity of the async path.

**D3. OAuth installed-app flow with a loopback redirect on `127.0.0.1` and a random port.**
The human runs `haste connector gmail auth` (human build only). The verb starts a one-shot listener on loopback, opens the consent URL, receives the code, and exchanges it for tokens. PKCE is used. Scopes: `gmail.readonly` and `gmail.compose` only. No `gmail.modify` or `mail.google.com` scope, so the token cannot archive or delete even if the code were compromised. Alternative: device-code flow. Rejected because Google restricts device flow for Gmail scopes.

*Implementation note:* the flow runs in a separate helper binary `haste-gmail-auth` (in the Gmail crate). The human-only `haste connector gmail auth` verb just starts it, so the `haste` CLI and `haste-tui` stay free of `reqwest` and Keychain code (verified with `cargo tree`). Secrets reach the helper through the environment, never the argument list. The helper tells the daemon through a human-only `ConnectorAuthorized` request so the event is audited.

*Cloud project:* the OAuth client belongs to a personal Google Cloud project whose publishing status is "In production", left unverified. A project in "Testing" status is rejected because its refresh tokens expire after 7 days. Unverified production use is limited to 100 users (this is single user) and shows a "Google hasn't verified this app" warning once at consent, which the human clicks through. The one-time setup (create project, enable Gmail API, configure the consent screen with the two scopes, create a Desktop OAuth client, set publishing status to production) is documented in `README.md`. Refresh tokens then are not subject to a 7-day clock; they can still be invalidated by revoking the app, a password change, or six months of inactivity.

**D4. Refresh token stored in the macOS Keychain via `security-framework`.**
Service `com.haste.gmail`, account = the Gmail address. Access tokens are kept in memory only and refreshed on demand. The Keychain item is the only copy of the refresh token. Alternative: a `0600` file under `HASTE_HOME`, as used for `human.token`. Rejected because the refresh token grants long-lived access to the whole mailbox and the Keychain gives an access-control boundary that a plain file does not.

**D5. Sync cursor is Gmail `historyId`, stored as `Cursor`.**
- First cycle: `users.messages.list` for the last N days (compile-time constant `BACKFILL_DAYS` = 30, not configurable), then `users.threads.get` per thread, mapped into `Batch`.
- Later cycles: `users.history.list` with `startHistoryId` = stored cursor. Changed thread IDs are re-fetched in full, so the mapper always sees a complete thread.
- If Gmail returns 404 for the stored history ID (history expired, roughly a week), fall back to a bounded backfill and reset the cursor. This is treated as `Other`, not retried in a loop.

**D6. Mapping.**
- `Thread.external_id` = Gmail thread ID, `source` = `gmail`, `subject` from the first message, participants from `From`/`To`/`Cc` headers deduplicated.
- `Message` built from `payload.headers` and `snippet`. Plain-text body extracted from `text/plain` parts only; HTML is not stored. Body length capped by constant `MAX_BODY_BYTES`.
- Labels `INBOX`, `UNREAD`, `SENT` are read only to set the local `archived` and read state where the data model has fields for them. Labels are never written back.

*Implementation notes:* a message with no `text/plain` part is stored with `body_text` = the marker `[body unavailable] ` + its snippet (the `Message` type has no separate flag, and changing it would need a store migration). `Message::thread_id` in a connector `Batch` carries the provider's thread id; the daemon resolves it to the stored thread. Labels are read into a `Labels` value by the mapper but not persisted: archive state is user-owned triage, which re-syncs must not overwrite, and the data model has no read flag.

**D7. Draft creation is a proposal execution, not a connector call from the agent path.**
An approved `draft` proposal makes the daemon call `users.drafts.create` with a plain-text MIME message (`Content-Type: text/plain; charset=UTF-8`, no HTML or multipart/alternative part) built from the proposal body and the thread's `In-Reply-To`/`References` headers, so the draft lands in the right thread. The result is the Gmail draft ID, recorded on the proposal and in the audit log. The connector never sends: `send` returns `CapabilityMissing` through the guard.

*Implementation note:* drafting needed a connector call that is not `send`, so `Connector` gained a `create_draft` method with a default that returns an error, and `Guarded::create_draft` (capability `draft`, kill switch, breaker, audit `draft.create`). Existing connectors are unaffected. This is a small extension to the connector-framework contract. The daemon runs it inside `Approve`, passes the thread's provider id in `DraftToSend::thread_id`, and records `draft:<id>` as the proposal result; the proposal stays `approved`, and a failure is audited and shown as a notice.

**D8. Circuit breaker and error mapping.**
- Refresh rejected with `invalid_grant` (token revoked or expired) → `Checkpoint`, so the guard engages the kill switch and notifies the human that re-authorization is needed (`haste connector gmail auth`, then `haste connector release gmail`). Never retried.
- HTTP 401 after one successful token refresh → `Other` (unexpected auth failure). Not retried.
- HTTP 403 with `rateLimitExceeded`/`userRateLimitExceeded`, 429, 5xx → `Transient`, bounded by `TRANSIENT_ATTEMPTS_PER_CYCLE`.
- Any response Google uses for account verification or restriction (`accountDisabled`, the reauth/challenge responses, `failedPrecondition` with a verification reason) → `Challenge` or `Checkpoint`. The mapper errs on the side of `Checkpoint` when unsure, so the breaker opens.

**D9. Scheduler integration.**
The existing scheduler calls `Guarded::fetch` on a fixed interval (constant `GMAIL_SYNC_INTERVAL_SECS`, default 300). The kill switch for `gmail` starts disengaged after a successful `auth`. Engaging it stops sync without losing the cursor; releasing it resumes from that cursor.

**D11. Connector administration is generic and human-only.**
Three new protocol requests in `haste-proto`: `ConnectorStatus` (returns each known source with `engaged: bool`), `ConnectorRelease { source }` and `ConnectorEngage { source }`. All three map to the existing human-only `manage` action, so the policy gate already denies them for agent and machine principals, and every call is audited (`connector.release` / `connector.engage`, source, outcome). Release and engage publish a `Notice` event so open TUIs update. `haste-store` gains a `list_kill_switches` query over `connector_state`. The `haste connector status|release|engage <source>` verbs and the Gmail `auth` verb live in the same human-only `connector` group, compiled only with the `human` feature. The TUI gets a connectors panel (list with engaged markers, `R` to release, `E` to engage) that calls the same requests; release asks for a `y` confirmation. Releasing never clears the stored cursor, and a breaker that trips again re-engages the switch immediately. Alternative: a separate connector-admin change. Rejected because Gmail is the first real connector and the gap would otherwise ship with it.

**D10. Testing.**
- Unit tests for the mapper using recorded JSON fixtures under `crates/haste-connector-gmail/tests/fixtures/`.
- Unit tests for error mapping (status and body → `ConnectorError`) including the challenge/checkpoint cases.
- A fake HTTP server test for OAuth token refresh and history expiry fallback.
- No live Google calls in the test suite. A manual smoke check is documented, not automated.

## Risks / Trade-offs

- **[Refresh token theft from the Keychain by same-user processes]** → Same boundary as the existing threat model ("does not protect against another process running as the same macOS user"). Mitigation: minimal scopes, no send or modify scope, kill switch on compromise.
- **[Google verification or quota changes]** → Restricted Gmail scopes normally need Google app verification for broad distribution. Mitigation: single-user personal use of an unverified app in production status (under the 100-user cap), which avoids both verification and the 7-day testing-mode token expiry. If Google tightens unverified use, the connector fails closed (kill switch plus notification) and the human decides.
- **[History expiry on long downtime]** → Forces a bounded re-backfill that may re-emit threads. Mitigation: upsert by `(source, external_id)` already dedupes in the store; stale-thread detection in proposals still applies.
- **[Misclassifying a restriction as a transient error]** → Breaker does not open and the connector keeps hitting the account. Mitigation: unknown error bodies default to `Checkpoint` (fail closed), and the guard never retries those.
- **[Draft created from a stale thread]** → The reply draft may quote outdated context. Mitigation: the existing stale-thread flag blocks approval when the source thread changed after proposal creation.
- **[HTML-only messages lose body text]** → Some mail has no `text/plain` part. Mitigation: store the snippet and mark the message as body-unavailable; HTML-to-text is a later change.

## Migration Plan

1. Add the crate and the `connector-gmail` capability behind a Cargo feature `gmail` on `hasted`, off by default, so the daemon builds and runs unchanged without it.
2. Complete the one-time Google Cloud project setup (production publishing status), then run `haste connector gmail auth` once (human build) to store the refresh token in the Keychain.
3. Enable the feature in the release build and reload the launchd agent.
4. Rollback: engage the `gmail` kill switch, or `launchctl bootout` and rebuild without the feature. Delete the Keychain item `com.haste.gmail` to revoke local access (or `haste connector engage gmail`, which keeps the cursor); revoke the app in the Google account page to revoke remote access. The store keeps existing threads; no schema rollback is needed because the store schema is unchanged by this change.

## Open Questions

None. The testing-mode, plain-text draft, constant backfill window and connector-admin placement questions are resolved above (D3, D5, D7, D11).
