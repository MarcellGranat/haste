## ADDED Requirements

### Requirement: Calendar proposal payloads
The proposal system SHALL support two calendar payloads: `CalendarCreate` with calendar, title, start, end and optional attendees and description, and `CalendarUpdate` with an event reference and changed fields limited to title, start, end, description and location. A proposal from an agent SHALL record the herdr workspace name as its project.

#### Scenario: Create proposal recorded
- **WHEN** an agent in a herdr workspace proposes a calendar create
- **THEN** a pending `CalendarCreate` proposal is stored with the workspace as project and nothing is written to Google Calendar

#### Scenario: Update with disallowed field rejected
- **WHEN** an agent proposes a `CalendarUpdate` naming a field outside the allow-list
- **THEN** the proposal is rejected at creation and not stored as pending

### Requirement: Calendar proposals execute only on human approval
Approval of a calendar proposal SHALL be a separate human action from creating the proposal. Execution SHALL happen only on approval and SHALL be audited with the Google event ID as detail.

#### Scenario: Approval executes once
- **WHEN** the human approves a calendar proposal
- **THEN** the connector executes it once and the proposal records the result

#### Scenario: Rejected proposal executes nothing
- **WHEN** the human rejects a calendar proposal
- **THEN** no calendar request is made

### Requirement: iMessage draft proposals
An iMessage reply SHALL be representable only as a `Draft` proposal whose body is plain text and whose target is an iMessage thread. Approving it SHALL NOT transmit it.

#### Scenario: iMessage draft stored
- **WHEN** an agent proposes a reply to an iMessage thread
- **THEN** a pending `Draft` proposal with source `imessage` is stored and nothing is transmitted

#### Scenario: Approved draft is copy-only
- **WHEN** the human approves an iMessage draft
- **THEN** the proposal is marked approved for manual copy and no transmission occurs
