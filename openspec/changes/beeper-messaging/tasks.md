## 1. Human spike (blocking, no code, structure-only output)

- [ ] 1.1 Confirm Beeper Desktop is installed and running on this Mac, and record its version (no account names)
- [ ] 1.2 Human creates the API token in Beeper Desktop settings and records where the setting is; the token is not written into any file or the shell history
- [x] 1.3 Spike S2: confirm the base URL and port (default `http://localhost:23373`), the auth header format and the cheapest authenticated call (for example `GET /v1/info`) with a single manual request whose output shows status code and key names only
- [x] 1.4 Spike S3: from `GET /v1/accounts`, record the count of accounts per `network` value (exact strings) and whether each account is WhatsApp, Messenger or iMessage; confirm the iMessage bridge on this Mac, given that `chat.db` is empty here
- [x] 1.5 Spike S4: determine whether `GET /v1/accounts`, `GET /v1/accounts/{accountID}` or `GET /v1/bridges/{bridgeID}/logins` expose an on-device versus cloud connection type (record the field name and value set, or record that none exists); decide whether human attestation is the permanent mechanism (design Q1)
- [x] 1.6 Spike S5: confirm the chat list and message list field names, the message sort key type and stability, the timestamp field, and whether history is complete for a chat on this Mac (record counts only)
- [ ] 1.7 Spike S6: confirm the error shape when Beeper Desktop is not running and when the token is wrong, by requesting with the app stopped or with a wrong token; no send is made
- [x] 1.8 Record the spike outcomes (structure only, no message text, no names, no ids) in design.md under Spike results
- [ ] 1.9 Human decides design Q3 (per-source switches only), Q5 (iMessage overlap with `imessage-connector`), Q7 (history window), Q8 (digest content) and Q9 (cap scope); record decisions in design.md
- [ ] 1.10 Human sign-off on section 1 before section 2 starts

## 2. Core constants and dependencies

- [x] 2.1 Confirm `email-send` is archived or merged; if not, stop this change at section 6 and mark it blocked
- [x] 2.2 Add `BEEPER_*` constants (base URL, send caps, send windows, request timeout, poll interval, page size, page and chat limits, backfill cap, body cap, history window) in `haste-connector-beeper::consts`, with a test that the caps are non-zero and `BEEPER_MAX_SEND_BYTES` is at most `MAX_SEND_BODY_BYTES` of haste-core
- [x] 2.3 Confirm `Source::Whatsapp`, `Source::Messenger` and `Source::Imessage` round-trip by name in `haste-core`
- [x] 2.4 Confirm the default-engaged flag on the connector descriptor (provided by the connector framework) is available for three registrations

## 3. Store (haste-store)

- [x] 3.1 Confirm the send-lifecycle columns from `email-send` (`send_started_at`, `send_outcome`) are used for Beeper sends; add a source column filter for the caps if missing
- [x] 3.2 Implement the per-source send window count over `executing` and `sent` rows whose `send_started_at` falls in the window
- [x] 3.3 Store tests: hourly and daily counts include `executing`, survive reopening the database, and count only the given source
- [x] 3.4 Store test: a Beeper chat thread with `to` empty is stored and retrieved with its source and external id

## 4. Beeper client, token and config (haste-connector-beeper)

- [x] 4.1 Create the crate `crates/haste-connector-beeper` with the `Connector` implementation per source and the `beeper` helper binary for the auth verb (human build)
- [x] 4.2 Implement `beeper.json` loading with unknown-key rejection, the account and chat allow-list and deny-list, `on_device_attested`, `history_days` (cap 90) and the loopback-only base URL override, with tests for each refusal
- [x] 4.3 Implement the Keychain token store under `com.haste.beeper` (account `api-token`) through the existing wrapper, with an in-memory fake for tests
- [x] 4.4 Implement the auth helper: read the token without echo, check it with the chosen authenticated call, store it only on success; a failed check stores nothing
- [x] 4.5 Implement the HTTP client on loopback with redirects disabled, `BEEPER_REQUEST_TIMEOUT`, no retry and one error classifier mapping connection refused to `Authorization` "Beeper Desktop not running", 401 and 403 to `Authorization` "Beeper token rejected", 429 and 5xx by the send or read rules
- [x] 4.6 Test: a non-loopback URL is refused before any connection; a redirect is not followed; the token is never in an error string

## 5. Read path (haste-connector-beeper)

- [x] 5.1 Implement the on-device check per account (cloud refusal, attestation fallback, unknown refusal) and run it before every cycle
- [x] 5.2 Implement the network-to-source mapping with the exact spike values and the "unsupported network" refusal
- [x] 5.3 Implement the chat list and message list mappers to `Thread`, `Message` and `Person`, with body truncation at a UTF-8 boundary and the `[attachment]` label
- [x] 5.4 Implement the per-chat cursor (chat id to sort key) encoded in the connector cursor, the first-sync history window and backfill cap, the page and chat limits, and idempotent upserts
- [x] 5.5 Implement the counts-only cycle audit with skipped-chat reason labels
- [x] 5.6 Build the fake Beeper server on loopback (accounts, chats, messages, send, info, failure injection per endpoint) and record requests without bodies
- [x] 5.7 Read tests: unselected chat not requested, deny wins, backfill cap, truncation, attachment label, delta cycle, idempotent re-read, cloud account refused, unknown type refused, not-running is Authorization with no breaker, and audit has no text

