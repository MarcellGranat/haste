## Why

Haste now has a core (`core-policy-gate`) and a live Gmail connector (`gmail-connector`), but iCloud Mail is the second source the personal inbox needs, and it is reachable only through IMAP. Adding it proves that the connector framework (`Connector` trait, guard, kill switch, circuit breaker) works for a second, non-OAuth provider, and gives the unified inbox its first non-Google source.

## What Changes

- Add a `haste-connector-icloud` crate that implements the existing `Connector` trait for `Source::Icloud`.
- Connect to `imap.mail.me.com:993` over implicit TLS and authenticate with the Apple ID and an app-specific password. The password is stored in the macOS Keychain (service `com.haste.icloud`), never in the SQLite store, the audit log, or a config file. The non-secret settings (address, host) live in `icloud.json` in the data directory with mode `0600`, mirroring `gmail.json`.
- Add a human-only `haste connector icloud auth` verb (human build) that prompts for the address and app-specific password (no echo), validates them with a LOGIN and SELECT, and writes the Keychain item.
- Add incremental read-only sync of the INBOX (and any folders the human lists in `icloud.json`): an initial bounded backfill, then delta sync using `UIDVALIDITY` plus `UIDNEXT`/last-seen UID as the connector `Cursor`. A changed `UIDVALIDITY` triggers a full bounded re-sync for that folder, not an unbounded loop.
- Map IMAP envelopes and bodies (`BODY.PEEK[]`, so the `\Seen` flag is never changed) into the existing `Thread{source=icloud}` and `Message` model. Threads are built from `References`/`In-Reply-To`, falling back to normalised `Subject`, and participants are mapped to `Person`.
- Declare capabilities `read = true`, `draft = false`, `send = false`, `archive = false`. The connector never writes to the mailbox: no APPEND, no STORE, no COPY/MOVE, no EXPUNGE.
- Wire the connector into the daemon scheduler behind the existing per-connector kill switch and circuit breaker. An authentication failure (`NO`/`BAD` on LOGIN, or an Apple "app-specific password required" response) is terminal: engage the kill switch, notify the human that the app-specific password must be renewed, and never retry. Network and transient errors use the per-cycle attempt budget.
- Add an audit entry for every sync cycle, with counts only (no subjects or bodies).

### Non-goals

- Sending mail (SMTP is not touched), drafting, archiving, flagging, moving or deleting messages in iCloud.
- Using the Apple ID main password, OAuth or CardDAV/CalDAV.
- Attachment bodies: attachment metadata is stored, the content is not downloaded.
- IDLE push notifications; sync is polled by the scheduler.
- Multiple iCloud accounts.

## Capabilities

### New Capabilities
- `connector-icloud-imap`: IMAP over implicit TLS with an app-specific password held in the Keychain, read-only UID-based delta sync into the data model, the capability declaration (`read` only), the human-only `connector icloud auth` verb, and terminal handling of authentication failures.

### Modified Capabilities
<!-- None: the connector framework and kill-switch administration are reused as they are. If implementation shows the Connector trait must change, that becomes a delta spec for connector-framework. -->

## Impact

- **New code**: `crates/haste-connector-icloud` (IMAP client wrapper, Keychain binding, sync and envelope mapper); wiring in `crates/hasted` (scheduler registration behind the `icloud` feature, like `gmail`); a human-only `icloud` verb in `crates/haste` (human build) and an optional TUI connectors row.
- **Dependencies**: an IMAP client with TLS (for example `imap` with `native-tls`, or `async-imap` with `tokio-rustls`), `mail-parser` for RFC 5322 parsing, and the Keychain binding already used by the Gmail connector (`security-framework`). Exact crates are chosen in `design.md`.
- **External systems**: iCloud IMAP at `imap.mail.me.com:993` (read-only). This is the second change that contacts an external service.
- **Filesystem**: `icloud.json` in the data directory (no secrets); no new files in the notes root. The app-specific password exists only in the Keychain.
- **Security surface**: the daemon is the only process that reads the Keychain item; the connector cannot write to the mailbox; agents still cannot send, and no iCloud action reaches an agent beyond `read`.
