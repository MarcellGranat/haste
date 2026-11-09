## ADDED Requirements

### Requirement: Gmail connector capability declaration
The Gmail connector SHALL declare the capabilities `read = true`, `draft = true`, `send = false` and `archive = false`. The connector MUST NOT expose any method that sends mail or modifies Gmail labels.

#### Scenario: Capabilities reported
- **WHEN** the daemon queries the Gmail connector capabilities
- **THEN** the result has `read` and `draft` set to true and `send` and `archive` set to false

#### Scenario: Send refused through the guard
- **WHEN** any caller invokes send on the Gmail connector through the guard
- **THEN** the guard returns a capability-missing error and writes a `refused_capability` audit entry

### Requirement: OAuth consent with minimal scopes
The system SHALL authorize the Gmail account with OAuth 2.0 using PKCE and a loopback redirect on `127.0.0.1`. The requested scopes MUST be exactly `gmail.readonly` and `gmail.compose`. The consent flow SHALL be available only in the human build of the CLI.

#### Scenario: Successful consent
- **WHEN** the human runs the Gmail auth verb and approves consent in the browser
- **THEN** the system receives the authorization code on the loopback listener, exchanges it with PKCE, and stores the refresh token in the macOS Keychain

#### Scenario: Agent build has no auth verb
- **WHEN** an agent runs the `haste` CLI built without the human feature
- **THEN** no Gmail auth verb exists and the command is rejected

#### Scenario: Production-status project avoids weekly re-auth
- **WHEN** the OAuth client belongs to a Google Cloud project with the "In production" publishing status
- **THEN** the system does not assume refresh tokens expire after 7 days and does not schedule periodic re-authorization

#### Scenario: Unverified-app warning at consent
- **WHEN** the human completes consent for the unverified production app
- **THEN** the flow proceeds after the human acknowledges Google's unverified-app warning once, and no verification is required for single-user use

#### Scenario: Redirect listener only on loopback
- **WHEN** the consent flow is running
- **THEN** the redirect listener binds to `127.0.0.1` only and closes after one response or on timeout

### Requirement: Refresh token storage in the Keychain
The refresh token SHALL be stored only in the macOS Keychain under service `com.haste.gmail`. The refresh token and access token MUST NOT be written to the SQLite store, the audit log, logs, or the notes root.

#### Scenario: Token not in store
- **WHEN** authorization completes
- **THEN** the SQLite store and audit log contain no refresh or access token material

#### Scenario: Access token refresh
- **WHEN** an API call needs an access token and none is cached in memory
- **THEN** the system refreshes it from the Keychain refresh token and keeps only the access token in memory

#### Scenario: Refresh rejected
- **WHEN** Google rejects the refresh token with an invalid-grant error
- **THEN** the connector returns a `Checkpoint` error, the guard engages the Gmail kill switch, no retry is made, and the human is notified that re-authorization is needed

### Requirement: Incremental sync with history cursor
The connector SHALL perform an initial backfill of the last `BACKFILL_DAYS` days (a compile-time constant of 30, not user-configurable), then sync changes using the Gmail history ID stored as the connector cursor. Changed threads MUST be re-fetched in full before mapping.

#### Scenario: First cycle backfill
- **WHEN** no cursor is stored and fetch is called
- **THEN** the connector lists threads from the last `BACKFILL_DAYS` days and returns them as a batch with a new cursor set to the latest history ID

#### Scenario: Steady-state delta
- **WHEN** a cursor is stored and fetch is called
- **THEN** the connector requests history since that cursor and returns only the threads that changed, each fully fetched

#### Scenario: Expired history cursor
- **WHEN** Google reports the stored history ID as no longer available
- **THEN** the connector performs a bounded backfill, resets the cursor, and does not loop

#### Scenario: Empty delta
- **WHEN** history since the cursor contains no changes
- **THEN** the connector returns an empty batch with the same cursor

### Requirement: Mapping Gmail threads into the data model
The connector SHALL map each Gmail thread to a `Thread` with `source = gmail` and `external_id` equal to the Gmail thread ID, and each message to a `Message` in that thread. Participants SHALL be taken from the `From`, `To` and `Cc` headers and deduplicated. Only `text/plain` bodies SHALL be stored, truncated to `MAX_BODY_BYTES`. Attachment content MUST NOT be downloaded.

#### Scenario: Thread mapping
- **WHEN** a Gmail thread with three messages is mapped
- **THEN** one `Thread` with the Gmail thread ID and three `Message` records is produced, with participants deduplicated across headers

#### Scenario: HTML-only message
- **WHEN** a message has no `text/plain` part
- **THEN** the message is stored with its snippet and marked as body unavailable, and no HTML is stored

#### Scenario: Long body truncated
- **WHEN** a plain-text body exceeds `MAX_BODY_BYTES`
- **THEN** the stored body is truncated to that limit on a UTF-8 character boundary

#### Scenario: Labels are read only
- **WHEN** the mapper reads the `INBOX`, `UNREAD` or `SENT` labels
- **THEN** only local state fields change and no request is made to modify Gmail labels

### Requirement: Draft creation from approved proposals
The system SHALL create a Gmail draft only when a `draft` proposal has been approved by a human. The draft MUST be created with `users.drafts.create` and MUST carry the thread's `In-Reply-To` and `References` headers when the proposal targets a thread. The draft body SHALL be plain text only (`Content-Type: text/plain; charset=UTF-8`) with no HTML part. The system MUST NOT send the draft.

