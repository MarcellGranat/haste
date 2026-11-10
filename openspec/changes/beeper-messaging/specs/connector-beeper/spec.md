## ADDED Requirements

### Requirement: Beeper connector registers one source per connector with the kill switch engaged
The Beeper connector SHALL be registered as three connectors, one each for `Source::Whatsapp`, `Source::Messenger` and `Source::Imessage`, each with its own kill switch, cursor and audit label. Each SHALL declare `default_engaged = true`. Capabilities SHALL be `read = true`, `send = true` (human path only), `draft = false` and `archive = false`.

#### Scenario: Fresh install engages every source
- **WHEN** the daemon starts for the first time with the Beeper connectors registered
- **THEN** the kill switches for `whatsapp`, `messenger` and `imessage` are engaged and no Beeper request is made

#### Scenario: One source released
- **WHEN** the human releases `whatsapp` and not `messenger`
- **THEN** only the WhatsApp connector reads, and the Messenger connector makes no request

#### Scenario: No archive or draft capability
- **WHEN** the capability set of any Beeper connector is inspected
- **THEN** `archive` and `draft` are false and no method archives or creates a provider draft

### Requirement: Loopback-only base URL
The connector SHALL talk only to a loopback base URL. The default SHALL be `http://127.0.0.1:23373`. An override in `beeper.json` SHALL be accepted only when its host is `127.0.0.1`, `localhost` or `[::1]` and its scheme is `http`. The HTTP client SHALL NOT follow redirects.

#### Scenario: Non-loopback override refused
- **WHEN** `beeper.json` sets the base URL to a non-loopback host
- **THEN** the configuration fails to load with a validation error, the connector does not start, and no connection is attempted

#### Scenario: Redirect not followed
- **WHEN** the Beeper API answers with a redirect
- **THEN** the connector does not follow it and reports a `Other` error with no URL in the message

### Requirement: Token stored only in the macOS Keychain
The Beeper API token SHALL be stored only in the macOS Keychain under service `com.haste.beeper`. The human SHALL enter it through `haste connector beeper auth` (human build only), read without echo. No file, audit entry, log line, or environment variable SHALL hold the token, and no build SHALL read the token from an environment variable.

#### Scenario: Token absent from files
- **WHEN** the auth verb completes
- **THEN** no file in the data directory contains the token text

#### Scenario: No environment token
- **WHEN** a token value is set in the process environment
- **THEN** the connector ignores it and reads only the Keychain item

#### Scenario: Token rejected
- **WHEN** Beeper rejects the token with 401 or 403 during a cycle or send
- **THEN** the outcome is `Authorization` (permission needed), the connector is not retried in the cycle, and the circuit breaker does not open

### Requirement: Beeper not running is a permission state
When the Beeper Desktop API cannot be reached because the application is not running, the connector SHALL return `Authorization` with the reason "Beeper Desktop not running". It SHALL NOT retry within the cycle and SHALL NOT trip the circuit breaker.

#### Scenario: Connection refused
- **WHEN** a cycle cannot connect to the loopback address
- **THEN** the cycle ends with `Authorization`, the kill switch stays released, and the next scheduled cycle tries again

### Requirement: On-device connections only
Before reading or sending through an account, the connector SHALL verify that the account is an on-device connection. When the API exposes the connection type, a cloud connection SHALL be refused with the reason "cloud connection" and no chat of that account SHALL be read or sent, unless `beeper.json` records `allow_cloud: true` for that account id (a per-account human decision that never applies to another account and is never implied by `on_device_attested`). When the API does not expose the connection type, the account SHALL be read only if `beeper.json` records `on_device_attested: true` for that account id. An account with neither signal SHALL be refused with the reason "connection type unknown".

#### Scenario: Cloud account allowed per account
- **WHEN** a selected account is reported as a cloud connection and `beeper.json` records `allow_cloud: true` for it
- **THEN** its selected chats are read and replies may be sent, the cycle is audited with the label `cloud_allowed`, and another cloud account without the setting is still refused

#### Scenario: Cloud account refused
- **WHEN** a selected account is reported as a cloud connection and `allow_cloud` is not set for it
- **THEN** no chat of that account is read, the refusal reason "cloud connection" is recorded as a counts-only label, and no request is made for its chats

