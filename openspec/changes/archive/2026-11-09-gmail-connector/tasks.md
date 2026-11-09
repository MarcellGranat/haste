## 1. Crate and dependencies

- [x] 1.1 Add `crates/haste-connector-gmail` to the workspace with a library target depending only on `haste-core`
- [x] 1.2 Add HTTP (`reqwest` with rustls), `serde`/`serde_json`, and `security-framework` dependencies; confirm `cargo tree -p haste -p haste-tui` does not pull them in
- [x] 1.3 Add `BACKFILL_DAYS` (compile-time constant, 30, not configurable), `MAX_BODY_BYTES`, `GMAIL_SYNC_INTERVAL_SECS` and the Gmail scope constants in one module

## 2. Keychain token storage

- [x] 2.1 Implement a Keychain store for the refresh token under service `com.haste.gmail` with get, set and delete
- [x] 2.2 Unit-test the store behind a trait so tests use an in-memory fake
- [x] 2.3 Implement in-memory access-token cache with refresh on demand and invalid-grant mapping to `Checkpoint` (breaker engages the kill switch and notifies; no retry)

## 3. OAuth consent flow

- [x] 3.1 Implement PKCE verifier and challenge generation with a test against the RFC 7636 vector
- [x] 3.2 Implement the loopback redirect listener bound to `127.0.0.1` with a random port, one response, and timeout
- [x] 3.3 Implement the code-for-token exchange and store the refresh token through the Keychain store
- [x] 3.4 Add the human-only `haste connector gmail auth` verb behind the `human` feature, and confirm the agent build does not compile it
- [x] 3.5 Document the one-time Google Cloud project setup in `README.md` (create project, enable Gmail API, consent screen with the two scopes, Desktop OAuth client, set publishing status to "In production", expect one unverified-app warning)

## 4. Gmail HTTP client and error mapping

- [x] 4.1 Implement a thin client for `users.messages.list`, `users.threads.get`, `users.history.list` and `users.drafts.create` with an injectable base URL
- [x] 4.2 Implement response-to-`ConnectorError` mapping: 429, 5xx and rate-limit reasons to `Transient`; invalid-grant to `Checkpoint`; restriction and verification bodies to `Checkpoint` or `Challenge`; unknown bodies to `Checkpoint`; other unrecognised non-verification failures to `Other`
- [x] 4.3 Unit-test the mapping with recorded fixtures under `crates/haste-connector-gmail/tests/fixtures/`

## 5. Mapper

- [x] 5.1 Map Gmail thread and message JSON into `Thread` and `Message`, including deduplicated participants from `From`/`To`/`Cc`
- [x] 5.2 Extract `text/plain` bodies only, mark HTML-only messages as body unavailable, truncate to `MAX_BODY_BYTES` on a UTF-8 boundary
- [x] 5.3 Read `INBOX`, `UNREAD` and `SENT` labels into local state only, with no label writes
- [x] 5.4 Unit-test the mapper for the three-message thread, HTML-only message, and long-body scenarios

## 6. Connector implementation

- [x] 6.1 Implement the `Connector` trait for Gmail with capabilities `read = true`, `draft = true`, `send = false`, `archive = false`
- [x] 6.2 Implement `fetch` with first-cycle backfill, history-based delta sync, empty-delta handling, and expired-cursor fallback that resets the cursor without looping
- [x] 6.3 Implement `send` as unsupported, and confirm the guard returns `CapabilityMissing` for it in a test
- [x] 6.4 Test `fetch` against a local fake HTTP server covering backfill, delta, empty delta, and expired history

## 7. Draft creation

- [x] 7.1 Build a plain-text MIME draft (`text/plain; charset=UTF-8`, single part, no HTML) with `In-Reply-To` and `References` headers from the target thread, and test that no HTML or multipart/alternative part is produced
- [x] 7.2 Wire the approved `draft` proposal execution in `hasted` to call `users.drafts.create` and record the draft ID on the proposal
- [x] 7.3 Test that a pending or rejected proposal makes no drafts request, and that a stale proposal requires the stale confirmation before creating a draft

## 8. Daemon integration

- [x] 8.1 Register the Gmail connector in the scheduler behind the `gmail` Cargo feature on `hasted`, default off
- [x] 8.2 Run scheduled fetches through `Guarded::fetch` at `GMAIL_SYNC_INTERVAL_SECS`, and confirm a kill-switch engagement preserves the stored cursor
- [x] 8.3 Add audit entries for `fetch` outcomes, `draft.create`, and auth events, and confirm no bodies or tokens appear in the audit log
- [x] 8.4 Add an end-to-end test in `crates/hasted/tests/` using the fake Gmail server and the in-memory Keychain fake

## 9. Connector administration (all connectors, human only)

- [x] 9.1 Add `list_kill_switches` to `haste-store` (every known source with its engaged flag) with a unit test
- [x] 9.2 Add `ConnectorStatus`, `ConnectorRelease` and `ConnectorEngage` request types and responses to `haste-proto`, with serde round-trip tests
- [x] 9.3 Handle the three requests in `hasted` dispatch under the human-only `manage` action: validate the source is registered, audit `connector.release` / `connector.engage`, publish a `Notice` event, and leave the stored cursor untouched
- [x] 9.4 Add `haste connector status|release|engage <source>` to the CLI behind the `human` feature, and test that the agent build has no `connector` command
- [x] 9.5 Add a connectors panel to `haste-tui` (list with engaged markers, `R` release with `y` confirmation, `E` engage) with key-handling tests that need no live terminal
- [x] 9.6 Add end-to-end tests in `crates/hasted/tests/`: agent and machine tokens are denied and audited, human release resumes fetching from the stored cursor, unknown source is a validation error, and a re-tripped breaker engages the switch again

## 10. Verification and docs

- [x] 10.1 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`
- [x] 10.2 Run `cargo test -p haste --features human` and confirm the auth verb is present only in the human build
- [x] 10.3 Confirm `cargo tree -p haste -p haste-tui | grep -E 'reqwest|security-framework'` prints nothing
- [x] 10.4 Document the manual smoke check (auth, first sync, one draft approval) in `README.md` and tick roadmap item 1