## 6. Human-only send path

- [x] 6.1 Implement the send request builder: one `POST /v1/chats/{chatID}/messages` with the text only, refusing an attachment field and any chat not selected
- [x] 6.2 Implement the digest over chat id, displayed name and body SHA-256 (reuse the `recipients_digest` helper of `email-send`) with a test that a name change alters it
- [x] 6.3 Implement the outcome classification table of design D8 onto `SendOutcome` (`NoEffect`, `Ambiguous`, `Sent`) and the `Authorization` and `Checkpoint` mapping
- [x] 6.4 Implement the send method on each Beeper connector, performing exactly one request, with no retry, and the on-device and selection checks before any request
- [x] 6.5 Send tests: one request on success, no request on refusal before send (cloud, unselected, kill switch, cap, digest mismatch), 401 returns to approved with no retry, connection refused returns to approved, 429 returns to approved, timeout after write is ambiguous, 2xx without id is ambiguous, 400 returns to approved, 403 restriction opens the breaker

## 7. Daemon wiring and dispatch (hasted)

- [x] 7.1 Register the three Beeper connectors with default-engaged switches, their own polling intervals and the `Guarded` wrapper
- [x] 7.2 Route the `SendPreview` and `Send` requests from `email-send` to the Beeper connectors for Beeper threads, without changing the email path
- [x] 7.3 Make approval of a Beeper draft skip provider draft creation, and test that no Beeper request occurs on approval
- [ ] 7.4 Map Beeper refusals (`kill switch engaged`, `connector stopped by circuit breaker`, `recipients_changed`, caps, `unsupported network`, `cloud connection`) to distinct codes with no content in the message
- [x] 7.5 Audit `proposal.send` (sent, refused, needs_check, failed) with labels and the Beeper message id only
- [x] 7.6 Dispatch tests with fake connectors: one connector call per send, agent denied for preview and send, kill switch refuses preview and send, a restart leaves `executing` untouched

## 8. Approvals TUI and status (haste-tui)

- [ ] 8.1 Show the chat send confirmation with the chat name, network, text and digest, requiring `y`
- [ ] 8.2 Handle `recipients_changed` by reloading the preview without sending
- [ ] 8.3 Show Beeper needs-check rows with the outcome label only and offer only "mark sent" and "return to approved", each behind a confirmation
- [ ] 8.4 Show per-source Beeper status with the "not running" and "token rejected" permission states and the fixing command
- [ ] 8.5 Snapshot tests: confirmation text contains the chat name and text, the needs-check row contains no text

## 9. Cross-cutting tests and scope

- [ ] 9.1 Extend the exhaustive per-principal scope-filter test to Beeper threads
- [x] 9.2 Add a scan test: a distinctive chat name, message text and token marker never appear in audit output or captured logs for success, refusal, not-running and ambiguous outcomes
- [x] 9.3 Add a test that agents and machine principals are denied `SendPreview` and `Send` for Beeper threads
- [x] 9.4 Add a test that the agent CLI and TUI crates do not depend on `haste-connector-beeper` or `reqwest`

## 10. Documentation and checks

- [x] 10.1 Add a README section "Beeper (WhatsApp, Messenger, iMessage)": setup, the on-device requirement and the attestation, the token step, the selection file, the send limits, the Terms-of-Service risk (Beeper's own warning about suspension and that unofficial integrations can be blocked), and that this is for personal use
- [x] 10.2 Update the README Policy table (Beeper send: human only, one chat, confirmed) and the Spec coverage notes
- [x] 10.3 Update the roadmap entry `whatsapp-messenger-spike` to point to this change
- [x] 10.4 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`; record results

## 11. Manual smoke (human, real Beeper Desktop, throwaway chat)

- [ ] 11.1 Fresh install: start the daemon and confirm `haste connector status` lists `whatsapp`, `messenger` and `imessage` engaged and no Beeper request is made
- [ ] 11.2 With Beeper Desktop quit, release `whatsapp` and confirm the status shows "Beeper Desktop not running", no breaker, and no retry loop in the audit log
- [ ] 11.3 Start Beeper Desktop, create the token with `haste connector beeper auth`, confirm no token text is in any file under the data directory
- [ ] 11.4 Select one throwaway WhatsApp chat in `beeper.json` and one account, release `whatsapp`, and confirm one cycle adds one thread with counts-only audit
- [ ] 11.5 Confirm a cloud account (if one exists) is refused, or confirm the attestation path for an on-device account
- [ ] 11.6 Propose a reply to the throwaway chat as an agent, approve it, and confirm nothing was sent at approval
- [ ] 11.7 Send the reply from the TUI after reading the confirmation; confirm it arrives in the throwaway chat once and the proposal shows `sent` with `sent:beeper:<id>`
- [ ] 11.8 Repeat the send of the same proposal and confirm it is refused (not `approved`) with no second message
- [ ] 11.9 Engage `whatsapp` and confirm preview and send are refused before any request
- [ ] 11.10 Delete the throwaway chat's messages by hand in Beeper Desktop (haste never deletes) and remove the Keychain item `com.haste.beeper` if the integration is retired
