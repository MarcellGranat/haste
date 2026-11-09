## MODIFIED Requirements

### Requirement: Connector trait with capability flags
Every connector SHALL implement a common trait exposing its `source` and a capability set with flags `read`, `draft`, `send`, `archive`, `reminders_create` and `reminders_complete`. The task-like methods `create_reminder` and `complete_reminder` SHALL default to returning an unsupported error, and so SHALL the `send` method for a connector that does not declare `send`. The daemon SHALL invoke a connector method only when the corresponding capability flag is set, and `send` SHALL be invoked only from the human send path.

#### Scenario: Method blocked by capability
- **WHEN** a connector without the `send` flag is asked to send
- **THEN** the daemon refuses the call without invoking the connector and audits the refusal

#### Scenario: Default task method unsupported
- **WHEN** a connector that does not override `create_reminder` is asked to create a reminder
- **THEN** the call returns an unsupported error and the connector is not otherwise invoked

### Requirement: Bounded retry for transient errors
For transient errors on reads and draft creation, the daemon SHALL allow at most a fixed small number of attempts within one scheduled cycle, after which it SHALL wait for the next cycle. The daemon SHALL NOT run an unbounded retry loop. A send SHALL be exempt from retry entirely: it makes exactly one attempt, and a transient error on a send is never retried, as defined by the email-send spec.

#### Scenario: Transient failure exhausts budget
- **WHEN** a transient error occurs on every attempt in a cycle for a read
- **THEN** exactly the configured number of attempts is made and the connector is not called again until the next cycle

#### Scenario: Transient failure on a send
- **WHEN** a send returns a transient error
- **THEN** no second attempt is made, and the outcome is classified by the send rules
