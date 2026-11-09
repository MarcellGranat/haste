## 1. Human spikes (deferred: run after the code is built against fakes; the human chose to build first)

- [ ] 1.1 Human authorizes Gmail OAuth on this Mac with `haste connector gmail auth` and confirms `haste connector status` lists `gmail`; record the result in design "Spike outcomes"
- [ ] 1.2 Human creates an iCloud app-specific password and runs `haste connector icloud auth`; confirm the Keychain item exists and `icloud.json` has no secret field
- [ ] 1.3 Spike S3a: confirm SMTP login works with the stored password against `smtp.mail.me.com:587` using a throwaway script outside the repo (STARTTLS, `EHLO`, `AUTH`, `QUIT`), and record which AUTH mechanism Apple accepts (LOGIN or PLAIN) for design D7
- [ ] 1.4 Spike S3b: human sends one self-addressed plain-text mail through the spike script and records whether iCloud files it into Sent by itself (answers design Q2); record whether the sent copy is visible in the Sent Messages folder
- [ ] 1.5 Spike S2: confirm with one manual call (Gmail API Explorer or a curl with the OAuth token from Keychain, outside the repo) that `users.messages.send` with a raw message to the human's own address is allowed under `gmail.compose`; if refused, record fallback to `users.drafts.send` (answers Q7 and design D6)
- [ ] 1.6 Human confirms a send to their own address works end to end through the spike and records the Gmail message id and the arrival time
- [x] 1.7 Human decides design Q1 (delete approval-time Gmail draft after send), Q3 (no iCloud Drafts copy), Q4 (reply target from From or stored Reply-To), Q5 (single To, no Cc/Bcc), Q6 (send flag true), Q8 (result id content), Q9 (typed confirmation for human override); record the decisions in design.md
- [x] 1.8 Record the SMTP client decision (hand-written, D7) or switch to lettre if the spike shows the hand-written path cannot be done with native-tls STARTTLS

## 2. Core types and limits (haste-core)

- [ ] 2.1 Add `haste_core::send` with the named constants `MAX_RECIPIENTS`, `MAX_SEND_BODY_BYTES`, `MAX_SUBJECT_BYTES`, `MAX_SENDS_PER_HOUR`, `SEND_WINDOW_SECS` and `SEND_TIMEOUT`
- [ ] 2.2 Add `SendOutcome` (`NoEffect`, `Ambiguous`, `Sent`) and a `SendError` type carrying a fixed label
- [ ] 2.3 Add the address and subject validators (CR, LF, NUL, comma, semicolon, whitespace, local@domain shape, byte limits) with unit tests for each refusal
- [ ] 2.4 Add `recipients_digest` (SHA-256 over sorted recipients, hex) with a test that order does not change the digest and any change does
- [ ] 2.5 Remove `approved -> sent` from `transition()` and update the proposal transition tests so `approved -> sent` is refused and the permitted set is exactly the modified lifecycle
- [ ] 2.6 Add `Source` sendability checks and serde round-trip tests for the new `DraftToSend` recipient list field

## 3. Store (haste-store)

- [ ] 3.1 Add a migration adding `send_recipients`, `send_started_at` and `send_outcome` columns to proposals
- [ ] 3.2 Implement `begin_send(id, acknowledge_stale, now, recipients)` as one transaction: state check (`approved` only), stale check, rate-limit count over `SEND_WINDOW_SECS` including `executing`, store recipients, move to `executing`
- [ ] 3.3 Implement `finish_send(id, result, now)` (`executing -> sent`) and `fail_send_no_effect(id, reason, now)` (`executing -> approved`), refusing any other state
- [ ] 3.4 Implement `mark_send_needs_check(id, label, now)` that leaves the state `executing` and records the fixed outcome label
- [ ] 3.5 Implement the human override methods `human_mark_sent` and `human_return_to_approved` for executing sends, each refusing non-send and non-executing proposals
- [ ] 3.6 Store tests: each transition permitted and refused, the rate limit counts `executing` rows and survives reopening the database, the recipient list is stored once and never rewritten
- [ ] 3.7 Store test: a restart with an `executing` send lists it as needing a check and changes nothing

