# connector-icloud-imap Specification

## Purpose
Read-only iCloud Mail sync over IMAP, authenticated with an app-specific password stored in the macOS Keychain.

## Requirements

### Requirement: Connector declares read-only capabilities
The iCloud connector SHALL declare the capabilities `read = true`, `draft = false`, `send = false` and `archive = false` for `Source::Icloud`. It SHALL NOT issue any IMAP command that changes mailbox state (APPEND, STORE, COPY, MOVE, EXPUNGE, CREATE, DELETE, RENAME).

#### Scenario: Capability declaration
- **WHEN** the daemon registers the iCloud connector
- **THEN** the declared capabilities are exactly `read` and nothing else

#### Scenario: No mailbox writes
- **WHEN** a sync cycle runs against a mailbox with new and unread messages
- **THEN** no IMAP command other than LOGIN, LOGOUT, EXAMINE/SELECT, UID SEARCH and UID FETCH is sent, and no message flag changes

### Requirement: Authentication with an app-specific password in the Keychain
The connector SHALL connect to `imap.mail.me.com` on port 993 over implicit TLS with certificate verification enabled, and SHALL authenticate with the Apple ID address and an app-specific password. The password SHALL be stored only in the macOS Keychain under service `com.haste.icloud`. The non-secret settings (address, host, port, the user's folder list and backfill sizes) SHALL be stored in `icloud.json` in the data directory with mode `0600`, and that file SHALL NOT contain the password.

#### Scenario: Password never written to disk outside the Keychain
- **WHEN** `haste connector icloud auth` completes successfully
- **THEN** `icloud.json` contains no password or secret field, and the Keychain item `com.haste.icloud` holds the app-specific password

#### Scenario: Certificate verification
- **WHEN** the server presents a certificate that does not validate
- **THEN** the connection is refused and the cycle is treated as a transient error

### Requirement: Human-only auth verb validates before storing
The `haste connector icloud auth` verb SHALL be available only in the human build. It SHALL read the app-specific password without echoing it, SHALL run LOGIN and SELECT INBOX against the server, and SHALL write the Keychain item only if both succeed.

#### Scenario: Successful authorization
- **WHEN** the human runs `haste connector icloud auth` with a valid address and app-specific password
- **THEN** the command prints the address, the Keychain item is written, and `haste connector status` lists `icloud`

#### Scenario: Failed validation leaves nothing stored
- **WHEN** LOGIN is rejected during `haste connector icloud auth`
- **THEN** the command exits with an error and no Keychain item or `icloud.json` change is written

#### Scenario: Agent build has no auth verb
- **WHEN** an agent runs `haste connector icloud --help` on the agent build
- **THEN** the `auth` subcommand is not listed

### Requirement: Incremental UID-based sync with UIDVALIDITY handling
The connector SHALL sync each configured folder (INBOX by default) using a cursor of `{uidvalidity, last_uid}` per folder, stored as the connector `Cursor`. When `UIDVALIDITY` matches the stored value, the connector SHALL fetch only UIDs greater than `last_uid`. When `UIDVALIDITY` differs, the connector SHALL discard that folder's cursor and perform one bounded backfill.

#### Scenario: First sync is bounded
- **WHEN** the connector syncs a folder for the first time
- **THEN** it fetches at most the configured backfill count of the newest UIDs: the folder's own `backfill` if set, else the global `backfill_count`, else 200

#### Scenario: User-chosen folders
- **WHEN** `icloud.json` lists the folders `INBOX` and `Sent Messages`
- **THEN** both are synced, each with its own cursor, and no other folder is read

#### Scenario: Missing folder does not stop the others
- **WHEN** a listed folder does not exist on the server
- **THEN** that folder is skipped, the cycle's audit entry records the error by folder name, and the other folders still sync

#### Scenario: Delta sync after restart
- **WHEN** the daemon restarts and the folder's `UIDVALIDITY` is unchanged
- **THEN** only messages with UIDs greater than the stored `last_uid` are fetched

#### Scenario: UIDVALIDITY change resets one folder only
- **WHEN** the server reports a different `UIDVALIDITY` for a folder
- **THEN** that folder's cursor is reset, one bounded backfill runs, the audit log records the reset, and other folders keep their cursors

#### Scenario: Re-sync is idempotent
- **WHEN** the same UID range is fetched twice
- **THEN** the store contains one `Message` per source id and the same thread identifiers as before

### Requirement: Read messages without marking them seen
The connector SHALL fetch message bodies with `BODY.PEEK` so that the `\Seen` flag is never set by haste.

#### Scenario: Unread message stays unread
- **WHEN** an unread message is synced
- **THEN** after the cycle the message still has no `\Seen` flag on the server

### Requirement: Mapping into the unified data model
The connector SHALL map each message into `Message` with `source = icloud` and into a `Thread{source=icloud}`. Threads SHALL be grouped by `In-Reply-To`/`References`, falling back to the normalised subject within the same folder. Participants SHALL be mapped to `Person`. The `text/plain` part SHALL be preferred. When a message has no `text/plain` part, its `text/html` part SHALL be fetched (bounded, with `BODY.PEEK`) and reduced to text locally; HTML SHALL never be rendered, and no resource it references SHALL be requested. Only the reduced text is stored. Attachments SHALL be recorded as metadata (name, content type, size) only.

#### Scenario: Reply joins its thread
- **WHEN** a message whose `In-Reply-To` matches an already-stored message is synced
- **THEN** it is attached to that message's thread

#### Scenario: Attachment content is not downloaded
- **WHEN** a message with an attachment is synced
- **THEN** the attachment metadata is stored and no attachment body is requested from the server

#### Scenario: Plain text wins over HTML
- **WHEN** a message has both a `text/plain` and a `text/html` part
- **THEN** only the `text/plain` part is fetched

#### Scenario: HTML-only message becomes text
- **WHEN** a message has only a `text/html` part
- **THEN** that part is fetched once with `BODY.PEEK`, markup, scripts and styles are removed, and the remaining text is stored

#### Scenario: Remote content is never requested
- **WHEN** a message's HTML references remote images or tracking pixels
- **THEN** no request is made to any such address and no HTML is stored

#### Scenario: Neither part available
- **WHEN** a message has no `text/plain` and no `text/html` part
- **THEN** the message is stored with the body marked unavailable

### Requirement: Authentication failure is terminal
An authentication failure (a `NO` or `BAD` response to LOGIN, or an Apple response that requires an app-specific password) SHALL engage the connector's kill switch, notify the human that the app-specific password must be renewed, and SHALL NOT be retried automatically. Only a human release (`haste connector release icloud`) resumes the connector.

#### Scenario: Revoked app-specific password
- **WHEN** a sync cycle's LOGIN is rejected as an authentication failure
- **THEN** the kill switch for `icloud` is engaged, the human is notified, and no further LOGIN is attempted until the human releases the connector

#### Scenario: Release after re-authorization
- **WHEN** the human runs `haste connector icloud auth` with a new app-specific password and then `haste connector release icloud`
- **THEN** the next cycle syncs from the saved cursor

### Requirement: Transient errors use the cycle budget
Network errors, timeouts and unexpected server `BYE` responses SHALL be treated as transient. The connector SHALL retry only within the per-cycle attempt budget and SHALL end the cycle when the budget is exhausted. The circuit breaker SHALL apply unchanged.

#### Scenario: Timeout within budget
- **WHEN** a connection times out on the first attempt and succeeds on the second within the budget
- **THEN** the cycle completes normally and one audit entry is written for the cycle

#### Scenario: Budget exhausted
- **WHEN** every attempt in a cycle fails transiently
- **THEN** the cycle ends without a kill switch engagement and the next scheduler tick tries again

### Requirement: Audit and secret hygiene
The connector SHALL write one audit entry per sync cycle containing the source, folder names, counts of new messages and threads, and any cursor reset. Audit entries, logs and error messages SHALL NOT contain the app-specific password, message subjects, or message bodies.

#### Scenario: Cycle audit entry
- **WHEN** a sync cycle finishes
- **THEN** the audit log gains one entry with source `icloud` and counts, and no subject or body text

#### Scenario: Password absent from errors
- **WHEN** LOGIN fails and the error is recorded
- **THEN** the recorded error text does not contain the password value
