## Why

Haste can read Gmail and iCloud Mail and can turn an agent's request into a plain-text draft after a human approves it, but the human still has to leave haste and press send in Gmail or Mail.app. The approval model already has a human-only `send` action, a TUI confirmation and a connector `send` method, yet no real connector implements sending. The user has decided that the human may send mail from haste and that agents never may. This change makes the human send path real for Gmail and iCloud, with safety rules built for a non-idempotent action: at most once, never retried automatically, and ambiguous outcomes held for a human.

## What Changes

- **Gmail human send**: an approved mail proposal can be sent through the Gmail API by an explicit human action. The send uses only scopes already granted (`gmail.readonly`, `gmail.compose`); no scope is added. The send method (`users.drafts.send` or `users.messages.send`) is chosen in `design.md` after a spike.
- **iCloud human send over SMTP**: the iCloud connector submits a composed plain-text RFC 5322 message to `smtp.mail.me.com:587` with STARTTLS and certificate verification, authenticates with the existing Keychain app-specific password, and then appends a copy to the Sent folder over IMAP. SMTP stays inside the iCloud connector crate; the agent CLI and TUI gain no new dependency.
- **Send lifecycle**: a send moves `approved -> executing -> sent`. An outcome that is certainly without effect (refused before the message body was accepted) returns the proposal to `approved`. An ambiguous outcome (timeout, dropped connection, or a failure after the body was transmitted) leaves it in `executing`, shown to the human as needing a check. Nothing re-sends automatically, and no path goes from `executing` back to `approved` for an ambiguous outcome.
- **Recipients confirmed before sending**: the TUI shows the full recipient list, the source and the subject before the human confirms. The recipient list is resolved once, stored on the proposal at confirmation, and the connector sends exactly that list. Any change in the resolved list between preview and send refuses the send.
- **Refusals**: CR or LF in any address or in the subject refuses the send (no silent stripping for sends); recipient count, body size and sends per hour are bounded by named constants; Bcc and attachments are not supported.
- **Safety**: no auto-send path exists. Agents still cannot send (the gate keeps `Action::Send` human-only). The per-connector kill switch and circuit breaker apply to sends, and a send never retries.
- **Audit**: audit entries carry proposal ids, provider message ids and counts only, never addresses, subjects or bodies.
- **Spikes and smoke tests**: upfront human spike tasks (Gmail OAuth authorization, iCloud app-specific password, SMTP login check, self-addressed send) and a final manual smoke group. Tests use fakes only: a local fake SMTP server, the existing fake IMAP server extended with APPEND, and the local fake Gmail server extended with the send endpoint.

**BREAKING**: the `approved -> sent` transition used for sends is removed; every send passes through `executing`. The `send` capability flag becomes true for Gmail and iCloud, but only the human send path consults it; agent-originated requests never reach a method that does.

## Capabilities

### New Capabilities
- `email-send`: the human-only send action shared by connectors (recipient resolution and confirmation, header and size refusal rules, sends-per-hour limit, at-most-once and ambiguous-outcome rules, audit content).
- `connector-icloud-smtp`: SMTP submission over STARTTLS with the Keychain password, the plain-text message format, and the IMAP APPEND copy to the Sent folder.

### Modified Capabilities
- `connector-gmail`: the capability declaration gains `send = true` for the human path, the "never send" requirement is replaced by the human-only send requirement, and the Gmail send method and its error classification are specified.
- `connector-icloud-imap`: the read-only capability declaration and the "no mailbox writes" rule gain the single exception of the Sent-copy APPEND issued by the send path, and the authentication rule covers SMTP as well.
- `proposals`: the lifecycle transitions for sends (`approved -> executing -> sent`, ambiguous outcomes held in `executing`, no direct `approved -> sent`), and the startup rule for `executing` proposals applies to sends too.
- `approvals-tui`: the send confirmation lists every recipient, and an `executing` send is shown as needing a check with the human choosing `sent` or `approved` after verifying in the mail app.
- `connector-framework`: the `send` trait method is default-unsupported, and the guard performs exactly one attempt for sends.

## Impact

- Code: `crates/haste-core` (send types, limits, proposal transitions), `crates/haste-store` (`begin_send`, send timestamps for the rate limit, recipient storage), `crates/hasted` (send dispatch, preview, outcome classification), `crates/haste-tui` (recipient confirmation, needs-check actions), `crates/haste-connector-gmail` (send method, MIME for send, fake server endpoint), `crates/haste-connector-icloud` (SMTP client, Sent APPEND, config field for the Sent folder name).
- Dependencies: the SMTP client decision is recorded in `design.md`. Default plan is a hand-written minimal SMTP client over the existing `native-tls` stack, adding no new crate; `lettre` is the alternative. Either way the dependency is confined to `haste-connector-icloud`.
- Systems: Gmail requires no new consent; the existing app-specific password is reused for SMTP. Live sends go to real mailboxes, so the smoke group is manual.
- Docs: README Gmail and iCloud sections and the Policy table change to describe the human send path.
- Audit: new actions `proposal.send`, `send`, `send.ambiguous` and `send.copy_failed` with ids and counts only.
