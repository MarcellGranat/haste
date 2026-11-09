## Context

Today the send path exists only as plumbing. `hasted` `send` (dispatch.rs) calls `prepare_send`, which checks `approved -> sent` and the stale flag but never stores an intermediate state, then calls `Guarded::send`. Both connectors return `ConnectorError::Other("sending is not supported ...")`, so a send attempt leaves the proposal `approved`. Gmail creates plain-text drafts through `users.drafts.create` on approval. iCloud is read-only IMAP (`imap` 2.4 over `native-tls`). The reminders work already defines the pattern for non-idempotent writes: `approved -> executing`, one external call, `sent` on success, `approved` only when the call certainly had no effect, and `executing` with a human check otherwise (`execute_reminder` and `certainly_no_effect` in dispatch.rs; `begin_execution`, `fail_execution` in haste-store reminders.rs). Sends follow the same pattern with stricter rules because a delivered mail cannot be recalled.

## Goals / Non-Goals

**Goals**
- A human can send an approved mail proposal from Gmail and from iCloud, from haste.
- At most one send per proposal, ever; no automatic retry at any layer.
- The recipients the human confirmed are the recipients that receive the mail.
- Agents keep no send path; the gate and the proposal state machine stay as they are.
- Audit and logs never hold addresses, subjects or bodies.

**Non-Goals**
- Attachments, HTML bodies, Bcc, Cc editing from the TUI (see open questions), scheduled sends, send-undo, sending from any address other than the connector's own account.
- Changing `note.append`, reminders or calendar behavior.

## Decisions

### D1. Send lifecycle uses `executing` for every send
`approved -> executing` happens in one store transaction (`begin_send`) that also runs the stale check and the rate-limit check and freezes the resolved recipients (D3). Only then does the connector run. Outcomes:
- success: `executing -> sent`, result `sent:<provider id>` (Gmail message id or iCloud SMTP queue id if available, otherwise `sent:smtp`), audit `proposal.send` `sent`.
- certainly no effect (see D5 table): `executing -> approved` with the reason recorded on the audit line, error returned to the TUI.
- ambiguous: stays `executing`, audit `proposal.send` `needs_check` with detail `outcome_unknown`; the TUI shows it under needs-check (approvals-tui delta).
- Sent but the iCloud Sent-copy APPEND failed: the proposal becomes `sent` (the mail was delivered), result `sent:smtp;copy_failed`, audit `send.copy_failed`. No retry.

The `approved -> sent` transition is removed from `transition()`, so a proposal cannot reach `sent` without passing `executing`. A unit test asserts it.

### D2. Preview and confirmation
Two human-only requests, both denied to agent principals:
- `SendPreview { proposal_id }`: read-only. Returns the source, the resolved recipient list (one entry for v1), the subject, body byte length, the stale flag and the sends remaining this hour. It does not change state.
- `Send { proposal_id, acknowledge_stale, recipients_digest }`: `recipients_digest` is a SHA-256 over the sorted resolved recipient list that the TUI displayed (sha2 is already a workspace dependency). If the daemon's fresh resolution differs, it refuses with `recipients_changed`, changes nothing, and the TUI reloads the preview.

The confirmation text names every recipient (not only the first), the source and the subject. Sending requires `y` after the list is shown.

### D3. Recipient resolution
- Explicit `to` on the proposal payload wins.
- Otherwise the reply target is the thread's last inbound message, taking `Reply-To` if known, else `From`, using the same rule as the Gmail draft path (`fallback_to`). For iCloud the target comes from the stored message participants (the store has no raw headers; see open question Q4).
- Exactly one recipient in v1. More than `MAX_RECIPIENTS` (constant, 1 in v1) is refused. Each address is validated to contain no CR, LF, NUL, comma, semicolon or whitespace, and to match a minimal `local@domain` shape. Any violation refuses the send with a validation error (refused, not stripped).
- The resolved list is stored on the proposal at `begin_send` (new column `send_recipients`) so the connector cannot re-resolve differently during the call.

### D4. Limits (named constants)
Placed in `haste-core::send` so the daemon and both connectors import them:
- `MAX_RECIPIENTS: usize = 1`
- `MAX_SEND_BODY_BYTES: usize = 32 * 1024` (checked before `begin_send`; the stored read limit `MAX_BODY_BYTES` is 64 KiB and is not reused)
- `MAX_SUBJECT_BYTES: usize = 255`
- `MAX_SENDS_PER_HOUR: u32 = 10`, counted from proposals whose `send_started_at` falls in the last 3600 seconds (includes `executing`, so a crash cannot reset the count)
- `SEND_TIMEOUT: Duration = 30 s` per network operation (SMTP reuses the existing socket timeout pattern)

