## MODIFIED Requirements

### Requirement: Gmail connector capability declaration
The Gmail connector SHALL declare the capabilities `read = true`, `draft = true`, `send = true` and `archive = false`. The `send` flag is consulted only by the human send path. The connector MUST NOT expose any method that modifies Gmail labels, archives, or deletes messages or threads. The connector SHALL NOT call any send method except from the human send path.

#### Scenario: Capabilities reported
- **WHEN** the daemon queries the Gmail connector capabilities
- **THEN** the result has `read`, `draft` and `send` set to true and `archive` set to false

#### Scenario: Send refused through the guard without a human
- **WHEN** a request that did not originate from the human send path reaches the Gmail connector through the guard
- **THEN** the guard refuses it, no Gmail request is made, and a refusal audit entry is written

### Requirement: Draft creation from approved proposals
The system SHALL create a Gmail draft only when a `draft` proposal has been approved by a human. The draft MUST be created with `users.drafts.create` and MUST carry the thread's `In-Reply-To` and `References` headers when the proposal targets a thread. The draft body SHALL be plain text only (`Content-Type: text/plain; charset=UTF-8`) with no HTML part. Creating a draft SHALL NOT send it; sending is only possible through the human send requirement.

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

## ADDED Requirements

### Requirement: Human-only Gmail send
The Gmail connector SHALL send an approved mail proposal only from the human send path, through one Gmail API send call made once per send. The message SHALL be built from the proposal's stored recipient, subject and body as defined by the email-send spec and SHALL be sent with the threading headers of the source thread. The connector SHALL use only the scopes `gmail.readonly` and `gmail.compose`. No scope SHALL be added.

#### Scenario: Confirmed send
- **WHEN** the human confirms a send of an approved proposal targeting a Gmail thread
- **THEN** exactly one send request is made, carrying the stored recipient, and the proposal moves to `sent` with the Gmail message id as result

#### Scenario: Method choice recorded
- **WHEN** the connector is built
- **THEN** the send method used is the one recorded in design D6 (`users.messages.send` with a raw message, or `users.drafts.send` if the spike requires it)

#### Scenario: Scope set unchanged
- **WHEN** the send path is compiled and the consent URL is built
- **THEN** the requested scopes are exactly `gmail.readonly` and `gmail.compose`

### Requirement: Gmail send error classification
The Gmail send path SHALL classify outcomes as follows: an HTTP 4xx response or HTTP 429 response is `NoEffect`; a refresh token rejected with `invalid_grant` is `Checkpoint` before any send request; a connection error, timeout, or HTTP 5xx response after the send request was written is `Ambiguous`. A send SHALL NOT be retried, including on HTTP 429 or 5xx.

#### Scenario: Rate limited send
- **WHEN** Gmail answers HTTP 429 to the send request
- **THEN** the outcome is `NoEffect`, the proposal returns to `approved`, and no retry is made

#### Scenario: Server error after request
- **WHEN** Gmail answers HTTP 500 to the send request
- **THEN** the outcome is `Ambiguous`, the proposal stays `executing`, and no retry is made

#### Scenario: Timeout after request written
- **WHEN** the connection times out after the send request body was written
- **THEN** the outcome is `Ambiguous`

### Requirement: Draft cleanup after a Gmail send
After a successful Gmail send, the connector MAY delete the approval-time draft recorded on the proposal, once, best-effort, through `users.drafts.delete`, if the design decision Q1 requires it. A failed or absent cleanup SHALL NOT change the proposal state, SHALL NOT be retried, and SHALL be audited with a count only.

#### Scenario: Cleanup fails
- **WHEN** the draft deletion fails after a successful send
- **THEN** the proposal stays `sent`, no retry is made, and the audit records `draft.cleanup` with outcome `failed`

### Requirement: Audit of connector activity
Every sync cycle, draft creation, send and authorization event SHALL produce an append-only audit entry containing the action name, source `gmail`, and outcome. Audit entries MUST NOT contain message bodies, subjects, recipient addresses, tokens or email addresses beyond what the audit format already records.

#### Scenario: Sync cycle audited
- **WHEN** a sync cycle completes
- **THEN** an audit entry with action `fetch`, source `gmail` and outcome `ok` or the error class is appended

#### Scenario: Draft audited
- **WHEN** a draft is created
- **THEN** an audit entry with action `draft.create`, source `gmail` and the Gmail draft ID as outcome detail is appended

#### Scenario: Send audited
- **WHEN** a send completes with any outcome
- **THEN** an audit entry with action `send`, source `gmail`, the outcome class, and the Gmail message id when one exists is appended, and contains no address or subject
