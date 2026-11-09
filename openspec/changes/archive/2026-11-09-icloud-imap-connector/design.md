## Context

The core (`core-policy-gate`) defines the `Connector` trait, the per-connector kill switch, the circuit breaker, the store, and the audit log. `gmail-connector` is the first real connector and sets the pattern: a separate `haste-connector-<name>` crate, a human-only auth verb, a non-secret JSON settings file in the data directory, a Keychain item for the secret, and scheduler wiring in `hasted` behind a cargo feature.

iCloud Mail offers IMAP only (`imap.mail.me.com:993`, implicit TLS). Authentication requires an Apple ID plus an app-specific password generated at appleid.apple.com; the main account password is refused. IMAP gives UIDs per folder, and the UID is only meaningful together with `UIDVALIDITY`. The connector must stay read-only: haste never writes to the user's mailbox.

## Goals / Non-Goals

**Goals:**
- Read-only sync of the iCloud folders the user lists (INBOX by default) into `Thread{source=icloud}` and `Message`.
- Resumable delta sync that survives daemon restarts and `UIDVALIDITY` changes without loops.
- The app-specific password stays in the Keychain and is read only by the daemon.
- Authentication failure is terminal (kill switch engaged, human notified, no retry), matching the Gmail `invalid_grant` rule.

**Non-Goals:**
- Any write to the mailbox (APPEND, STORE, COPY, MOVE, EXPUNGE) and any SMTP use.
- IDLE push; the scheduler polls.
- Attachment bodies; only attachment metadata is stored.
- Multiple accounts, or the main Apple ID password.

## Decisions

**Protocol client: `imap` crate (sync, native-tls) run inside a dedicated blocking task.** The daemon is tokio-based, but IMAP sessions here are short, sequential polls. A blocking client on `tokio::task::spawn_blocking` avoids pulling an async IMAP stack and keeps the TLS surface to one implementation. Alternative considered: `async-imap` with `tokio-rustls` — more moving parts for a poll every few minutes; rejected for now and can be swapped behind the client wrapper without touching the mapper.

**TLS: `native-tls` (Security.framework on macOS), not rustls.** It uses the system trust store and matches the macOS-only deployment. Certificate verification stays on; no `danger_accept_invalid_certs`. Alternative: rustls with bundled roots — rejected to avoid a second trust store.

**Credentials: Keychain item `com.haste.icloud`, account = Apple ID address, via `security-framework`** (the same binding the Gmail connector uses). `icloud.json` in the data directory holds only the address, host, port and folder list, mode `0600`. The password is never written to `icloud.json`, the store, the audit log, or the logs. The `auth` verb reads the password with no echo (`rpassword`-style terminal read) and runs LOGIN plus SELECT INBOX before writing the Keychain item, so a typo never lands in the Keychain.

