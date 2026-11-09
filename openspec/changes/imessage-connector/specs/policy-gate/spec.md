## ADDED Requirements

### Requirement: iMessage send action is undefined
The policy gate SHALL NOT define an `imessage.send` action.

#### Scenario: iMessage send undefined
- **WHEN** any principal requests `imessage.send`
- **THEN** the gate returns `denied` because the action is not defined
