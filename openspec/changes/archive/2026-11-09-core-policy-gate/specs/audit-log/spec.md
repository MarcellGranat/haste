## ADDED Requirements

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
