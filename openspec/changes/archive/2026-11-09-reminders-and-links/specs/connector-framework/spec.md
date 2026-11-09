## MODIFIED Requirements

### Requirement: Connector trait with capability flags
Every connector SHALL implement a common trait exposing its `source` and a capability set with flags `read`, `draft`, `send`, `archive`, `reminders_create` and `reminders_complete`. The task-like methods `create_reminder` and `complete_reminder` SHALL default to returning an unsupported error. The daemon SHALL invoke a connector method only when the corresponding capability flag is set.

#### Scenario: Method blocked by capability
- **WHEN** a connector without the `send` flag is asked to send
- **THEN** the daemon refuses the call without invoking the connector and audits the refusal

#### Scenario: Default task method unsupported
- **WHEN** a connector that does not override `create_reminder` is asked to create a reminder
- **THEN** the call returns an unsupported error and the connector is not otherwise invoked
