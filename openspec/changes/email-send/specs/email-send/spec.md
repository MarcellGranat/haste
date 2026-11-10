## ADDED Requirements

### Requirement: Human-only send action
Sending an approved mail proposal SHALL be executable only by a human principal through the TUI or the human shell. Agent and machine principals SHALL be denied every send request, including `SendPreview`, regardless of payload, and the attempt SHALL be audited. No code path SHALL send mail without an explicit human action: pressing the send key, followed by a delay during which the human can cancel.

#### Scenario: Agent send denied
- **WHEN** an agent token submits a send or send-preview request for a proposal
- **THEN** the daemon responds `denied`, changes nothing, and audits the attempt

#### Scenario: No automatic send
- **WHEN** a proposal is approved and the daemon runs any scheduled task
- **THEN** no connector send method is invoked

### Requirement: Send requires an approved proposal
A send SHALL start only from a proposal in state `approved` and SHALL move it to `executing` in the same transaction that runs the stale check, the rate-limit check and the recipient freeze. A proposal in any other state SHALL be refused without a connector call.

#### Scenario: Pending proposal cannot be sent
- **WHEN** the human requests send for a proposal in state `pending`
- **THEN** the request is refused and no connector call is made

#### Scenario: Stale proposal needs acknowledgement
- **WHEN** the source thread changed after the proposal was approved
- **THEN** the send is refused until the human acknowledges the stale flag, and no connector call is made

### Requirement: Recipient confirmation
Before a send, the TUI SHALL show every recipient, the source and the subject, and SHALL send only after the human has pressed send and not cancelled within the 5-second delay. The confirmation SHALL carry a digest of the displayed recipient list. The daemon SHALL recompute the recipient list and SHALL refuse with `recipients_changed` if the digest differs, changing nothing.

#### Scenario: Confirmed recipients match
- **WHEN** the human confirms a send and the recomputed recipient list has the same digest
- **THEN** the send proceeds with exactly that list

#### Scenario: Recipients changed before confirmation
- **WHEN** the reply target changes between preview and confirmation
- **THEN** the send is refused with `recipients_changed` and the TUI reloads the preview

#### Scenario: Recipient list is frozen
- **WHEN** a send is started
- **THEN** the resolved recipient list is stored on the proposal and the connector uses only that stored list

### Requirement: Recipient limits and address validation
A send SHALL have at most `MAX_RECIPIENTS` recipients (1 in this change). Each address SHALL contain no CR, LF, NUL, comma, semicolon or whitespace, and SHALL match a `local@domain` shape. A violation SHALL refuse the send with a validation error; the value SHALL NOT be silently stripped or rewritten.

#### Scenario: Line break in an address refused
- **WHEN** a proposal's recipient contains a CR or LF character
- **THEN** the send is refused with a validation error, no connection is opened, and the proposal stays `approved`

#### Scenario: Too many recipients refused
- **WHEN** a send resolves to more recipients than `MAX_RECIPIENTS`
- **THEN** the send is refused and the proposal stays `approved`

### Requirement: Header and subject injection refused
A send SHALL refuse, and SHALL NOT transmit, any message whose subject, address or other header value contains a CR or LF character or a NUL character, or whose subject exceeds `MAX_SUBJECT_BYTES`. The refusal SHALL occur before any connection is opened.

#### Scenario: Subject with a line break
- **WHEN** a proposal subject contains `\nBcc: x@example.org`
- **THEN** the send is refused, no connection is opened, and no Bcc header is ever produced

#### Scenario: Oversized subject
- **WHEN** a subject is longer than `MAX_SUBJECT_BYTES`
- **THEN** the send is refused with a validation error

### Requirement: Body and message limits
The body of a send SHALL be plain text only, SHALL NOT exceed `MAX_SEND_BODY_BYTES`, and SHALL have no attachments, HTML part or multipart structure. A body over the limit SHALL be refused before `begin_send` moves the proposal.