**Sync cursor: per folder, `{uidvalidity, last_uid}`, serialised into the connector `Cursor`.** Each cycle: SELECT folder (read-only `EXAMINE` is preferred so nothing can change flags), compare `UIDVALIDITY`. If it matches, `UID FETCH last_uid+1:*` for new messages. If it changed, discard the cursor for that folder and run one bounded backfill (newest N UIDs; N is the folder's `backfill`, else the global `backfill_count`, default 200) — it does not loop, and the audit log records the reset. The first run is the same bounded backfill. Messages are fetched with `BODY.PEEK[]` (or `BODY.PEEK[HEADER]` + `BODY.PEEK[TEXT]`) so `\Seen` is never set.

**Envelope and body parsing: `mail-parser`.** It handles RFC 5322 multipart, charset decoding and `References`/`In-Reply-To`. Plain-text body is preferred; HTML is reduced to text, never rendered. Attachments contribute metadata (name, type, size) only.

**Thread grouping:** `In-Reply-To`/`References` first, then normalised `Subject` (strip `Re:`/`Fwd:`, lowercase) within the same folder and a time window. Grouping is deterministic so that re-syncing the same UID range produces the same thread IDs (idempotent writes via the store's existing upsert on `(source, source_id)`).

**Failure handling:** errors are classified. `NO`/`BAD` on LOGIN, or Apple's app-password-required response, is terminal: engage the kill switch and notify the human, no retry. Connection, timeout and `BYE` are transient and use the per-cycle attempt budget; after the budget the cycle ends and the next scheduler tick tries again. The circuit breaker from the core applies unchanged.

**Capability declaration:** `read = true`, `draft = false`, `send = false`, `archive = false`. The gate matrix in the core already denies anything else for this source.

**Scheduling:** registered in `hasted` behind the `icloud` cargo feature, like `gmail`. Default interval 5 minutes, with the existing kill switch checked before every cycle.

## Risks / Trade-offs

- [Apple changes IMAP behaviour or rate limits] → sync is polled slowly, per-cycle budget, circuit breaker on repeated failure; the audit log shows the failures.
- [Mis-handled UIDVALIDITY causes duplicate or missing mail] → cursor keyed by `(folder, uidvalidity)`; a change resets only that folder; idempotent upsert on the source id makes re-sync safe.
- [Password leaks into logs or audit] → the password type is never `Debug`-printed; audit records counts and ids only; a unit test asserts the audit line does not contain the password fixture.
- [Blocking IMAP inside the async daemon starves the runtime] → all IMAP calls run under `spawn_blocking` with a socket timeout.
- [`native-tls` build friction on CI] → macOS is the only target; Linux builds of the connector are not required.
- [Body HTML tracking pixels or remote content] → HTML is never fetched or rendered; only text is stored.

## Migration Plan

1. Land the crate and the `icloud` feature off by default; no behaviour change for existing installs.
2. Human runs `haste connector icloud auth` once, then `launchctl kickstart -k gui/$(id -u)/com.haste.hasted`.
3. Rollback: `haste connector engage icloud` stops the source at once (cursor kept); removing the Keychain item `com.haste.icloud` and the `icloud.json` file fully disables it. Store rows from this source stay in place, as with Gmail rollback.

**Settings are user-specified.** The human chooses which folders to sync and how far back the first sync goes, both in `icloud.json` (and through flags on the `auth` verb): `folders` is a list of `{ "name": "INBOX", "backfill": 500 }` entries (INBOX only when unset), `backfill_count` is the global default (200), and a folder's own `backfill` overrides it. A folder that does not exist on the server is skipped with an audited error and never stops the other folders.

## Implementation Notes

Where the build refined or departed from the plan above:

- **Fetch plan uses BODYSTRUCTURE.** A message is fetched in two steps: `BODYSTRUCTURE` plus `BODY.PEEK[HEADER]`, then only the first `text/plain` part with a partial `BODY.PEEK[n]<0.N>` (at most 64 KiB). That is how attachment bodies and HTML are never requested; `BODY.PEEK[]` of the whole message would have downloaded them.
- **No `text/plain` part:** the `text/html` part is fetched (`BODY.PEEK`, at most 256 KiB of the encoded part) and reduced to text locally by the MIME parser's HTML-to-text conversion; the HTML itself is never stored or rendered and nothing it references is requested. The body is stored as `[body unavailable]` only when there is neither part. (First version stored `[body unavailable]` for every HTML-only mail; a live check showed that was 18 of 21 messages, so this was changed.) Attachments are appended to `body_text` as `[attachment: name (type, N bytes)]` lines, because `Message` has no attachment field.
- **Participants stay address strings,** as with Gmail; creating `Person` entities belongs to the identity-resolution change.
- **Message ids** are the `Message-ID` header (stable across a `UIDVALIDITY` reset), falling back to `folder:uidvalidity:uid`. **Thread keys** come from the oldest `References` entry, else `In-Reply-To` (resolved inside the batch), else a header-less `Re:` joins the same-folder subject thread. A reply whose parent is in an earlier sync and that carries only `In-Reply-To` can start a new thread; real mail clients send `References`.
- **Delta batches** hold only the newest messages of a thread, so the daemon's batch store now merges: it keeps the stored thread's subject and the union of participants (the Gmail connector, which returns whole threads, is unaffected).
- **Reporting:** the connector takes a `CycleAudit` callback; the daemon turns it into one `icloud.sync` audit entry per cycle with folder names and counts only.
- **Auth helper:** like Gmail, the password prompt and validation run in a separate binary (`haste-icloud-auth`) so the `haste` CLI links no IMAP or Keychain code. `haste` has no `icloud` cargo feature; the verb belongs to the `human` build, like Gmail's. `hasted` has the `icloud` feature, off by default.

## Open Questions

None. Which folders to sync and the backfill size are user settings (see above).
