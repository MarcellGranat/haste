## ADDED Requirements

### Requirement: NoteAppend proposal payload
The proposal store SHALL support a `NoteAppend` payload holding the principal, the vault-relative target path, the request text (or the typed reference for a back-reference), the idempotency key, and the creation time. Its audit record SHALL carry the proposal id, path, and byte length only.

#### Scenario: Pending note proposal
- **WHEN** an agent in `approval` mode requests a note append
- **THEN** a `NoteAppend` proposal is stored in `pending` with the payload and the audit record contains no text

### Requirement: NoteAppend executes only on human approval
Approving a `NoteAppend` proposal SHALL run the daemon's note executor once, under the original principal, using the same path and body checks as an automatic append. Re-approval of an executed proposal SHALL NOT append again. If the executor refuses (for example `vault_missing`, `size_limit`, or `front_matter_mismatch`), the proposal SHALL record the refusal reason and stay closed without writing.

#### Scenario: Approval runs once
- **WHEN** the human approves a `NoteAppend` proposal twice
- **THEN** the text is appended exactly once

#### Scenario: Approval refused by the executor
- **WHEN** the human approves a proposal whose target now fails the front matter check
- **THEN** nothing is written and the proposal records `front_matter_mismatch`

### Requirement: NoteAppend uses the proposal expiry rules
A `NoteAppend` proposal SHALL expire after the existing proposal TTL and SHALL write nothing on expiry. Supersession SHALL apply only when the principal, target path, and source are all equal.

#### Scenario: Expiry
- **WHEN** a pending `NoteAppend` proposal passes its TTL
- **THEN** it becomes `expired` and nothing is written