#### Scenario: Unknown connection type without attestation
- **WHEN** the API does not expose the connection type and `beeper.json` has no attestation for the account
- **THEN** the account is refused with "connection type unknown" and no chat of it is read

#### Scenario: Check runs before every send
- **WHEN** the human sends a reply through an account that the API now reports as cloud
- **THEN** the send is refused before any request to send and the proposal stays `approved`

### Requirement: Selected chats and accounts only
The connector SHALL read only the accounts and chats named in `beeper.json`. An empty chat allow-list SHALL read nothing. A chat named in the deny-list SHALL NOT be read even if it is in the allow-list. No request SHALL be made for an unselected account or chat, and unselected chats SHALL NOT be stored.

#### Scenario: Unlisted chat not read
- **WHEN** a sync cycle runs and a chat is not in the allow-list
- **THEN** no request is made for that chat's messages and no thread is stored for it

#### Scenario: Deny wins
- **WHEN** a chat id appears in both the allow-list and the deny-list
- **THEN** the chat is not read

### Requirement: Bounded reads
Each cycle SHALL read at most `BEEPER_MAX_CHATS_PER_CYCLE` chats and at most `BEEPER_MAX_PAGES_PER_CHAT` pages per chat, with page size `BEEPER_PAGE_SIZE`. The first sync for a chat SHALL read only messages within `history_days` and at most `BEEPER_BACKFILL_PER_CHAT` messages. Message bodies SHALL be truncated to `BEEPER_BODY_MAX_BYTES` at a UTF-8 boundary, with a marker when truncated. Non-text message types SHALL be stored as a fixed label and never as attachment names or content.

#### Scenario: Backfill cap
- **WHEN** a chat has more messages in the history window than `BEEPER_BACKFILL_PER_CHAT`
- **THEN** at most that many messages are stored for the first sync

#### Scenario: Body truncated
- **WHEN** a message body exceeds `BEEPER_BODY_MAX_BYTES`
- **THEN** the stored body is at most that many bytes, ends at a character boundary, and carries the truncation marker

#### Scenario: Attachment not stored
- **WHEN** a message is a media message
- **THEN** the stored body is the fixed label `[attachment]` and no file name or URL is stored

### Requirement: Incremental sync by chat cursor
The connector SHALL keep a cursor that maps each chat id to the last message sort key it stored. A later cycle SHALL read only messages newer than that key. Upserts SHALL be idempotent by Beeper message id, so reading the same message twice creates one `Message`. Missing history right after an account is added SHALL NOT be treated as an error.

#### Scenario: Delta cycle
- **WHEN** a chat has a stored sort key and new messages exist
- **THEN** only messages after that key are mapped

#### Scenario: Re-read is idempotent
- **WHEN** the same page is read twice
- **THEN** the store holds one `Message` per Beeper message id

### Requirement: Mapping chats, messages and people
Each selected chat SHALL map to one `Thread` with `source` set from the account network and `external_id` equal to the Beeper chat id. Each message SHALL map to one `Message` in its chat's thread with `external_id` equal to the Beeper message id. Senders and participants SHALL map to `Person` records keyed by the Beeper user id. An account network that maps to no supported source SHALL be refused with "unsupported network".

#### Scenario: Chat becomes thread
- **WHEN** a selected WhatsApp chat is synced
- **THEN** it is stored as one `Thread` with `source = whatsapp` and the Beeper chat id as external id

#### Scenario: Unsupported network refused
- **WHEN** a selected account's network maps to no supported source
- **THEN** that account is refused with "unsupported network" and nothing is stored for it

### Requirement: Human-only send into an existing selected chat
A reply SHALL be sent only by the human, through the `Send` request of the send lifecycle, and only into a chat that is an existing thread selected in `beeper.json`. Each send SHALL target exactly one chat. The connector SHALL NOT create chats, SHALL NOT send to a chat identifier that was not selected, and SHALL NOT send attachments.

#### Scenario: Agent cannot send
- **WHEN** an agent principal requests a send to a Beeper chat
- **THEN** the outcome is `denied` and no request is made to Beeper

#### Scenario: Unselected chat refused
- **WHEN** the human sends to a chat that is not in the allow-list
- **THEN** the send is refused before any request and the proposal stays `approved`

#### Scenario: Attachment refused
- **WHEN** a proposal carries an attachment
- **THEN** the send is refused with a validation error and no request is made