## 4. Gmail send (haste-connector-gmail)

- [ ] 4.1 Add the send MIME builder for plain text with `To`, `Subject`, `In-Reply-To`, `References`, `Date`, `Message-ID`, refusing CR/LF instead of stripping (separate from the draft builder)
- [ ] 4.2 Add the Gmail client send call (`users.messages.send` with `raw` and `threadId`, or `users.drafts.send` per D6) that performs exactly one HTTP request with no retry
- [ ] 4.3 Map Gmail responses to `SendOutcome`: 4xx and 429 to `NoEffect`, 5xx and timeout after write to `Ambiguous`, `invalid_grant` before the request to `Checkpoint`
- [ ] 4.4 Set `capabilities().send = true`; keep the scope constants unchanged and add a test that the requested scopes are still exactly `gmail.readonly` and `gmail.compose`
- [ ] 4.5 Implement the Gmail `send` trait method for the human path only and remove the stub error
- [ ] 4.6 Implement best-effort approval-draft cleanup after a successful send if Q1 says delete; never retried, audited with a count only
- [ ] 4.7 Extend the local fake Gmail server with `users.messages.send` and `users.drafts.delete`, recording requests with bodies excluded from the log
- [ ] 4.8 Gmail tests: one request on success, no request on refusal before send, 429 returns to approved with zero retries, 500 after request stays ambiguous with zero retries, timeout after write is ambiguous, the sent MIME has one text/plain part and no injected header

## 5. iCloud SMTP client (haste-connector-icloud)

- [ ] 5.1 Add `smtp.rs` with a phase-aware client: connect, `EHLO`, `STARTTLS`, TLS upgrade through `native_tls` on the same socket, `EHLO`, `AUTH` (mechanism per task 1.3), `MAIL FROM`, `RCPT TO`, `DATA` with dot-stuffing, `QUIT`
- [ ] 5.2 Add a plaintext transport trait for tests only, mirroring `PlainOpener`, that the production build never selects
- [ ] 5.3 Enforce no retries: each command is sent once; a reply timeout is recorded with the phase it occurred in
- [ ] 5.4 Map phases to outcomes: failure before `354` is `NoEffect`, `AUTH` 535 is `Checkpoint`, failure after `354` or after body write is `Ambiguous`, certificate failure is `NoEffect`
- [ ] 5.5 Add the RFC 5322 composer (plain text, CRLF, headers from the send spec, dot-stuffing, generated Message-ID using the connector domain) with tests for header refusal and dot-stuffing
- [ ] 5.6 Add a loopback fake SMTP server in `testing.rs` recording the command sequence and the message bytes, and able to fail at each phase
- [ ] 5.7 SMTP tests: exact command order on success, failure at connect, STARTTLS, AUTH, RCPT TO, after DATA, and no repeated commands

## 6. iCloud Sent copy and config

- [ ] 6.1 Add `sent_folder` to `IcloudConfig` (default `Sent Messages`), serialized in `icloud.json`, with a test that the default applies to a minimal file
- [ ] 6.2 Extend the fake IMAP server with `APPEND` (literal handling and `\Seen` flag) and keep refusing other mailbox writes
- [ ] 6.3 Implement `append_sent(message, folder)` as one new IMAP session with its own LOGIN, one APPEND, LOGOUT; no retry
- [ ] 6.4 Implement the iCloud `send` trait method: compose, SMTP submit, on acceptance append copy, return `SendReceipt` with `sent:smtp` or `sent:smtp;copy_failed`
- [ ] 6.5 Tests: APPEND issued exactly once after `250`, APPEND failure keeps the send as sent with the copy_failed label and no second submission, no APPEND before acceptance, the sync-path test still records no write command other than the send path
- [ ] 6.6 Set `capabilities().send = true` and test the capability declaration

## 7. Daemon dispatch (hasted)

