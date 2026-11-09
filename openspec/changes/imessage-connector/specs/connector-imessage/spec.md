## ADDED Requirements

### Requirement: iMessage connector declares read and draft only
The iMessage connector SHALL declare the capabilities `read = true`, `draft = true`, `send = false` and `archive = false` for `Source::Imessage`. It SHALL NOT implement any method that transmits a message, and it SHALL NOT write to the Messages database or its directory.

#### Scenario: Capability declaration
- **WHEN** the daemon registers the iMessage connector
- **THEN** the declared capabilities are exactly `read` and `draft`

#### Scenario: Send refused through the guard
- **WHEN** any caller invokes send on the iMessage connector through the guard
- **THEN** the guard returns a capability-missing error and writes a `refused_capability` audit entry

### Requirement: Read-only access to chat.db
The connector SHALL open `~/Library/Messages/chat.db` with a read-only SQLite handle and SHALL issue only SELECT statements against it. The connector SHALL NOT create, modify, or delete any file in the Messages directory.

#### Scenario: Read-only handle
- **WHEN** the connector opens the Messages database
- **THEN** the handle is opened read-only and any write statement fails

#### Scenario: No writes to Messages directory
- **WHEN** a sync cycle completes against a populated database
- **THEN** the modification times and contents of files in the Messages directory are unchanged by haste

### Requirement: Full Disk Access is a human precondition
When the Messages database cannot be opened because the process lacks Full Disk Access, the connector SHALL report `PermissionNeeded`, SHALL leave the kill switch engaged, SHALL NOT retry within the cycle, and SHALL show in the status view which binary needs Full Disk Access. The daemon SHALL NOT attempt to request Full Disk Access itself.

#### Scenario: Permission denied on open
- **WHEN** a sync cycle cannot open `chat.db` with a permission error
- **THEN** the cycle ends with `PermissionNeeded`, no retry occurs, the kill switch stays engaged, and the status view names the binary needing Full Disk Access

#### Scenario: Released after the grant
- **WHEN** the human grants Full Disk Access and runs `haste connector release imessage`
- **THEN** the next cycle reads the database from the stored cursor

### Requirement: Incremental sync by message ROWID
The connector SHALL sync messages with `message.ROWID` greater than the stored cursor and SHALL store the largest ROWID seen as the connector cursor. Re-reading an already-stored ROWID SHALL NOT create a duplicate `Message`.

#### Scenario: First cycle
- **WHEN** no cursor is stored and fetch is called
- **THEN** the connector reads a bounded number of the most recent messages, stores the largest ROWID as the cursor, and writes no more than the configured backfill count

#### Scenario: Delta cycle
- **WHEN** a cursor is stored and new messages exist
- **THEN** only messages with ROWID greater than the cursor are mapped

#### Scenario: Idempotent re-read
- **WHEN** the same ROWID range is read twice
- **THEN** the store holds one `Message` per source id

### Requirement: Mapping chats into threads
Each chat SHALL map to one `Thread` with `source = imessage` and `external_id` equal to the chat GUID. Each message SHALL map to one `Message` in its chat's thread. Participants SHALL come from the chat's handles and the sender handle, mapped to `Person`. Dates SHALL be converted from the Messages epoch (2001-01-01 UTC) to Unix time, with the unit confirmed against the observed schema. `is_from_me` SHALL set the message direction.

#### Scenario: Chat thread
- **WHEN** a chat with two handles and four messages is synced
- **THEN** one `Thread` with the chat GUID and four `Message` records is produced

#### Scenario: Date conversion
- **WHEN** a message with a stored Messages-epoch timestamp is mapped
- **THEN** its `sent_at` equals the Unix time for the same instant

#### Scenario: Text unavailable
- **WHEN** a message has no plain `text` value
- **THEN** the message is stored with its body marked unavailable and no attributed body is parsed in this change

### Requirement: Draft proposals are never transmitted
An iMessage draft SHALL exist only as a `Draft` proposal in the store. Approving an iMessage draft SHALL mark it "approved for manual copy" and SHALL NOT transmit it. No path in the daemon SHALL transmit an iMessage draft.

#### Scenario: Approve does not send
- **WHEN** the human approves an iMessage draft proposal
- **THEN** the proposal state becomes approved for manual copy, no network or Messages call is made, and an audit entry `draft.approve` with source `imessage` is written

#### Scenario: Pending draft creates nothing external
- **WHEN** an agent proposes an iMessage draft
- **THEN** only a stored proposal is created and nothing is written outside the store

### Requirement: Scope and project tags
iMessage threads SHALL carry `scope_tags` set by the human, using herdr workspace labels as the candidate source. Machine principals SHALL see iMessage threads only when a tag they are allowed to see is present, in the daemon read path.

#### Scenario: Machine sees only its tags
- **WHEN** a machine principal lists threads
- **THEN** iMessage threads without a tag present on that machine are absent from the result

### Requirement: Kill switch engaged on first install
The iMessage connector SHALL register with its kill switch engaged when no persisted state exists for it. Scheduled reads SHALL be refused until the human releases the connector.

#### Scenario: Fresh install
- **WHEN** the daemon starts for the first time with the iMessage connector registered
- **THEN** the kill switch is engaged and no read is performed until `haste connector release imessage`

### Requirement: Per-cycle audit with counts only
Every iMessage sync cycle SHALL write one audit entry containing the source `imessage`, the outcome, counts of new threads and messages, and any cursor reset. Audit entries, logs and errors SHALL NOT contain message text, handles, chat names, or attachment names.

#### Scenario: Cycle audit
- **WHEN** a sync cycle finishes
- **THEN** one audit entry with source `imessage` and counts is appended and no message text, handle or chat name appears in it