#### Scenario: Oversized body refused
- **WHEN** a proposal body exceeds `MAX_SEND_BODY_BYTES`
- **THEN** the send is refused, the proposal stays `approved`, and no connection is opened

#### Scenario: Plain text only
- **WHEN** a message is composed for sending
- **THEN** it has a single `text/plain; charset=UTF-8` part and no attachment

### Requirement: Sends per hour
The daemon SHALL allow at most `MAX_SENDS_PER_HOUR` sends in any rolling 3600-second window, counting proposals whose send started in that window, including those in `executing`. A send over the limit SHALL be refused before `begin_send`. The count SHALL survive daemon restart.

#### Scenario: Limit reached
- **WHEN** `MAX_SENDS_PER_HOUR` sends started in the last hour and the human requests another
- **THEN** the request is refused with a rate-limit reason and the proposal stays `approved`

#### Scenario: Restart does not reset the count
- **WHEN** the daemon restarts within the hour after the limit was reached
- **THEN** the send is still refused

### Requirement: At-most-once sending with no automatic retry
A send SHALL make exactly one attempt at the connector. Neither the daemon nor the connector SHALL retry a send, at any layer, within a cycle or later. A send that is ambiguous SHALL leave its proposal in `executing`. An ambiguous outcome SHALL NOT move the proposal back to `approved` and SHALL NOT be re-run by the daemon, by a restart or by a later human action other than an explicit check decision.

#### Scenario: Timeout is ambiguous
- **WHEN** the connection times out after the message body was transmitted
- **THEN** the proposal is `executing`, the audit records `needs_check` with the detail `outcome_unknown`, and no second attempt is made

#### Scenario: Certain refusal returns to approved
- **WHEN** the provider rejects the send before accepting the message body
- **THEN** the proposal returns to `approved` with the refusal reason and the human may send again later

#### Scenario: Restart with an executing send
- **WHEN** the daemon restarts while a send is `executing`
- **THEN** the proposal is left untouched and listed as needing a check, and no send is performed

### Requirement: Send outcome classification
The system SHALL classify each send outcome as `NoEffect`, `Ambiguous` or `Sent`. Only `NoEffect` SHALL return the proposal to `approved`. `Ambiguous` SHALL keep it in `executing`. `Sent` SHALL move it to `sent`.

#### Scenario: Classification table applied
- **WHEN** a send fails before any message body is accepted by the provider
- **THEN** the outcome is `NoEffect`

#### Scenario: Failure after body transmitted
- **WHEN** a send fails after the message body was transmitted
- **THEN** the outcome is `Ambiguous`

### Requirement: Kill switch and circuit breaker apply to sends
A send SHALL be refused without a connector call when the connector's kill switch is engaged. A `Checkpoint` or `Challenge` result from a send SHALL engage the kill switch, SHALL NOT be retried, and SHALL notify the human.

#### Scenario: Send with engaged kill switch
- **WHEN** the human confirms a send while the connector kill switch is engaged
- **THEN** the send is refused with `kill switch engaged`, no connection is opened and the proposal stays `approved`

#### Scenario: Authentication rejected during send
- **WHEN** the provider rejects the credentials during a send
- **THEN** the kill switch is engaged, the human is notified, and the proposal stays `approved`

### Requirement: Send audit contains no content
Send audit entries SHALL contain the proposal id, the provider message id when one exists, the source, fixed outcome labels and counts (recipient count, body byte length). They SHALL NOT contain recipient addresses, subjects, body text or credentials, and error text returned to any caller SHALL NOT contain them either.

#### Scenario: Audit after a successful send
- **WHEN** a send succeeds
- **THEN** the audit entry `proposal.send` with outcome `sent` names the proposal id, the source and `recipients=1`, and contains no address or subject

#### Scenario: Audit scan
- **WHEN** the test suite sends a message whose address and subject contain distinctive markers
- **THEN** the audit log and captured logs contain neither marker