- [ ] 7.1 Add the human-only protocol requests `SendPreview` and `Send { acknowledge_stale, recipients_digest }` to haste-proto and deny them for agent and machine principals in the gate and in `is_human_only`
- [ ] 7.2 Implement `SendPreview`: resolve recipients (explicit `to`, else thread reply target), validate, return source, recipients, subject, body length, stale flag and sends remaining, changing no state
- [ ] 7.3 Rewrite `send` in dispatch.rs to follow the `execute_reminder` pattern: `begin_send`, recompute recipients and compare digest, call the guarded connector once, classify with `SendOutcome`, then `finish_send`, `fail_send_no_effect` or `mark_send_needs_check`
- [ ] 7.4 Map refusals (`kill switch engaged`, `connector stopped by circuit breaker`, `send refused`, `recipients_changed`, rate limit) to distinct error codes with no addresses in the message
- [ ] 7.5 Add audit entries `proposal.send` (sent, refused, needs_check, failed) and `send.copy_failed` with ids and counts only
- [ ] 7.6 Publish `ProposalChanged` on each transition and a notice on needs-check
- [ ] 7.7 Ensure startup leaves `executing` proposals untouched (send included) and reports them as needing a check
- [ ] 7.8 Dispatch tests with fake connectors: one connector call per send, no call on any refusal before begin, ambiguous leaves executing, agent denied for preview and send, digest mismatch refused

## 8. Approvals TUI (haste-tui)

- [ ] 8.1 Show the send confirmation with every recipient, the source and the subject, and the digest; require `y`
- [ ] 8.2 Handle `recipients_changed` by reloading the preview without sending
- [ ] 8.3 Show `executing` send proposals under needs-check with the outcome label and offer only "mark sent" and "return to approved", each with a confirmation
- [ ] 8.4 Snapshot or unit tests for the confirmation text and the needs-check row, asserting no body text is rendered in the list

## 9. Cross-cutting tests and audit hygiene

- [ ] 9.1 Add an end-to-end test over the daemon dispatch with the Gmail and iCloud fakes: approve, preview, send, and assert one submission and one audit `sent`
- [ ] 9.2 Add a scan test: distinctive address and subject markers never appear in audit output or captured logs for success, refusal and ambiguous outcomes
- [ ] 9.3 Extend the exhaustive per-principal test so `Send` and `SendPreview` are denied for every agent and machine principal
- [ ] 9.4 Add a test that the agent CLI and TUI crates do not link `lettre`, `native-tls` or any new SMTP crate (`cargo tree` check in the test script)

## 10. Documentation

- [ ] 10.1 Update the README Gmail section: human send, the send method, the confirmation, and that an ambiguous send waits for a check
- [ ] 10.2 Update the README iCloud section: SMTP on 587, the Sent copy, `sent_folder` setting, and the single-attempt rule
- [ ] 10.3 Update the README Policy table and the Spec coverage notes for send
- [ ] 10.4 Update the Gmail and iCloud manual smoke checks to match the new send path

## 11. Verification and manual smoke (human)

- [ ] 11.1 Run `cargo test --workspace`, `cargo clippy --workspace`, and the `cargo tree` checks from the README coverage notes; record results
- [ ] 11.2 Smoke Gmail: approve a draft proposal to the human's address, confirm the draft appears, send it from the TUI, verify the message arrived and the proposal shows `sent` with the message id
- [ ] 11.3 Smoke iCloud: approve and send to the human's own address, verify arrival and the copy in Sent Messages, and that the sent copy is not duplicated
- [ ] 11.4 Smoke ambiguous path: with the network disabled after confirmation, verify the proposal stays `executing`, is listed as needing a check, and that no second send occurs when the network returns
- [ ] 11.5 Smoke kill switch: engage the connector and confirm a send is refused before any connection; release and confirm the same proposal can be sent once
- [ ] 11.6 Smoke agent denial: from an agent token, attempt a send request and confirm `denied` and an audit line with no addresses
