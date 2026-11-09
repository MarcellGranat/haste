# audit-log Specification

## Purpose
TBD - created by syncing change core-policy-gate. Update Purpose after archive.

## Requirements

### Requirement: Append-only JSONL audit log
The daemon SHALL record every gate decision, proposal transition, note append, authentication failure, connector call, and delivery attempt as one JSON object per line in an append-only file. The daemon SHALL open the file in append mode and SHALL NOT truncate or rewrite it.

#### Scenario: Decision recorded
- **WHEN** the gate resolves any action to an outcome
- **THEN** one audit line is appended containing the principal, action, target, and outcome

#### Scenario: Existing lines untouched
- **WHEN** new audit records are written
- **THEN** all previously written bytes are unchanged

### Requirement: Hash-chained records
Each audit record SHALL include a monotonically increasing sequence number and the SHA-256 hash of the previous record. A verification command SHALL detect any missing, reordered, or altered record.

#### Scenario: Altered record detected
- **WHEN** a record in the middle of the file is modified and the verification command is run
- **THEN** verification reports the first sequence number where the chain breaks

### Requirement: File permissions
The audit file SHALL be created with mode `0600`.

#### Scenario: New audit file
- **WHEN** the daemon creates the audit file for the first time
- **THEN** its permission bits are `0600`

### Requirement: Audit records contain no secrets
Audit records SHALL NOT include tokens, note bodies, or message bodies. They MAY include object identifiers, action names, outcomes, and reason codes.

#### Scenario: Note body omitted
- **WHEN** a note append is audited
- **THEN** the record contains the target path and length but not the appended text

### Requirement: Single writer
Only the daemon SHALL write to the audit file. Clients SHALL obtain audit information only through a daemon request.

#### Scenario: Client read request
- **WHEN** a client requests recent audit records
- **THEN** the daemon returns them from its own read of the file

### Requirement: Herdr link and agent transition audit
The daemon SHALL append audit records for herdr link state changes (connected, unreachable, protocol mismatch, with machine label and attempt number), for each transition of an agent to `blocked` after a machine's baseline snapshot (with machine label, workspace id, pane id, and the count of currently blocked agents), and for each herdr result delivery attempt (with proposal id, attempt number, and outcome). These records SHALL contain counts and identifiers only.

#### Scenario: Blocked transition audited without content
- **WHEN** an agent transitions to `blocked`
- **THEN** one audit record is appended with machine, workspace id, pane id and count, and contains no pane text, title, or label value

#### Scenario: Protocol mismatch audited
- **WHEN** the link detects a protocol mismatch
- **THEN** one `herdr.link` audit record is appended naming the expected and observed protocol numbers

#### Scenario: Delivery attempt audited
- **WHEN** a result delivery attempt fails
- **THEN** one audit record is appended with the proposal id and attempt number and no message body

### Requirement: Tag changes audited
Each change to a project tag on a thread SHALL be audited with the thread identifier, the tag added or removed, and the acting principal, and each refused or denied tag change SHALL be audited with its reason code.

#### Scenario: Tag added audited
- **WHEN** a human adds tag `haste` to a thread
- **THEN** one `tag.change` audit record names the thread id, the tag, and the human principal

#### Scenario: Denied tag change audited
- **WHEN** an agent token requests a tag change
- **THEN** the denial is recorded with the principal and no tag is changed