Kill switch engagement on release behaves like other connectors: a released connector is not re-engaged by the send path; the breaker engages it on `Checkpoint`/`Challenge` from a send (D5).

### D5. Error classification for sends
One attempt only. `Guarded::send` already makes one call; the connectors must not retry internally for sends.

| Failure point | Classification | Proposal |
|---|---|---|
| Refused before any send: address/subject/body validation, no connection | `Other` with a `send refused` prefix | `approved` |
| Connection refused, DNS failure, TLS setup or handshake failure (no credentials sent) | `Transient` | `approved` |
| Gmail HTTP 4xx (documented rejection, nothing created) and HTTP 429 | `Other` `http 4xx` | `approved` |
| SMTP `AUTH` rejected (535) | `Checkpoint` (breaker: kill switch engaged, human told to renew the password) | `approved` |
| SMTP `MAIL FROM`, `RCPT TO` rejected with 5xx before `354` | `Other` `smtp refused` | `approved` |
| Timeout, dropped connection, or any error after `354` or after the Gmail request body was written | `Transient` tagged `ambiguous` | `executing` (`needs_check`) |
| SMTP 4xx/5xx after the message body was sent (final `250` missing) | `Transient` tagged `ambiguous` | `executing` |
| Gmail HTTP 5xx | `Transient` tagged `ambiguous` (the server may have accepted) | `executing` |
| Gmail `invalid_grant` on refresh before the request | `Checkpoint` | `approved` |

The "tagged ambiguous" class is a new `SendOutcome` enum in `haste-core` (`NoEffect`, `Ambiguous`, `Sent`), so `certainly_no_effect` is not reused for sends.

### D6. Gmail send method (evaluation and recommendation)
Both candidates are covered by the `gmail.compose` scope in principle. Google's scope page describes `gmail.compose` as "Manage drafts and send emails." and does not map methods to scopes, so the claim is confirmed only by the spike (S2). `gmail.compose` is a restricted scope; for a single unverified personal project in production this is already accepted by the Gmail spec.

- **A. `users.drafts.send`** with the draft id recorded at approval (`draft:<id>` result). Pros: reuses the existing approval-time draft; no new MIME path. Cons: sends whatever the Gmail draft holds at send time, which may have been edited in Gmail after the human confirmed in haste; it fails if the approval-time draft was never created; the draft id must still exist.
- **B. `users.messages.send`** with a `raw` RFC 5322 message built from the proposal's stored body, the resolved recipient and the thread's `In-Reply-To`/`References`/`threadId`. Pros: sends exactly the bytes the human confirmed; independent of the draft. Cons: a second MIME path; the approval-time draft remains and must be cleaned up.

**Recommendation: B.** The confirmation guarantee in D2 only holds if the sent bytes are the ones displayed. Cleanup of the approval-time draft after `sent` uses `users.drafts.delete` (also covered by `gmail.compose`), best-effort, never retried, audited as a count only. Whether the draft is deleted at all is open question Q1. Option A is kept as the fallback if the spike shows `messages.send` is not allowed under `gmail.compose`.

### D7. SMTP client: hand-written over native-tls (chosen) versus lettre
- **Chosen: hand-written minimal client** in `haste-connector-icloud::smtp`. Needed commands: `EHLO`, `STARTTLS`, `EHLO`, `AUTH LOGIN` (or `AUTH PLAIN`, decided by the spike), `MAIL FROM`, `RCPT TO`, `DATA`, dot-stuffing, `QUIT`. The TLS upgrade reuses `native_tls::TlsConnector` on the same `TcpStream` after the `220` reply to `STARTTLS`, so certificate handling is identical to IMAP. No new crate.
- **Reasons**: the at-most-once rule (D5) needs to know exactly whether `354` was received and whether the body was written. A client that exposes each protocol phase makes that a plain state check. A library that returns one error for a whole send hides the phase, so every failure would have to be treated as ambiguous, which blocks the `approved` fallback for real refusals.
- **Rejected: lettre**: adds a sizeable dependency tree and its own TLS feature choice (`native-tls` or `rustls`), and its error type does not expose the DATA phase. It remains the fallback if the hand-written client fails the spike (S3) or the fake-server tests show protocol gaps.
- The fake SMTP server is also hand-written (loopback, plaintext for tests through a `PlainOpener`-style trait, as the IMAP fake does).