#### Scenario: Approved draft created
- **WHEN** a human approves a draft proposal targeting a Gmail thread
- **THEN** a Gmail draft is created inside that thread, and its draft ID is recorded on the proposal and in the audit log

#### Scenario: Plain-text draft only
- **WHEN** a draft is created for an approved proposal
- **THEN** the MIME message has a single `text/plain` part and no HTML or multipart/alternative part

#### Scenario: Unapproved proposal creates nothing
- **WHEN** a draft proposal is pending or rejected
- **THEN** no request is made to the Gmail drafts endpoint

#### Scenario: Stale thread blocks approval
- **WHEN** the source thread changed after the draft proposal was created
- **THEN** the proposal is flagged stale and the approval path requires the existing stale-thread confirmation before creating the draft

### Requirement: Error classification and circuit breaker
The connector SHALL map provider responses to `ConnectorError` as follows: restriction or account-verification responses map to `Checkpoint` or `Challenge`; rate limits, HTTP 429 and HTTP 5xx map to `Transient`; invalid-grant (refresh token rejected) maps to `Checkpoint`; other unrecognised failures that are not verification or restriction bodies map to `Other`. Unrecognised verification or restriction bodies MUST be classified as `Checkpoint`. `Challenge` and `Checkpoint` MUST NOT be retried.

#### Scenario: Checkpoint opens the breaker
- **WHEN** a sync call receives a restriction response from Google
- **THEN** the guard engages the Gmail kill switch, writes a `breaker_opened` audit entry, and notifies the human

#### Scenario: Transient errors bounded
- **WHEN** a sync call receives HTTP 429 on every attempt
- **THEN** the connector stops after `TRANSIENT_ATTEMPTS_PER_CYCLE` attempts in that cycle and reports `TransientExhausted`

#### Scenario: Invalid grant opens the breaker
- **WHEN** a token refresh fails with `invalid_grant`
- **THEN** the Gmail kill switch is engaged, a `breaker_opened` audit entry is written, the human is notified that re-authorization is needed, and no retry occurs

#### Scenario: Unknown restriction body fails closed
- **WHEN** a non-success response body is not recognised
- **THEN** the connector classifies it as `Checkpoint` and the breaker opens rather than retrying

### Requirement: Scheduler integration and kill switch
The daemon SHALL call the Gmail connector through the guard on a fixed interval of `GMAIL_SYNC_INTERVAL_SECS`. Engaging the Gmail kill switch SHALL stop sync without discarding the stored cursor.

#### Scenario: Scheduled sync
- **WHEN** the sync interval elapses and the kill switch is disengaged
- **THEN** the scheduler calls fetch through the guard with the stored cursor

#### Scenario: Kill switch engaged
- **WHEN** the kill switch is engaged
- **THEN** scheduled fetches are refused with a `refused_kill_switch` audit entry and the stored cursor is unchanged

### Requirement: Audit of connector activity
Every sync cycle, draft creation and authorization event SHALL produce an append-only audit entry containing the action name, source `gmail`, and outcome. Audit entries MUST NOT contain message bodies, tokens or email addresses beyond what the audit format already records.

#### Scenario: Sync cycle audited
- **WHEN** a sync cycle completes
- **THEN** an audit entry with action `fetch`, source `gmail` and outcome `ok` or the error class is appended

#### Scenario: Draft audited
- **WHEN** a draft is created
- **THEN** an audit entry with action `draft.create`, source `gmail` and the Gmail draft ID as outcome detail is appended

### Requirement: Human-only connector kill-switch administration
The system SHALL let the human list, release and engage the kill switch of any connector through new protocol requests `ConnectorStatus`, `ConnectorRelease` and `ConnectorEngage`. These requests SHALL be executable only by the human principal, SHALL be denied for every agent and machine principal, and SHALL be audited. The CLI verbs `haste connector status|release|engage <source>` SHALL exist only in the human build. Releasing a kill switch SHALL NOT clear the connector's stored cursor.

#### Scenario: Human releases an engaged connector
- **WHEN** the human runs `haste connector release gmail` while the Gmail kill switch is engaged
- **THEN** the switch is disengaged, a `connector.release` audit entry naming source `gmail` is appended, a notice event is published, and the next scheduled fetch runs from the stored cursor

#### Scenario: Human engages a connector
- **WHEN** the human runs `haste connector engage gmail`
- **THEN** the switch is engaged, a `connector.engage` audit entry is appended, and scheduled fetches are refused until it is released

#### Scenario: Agent cannot release
- **WHEN** an agent or machine token sends `ConnectorRelease` or `ConnectorEngage`
- **THEN** the daemon responds `denied`, changes nothing, and audits the attempt

#### Scenario: Agent build has no connector admin verbs
- **WHEN** an agent runs the `haste` CLI built without the human feature
- **THEN** `connector` is not a recognized command

#### Scenario: Status lists every known connector
- **WHEN** the human requests connector status
- **THEN** each known source is returned with whether its kill switch is engaged

#### Scenario: Unknown source
- **WHEN** the human releases or engages a source that has no registered connector
- **THEN** the daemon responds with a validation error and changes nothing

#### Scenario: TUI releases with confirmation
- **WHEN** the human selects an engaged connector in the TUI connectors panel and presses the release key
- **THEN** the TUI asks for confirmation, and only after `y` sends `ConnectorRelease`; any other key cancels

#### Scenario: Breaker trips again after release
- **WHEN** a released connector immediately returns a checkpoint error
- **THEN** the kill switch is engaged again with no retry