### Requirement: Send limits as named constants
Each send SHALL respect `BEEPER_MAX_SEND_BYTES`, `BEEPER_MAX_SENDS_PER_HOUR` and `BEEPER_MAX_SENDS_PER_DAY`, counted over the Beeper sends whose start time falls within the last hour (for the same network's account) and the last day (across WhatsApp, Messenger and iMessage together). Sends in `executing` SHALL count. The limits SHALL be compile-time constants and SHALL NOT be read from any file.

#### Scenario: Hourly cap reached
- **WHEN** the number of sends started in the last hour for this network's account equals `BEEPER_MAX_SENDS_PER_HOUR`
- **THEN** the next send is refused with "hourly limit reached" before any request

#### Scenario: Crash does not reset the count
- **WHEN** the daemon restarts after a send started and is still `executing`
- **THEN** that send still counts toward the caps

### Requirement: Send confirmation digest and at-most-once
The human SHALL confirm a send only after seeing the chat name, the network and the text. The confirmation SHALL carry a digest over the Beeper chat id, the displayed chat name and the SHA-256 of the body. If the freshly computed digest differs, the send SHALL be refused with `recipients_changed` and nothing SHALL change. A send SHALL make exactly one request to Beeper and SHALL NOT be retried at any layer.

#### Scenario: Digest mismatch
- **WHEN** the chat name changes between preview and confirmation
- **THEN** the send is refused with `recipients_changed`, the proposal stays `approved`, and no request is made

#### Scenario: One request per send
- **WHEN** a send is accepted by the human and the network succeeds
- **THEN** exactly one send request is made and the proposal becomes `sent` with result `sent:beeper:<message id>`

### Requirement: Send outcome classification
A Beeper send SHALL classify its outcome as follows. Refusals before any request, a connection refused, a 401 or 403, a 4xx other than auth, and a 429 SHALL NOT leave the proposal in `executing`; they SHALL return it to `approved` with a reason label. A timeout or dropped connection after the request was written, any 5xx, and a 2xx response without a message id SHALL leave the proposal in `executing` as needing a check. An account restriction response SHALL open the source's circuit breaker.

#### Scenario: Timeout after write is ambiguous
- **WHEN** the request was written and the response times out
- **THEN** the proposal stays `executing`, the audit entry is `needs_check` with `outcome_unknown`, and no second request is made

#### Scenario: 2xx without an id is ambiguous
- **WHEN** Beeper answers 2xx without a message id
- **THEN** the proposal stays `executing` as needing a check and is not marked `sent`

#### Scenario: Rejected before send returns to approved
- **WHEN** Beeper returns 400 for the chat
- **THEN** the proposal returns to `approved` with reason "write refused" and no retry occurs

### Requirement: Counts-only audit and no content in logs
Each sync cycle SHALL write one audit entry with source, outcome, counts of new threads and messages, counts of skipped chats by reason label, and any cursor reset. Each send SHALL write `proposal.send` with a fixed outcome and reason label, and the Beeper message id on success. Audit entries, logs and errors SHALL NOT contain message text, chat names, handles, phone numbers, account ids, the token, or the content of `beeper.json`.

#### Scenario: Cycle audit has counts only
- **WHEN** a sync cycle completes
- **THEN** one audit entry with the source and counts is appended and no chat name or message text appears in it

#### Scenario: Error text has no content
- **WHEN** the API returns an error whose body contains a message text
- **THEN** the error recorded in audit and logs contains only the status code and a fixed label

### Requirement: Scope tags apply to chats
Threads from Beeper SHALL carry project tags set only by the human, using herdr workspace labels as candidates. Machine principals SHALL see a Beeper thread only when its tag exists on their own machine, enforced in the daemon read path. Unselected chats SHALL NOT be stored, so no principal can see them.

#### Scenario: Machine sees only tagged chats
- **WHEN** a machine principal lists threads
- **THEN** Beeper threads without a tag present on that machine are absent from the result

### Requirement: Kill switch refuses all Beeper calls
When a Beeper source's kill switch is engaged, the daemon SHALL NOT invoke any method of that source's connector, including preview, send and read, and SHALL NOT read the Keychain token for it.

#### Scenario: Engaged source makes no request
- **WHEN** the `messenger` kill switch is engaged and a sync or a send is attempted for Messenger
- **THEN** no request is made to Beeper and the attempt is refused with "kill switch engaged"
