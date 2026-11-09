## ADDED Requirements

### Requirement: iMessage draft proposals
An iMessage reply SHALL be representable only as a `Draft` proposal whose body is plain text and whose target is an iMessage thread. Approving it SHALL NOT transmit it.

#### Scenario: iMessage draft stored
- **WHEN** an agent proposes a reply to an iMessage thread
- **THEN** a pending `Draft` proposal with source `imessage` is stored and nothing is transmitted

#### Scenario: Approved draft is copy-only
- **WHEN** the human approves an iMessage draft
- **THEN** the proposal is marked approved for manual copy and no transmission occurs