### D8. Message format and headers
- Plain text only: `Content-Type: text/plain; charset=UTF-8`, `Content-Transfer-Encoding: 8bit`, no multipart, no attachments.
- `From:` is the connector's configured address (iCloud requires it to match the authenticated account).
- `To:` the single resolved recipient. `Subject:` `Re: <subject>` for replies, `(no subject)` for empty subjects, and the length limit of D4.
- `In-Reply-To` and `References` from the thread's last message, as for drafts.
- `Date:` and `Message-ID:` generated by haste (`Message-ID` with the connector address domain).
- The send path refuses on CR or LF in any header value; the draft path's existing neutralising `clean()` is not used for sends (D3).

### D9. iCloud Sent copy
After `250` on the final DATA response, the connector issues `APPEND "<sent folder>" (\Seen) {n}` over a new IMAP session with the same Keychain password. The folder name comes from `icloud.json` field `sent_folder` (default `Sent Messages`). This is the only write the iCloud connector ever makes, and only inside the send path. Whether iCloud already files haste's SMTP submissions into Sent (which would make APPEND create a duplicate) is open question Q2, checked in spike S3.

### D10. Auditing
Audit actions: `proposal.send` (outcomes `sent`, `refused`, `needs_check`, `failed` with a reason label), `send` (connector-level, source and outcome), `send.ambiguous`, `send.copy_failed`. Details are fixed labels, provider message ids and counts (recipients, body bytes). Recipient addresses, subjects and bodies never reach the audit log, logs or error text returned to agents. `SendPreview` is not audited with content.

### D11. Permissions and the gate
- `Action::Send` stays human-only in the gate; agents receive `denied` for any send request regardless of payload.
- `SendPreview` and `Send` are human-only protocol requests and are absent from the agent CLI.
- The `send` capability flag is true for Gmail and iCloud. Agent paths never call `Guarded::send`; the flag is checked only inside the human send path. This resolves the brief's "flag stays false for agents" as "flag consulted only by the human path" (Q6).

### D12. Crash and restart
At daemon start, proposals in `executing` (sends included) are left untouched and listed as needing a check. Nothing is re-sent. The human resolves by marking `sent` (after confirming in the mail app or the Sent folder) or `approved` (after confirming nothing was delivered), and the second choice is audited as a human override.

## Risks / Trade-offs

- A send may be ambiguous and need manual checking -> accepted; the design trades convenience for never double-sending.
- The approval-time Gmail draft may be left behind -> cleaned up best-effort (Q1), or accepted as a visible draft.
- Hand-written SMTP may miss a server quirk -> limited to the spike and the fake-server tests; lettre is the fallback (D7).
- `MAX_SENDS_PER_HOUR` counts only haste sends; a manual send in Gmail is not counted -> accepted.

## Open Questions

- **Q1. Gmail draft cleanup**: after a successful `messages.send`, delete the approval-time draft (best-effort), or leave it for the human? Recommendation: delete.
- **Q2. iCloud Sent copy**: does `smtp.mail.me.com` already file haste's submissions into "Sent Messages"? If yes, skip the APPEND. Decided by spike S3; the default in D9 assumes no.
- **Q3. iCloud drafts folder**: does iCloud need a Drafts-folder copy as the approval-time record (analogous to Gmail drafts)? Recommendation: no; the proposal row is the record. Confirm the human agrees.
- **Q4. iCloud reply target**: the store keeps participants but no raw `Reply-To`. Options: store `Reply-To` in the message row (a data-model change), or use `From` only in v1. Decide before task 6.
- **Q5. Bcc and reply-all**: v1 is single To, no Cc, no Bcc, no reply-all. Confirm, or specify Cc editing later.
- **Q6. Send capability flag**: confirm that `send = true` on the connectors is acceptable given the brief said the flag stays false for agent paths (D11 proposes "consulted only by the human path").
- **Q7. Gmail method**: confirm `messages.send` is allowed under `gmail.compose` (spike S2); if not, fall back to option A.
- **Q8. Proposal result on send**: whether the stored result should carry the SMTP queue id (the server's `250` text may contain one, which is provider-dependent) or only `sent:smtp`.
- **Q9. Human override**: whether `executing -> approved` for a send (human says nothing was delivered) requires a typed confirmation, not just `y`.

## Spike outcomes

To be recorded by the human during spikes S1 to S4 (tasks group 1). Until then the decisions above are provisional where marked by Q.
