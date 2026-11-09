## ADDED Requirements

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
