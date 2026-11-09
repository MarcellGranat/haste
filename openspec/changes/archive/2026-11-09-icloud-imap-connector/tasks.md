## 1. Crate and configuration

- [x] 1.1 Create `crates/haste-connector-icloud` with a `Connector` impl for `Source::Icloud` declaring only `read = true`
- [x] 1.2 Add `icloud.json` settings type (address, host, port, user-specified folders with optional per-folder backfill, global backfill count) with `0600` writer and no secret fields
- [x] 1.3 Add Keychain read/write for service `com.haste.icloud` using the same binding as the Gmail connector
- [x] 1.4 Add the `icloud` cargo feature to `hasted`, off by default (the `haste` verb belongs to the `human` build, like Gmail's)

## 2. IMAP client

- [x] 2.1 Wrap the IMAP client with implicit TLS to `imap.mail.me.com:993` using `native-tls`, certificate verification on
- [x] 2.2 Run every IMAP call under `spawn_blocking` with socket timeouts
- [x] 2.3 Classify errors into authentication (terminal), transient (cycle budget) and protocol (logged) categories
- [x] 2.4 Expose only read operations: LOGIN, LOGOUT, EXAMINE, UID SEARCH, UID FETCH with `BODY.PEEK`

## 3. Sync and cursor

- [x] 3.1 Define the `{uidvalidity, last_uid}` per-folder cursor and serialise it into the connector `Cursor`
- [x] 3.2 Implement delta fetch of UIDs above `last_uid` when `UIDVALIDITY` matches
- [x] 3.3 Implement the bounded backfill (user-specified size, default 200 newest UIDs) for first sync and for `UIDVALIDITY` changes, resetting only that folder; skip a missing folder without stopping the others
- [x] 3.4 Write an audit entry per cycle with counts and any cursor reset, no subjects or bodies

## 4. Mapping into the data model

- [x] 4.1 Parse RFC 5322 messages with `mail-parser`, keeping plain text only and recording attachment metadata
- [x] 4.2 Build thread grouping from `In-Reply-To`/`References`, falling back to normalised subject within the folder
- [x] 4.3 Map participants to `Person` and write `Message` and `Thread{source=icloud}` through the store upsert on source id
- [x] 4.4 Confirm re-syncing the same UID range yields identical thread and message ids
- [x] 4.5 Record the `text/html` part from BODYSTRUCTURE and fetch it (bounded, `BODY.PEEK`) only when a message has no `text/plain` part
- [x] 4.6 Reduce the HTML to text locally (markup, scripts and styles removed, entities decoded) and store only the text
- [x] 4.7 Tests: plain wins over HTML, HTML-only becomes text, scripts/styles/tracking pixels leave no trace, neither part gives the unavailable marker, and no request to an address inside the HTML

## 5. Auth verb and administration

- [x] 5.1 Add `haste connector icloud auth` (human build only; flags `--address`, repeatable `--folder`, `--backfill`): no-echo password prompt, LOGIN and SELECT INBOX before writing the Keychain item
- [x] 5.2 Ensure `connector status`, `release icloud` and `engage icloud` work for the new source without code specific to it
- [x] 5.3 Ensure the agent build does not list the `auth` subcommand

## 6. Scheduler and failure handling

- [x] 6.1 Register the connector in the `hasted` scheduler behind the kill switch and circuit breaker, default interval 5 minutes
- [x] 6.2 On authentication failure, engage the kill switch, notify the human to renew the app-specific password, and do not retry
- [x] 6.3 Verify transient errors end the cycle within the attempt budget and never loop

## 7. Tests

- [x] 7.1 Unit tests for the cursor logic: delta, first sync, `UIDVALIDITY` change affecting one folder only
- [x] 7.2 Unit tests for the mapper: reply threading, subject fallback, HTML not fetched, attachment metadata only
- [x] 7.3 Test that no mutating IMAP command is ever constructed (STORE, APPEND, COPY, MOVE, EXPUNGE)
- [x] 7.4 Test that audit lines and error strings never contain the password fixture or message text
- [x] 7.5 Integration test against a local fake IMAP server covering auth failure, delta sync and idempotent re-sync

## 8. Verification and docs

- [x] 8.1 Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`, and `cargo test -p haste --features human`
- [x] 8.2 Add an iCloud section to `README.md` covering app-specific password creation, the auth verb, and a manual smoke check
- [x] 8.3 Manual smoke check against a live account: auth, delta after new mail, kill switch engage and release
