# connector-framework Specification

## Purpose
TBD - created by syncing change core-policy-gate. Update Purpose after archive.

## Requirements

### Requirement: Connector trait with capability flags
Every connector SHALL implement a common trait exposing its `source` and a capability set with flags `read`, `draft`, `send`, and `archive`. The daemon SHALL invoke a connector method only when the corresponding capability flag is set.

#### Scenario: Method blocked by capability
- **WHEN** a connector without the `send` flag is asked to send
- **THEN** the daemon refuses the call without invoking the connector and audits the refusal

### Requirement: Per-connector kill switch
Each connector SHALL have a persisted kill switch. When the switch is engaged, the daemon SHALL NOT invoke any method on that connector, and the switch SHALL survive daemon restart.

#### Scenario: Kill switch engaged
- **WHEN** the kill switch for a connector is engaged
- **THEN** all reads, drafts, sends, and archives for that connector are refused until it is released

#### Scenario: Kill switch survives restart
- **WHEN** the daemon restarts with the switch engaged
- **THEN** the connector remains disabled

### Requirement: Circuit breaker on challenge or checkpoint
When a connector returns a challenge or checkpoint error, the daemon SHALL immediately engage that connector's kill switch, SHALL NOT retry the call, and SHALL notify the human through the TUI.

#### Scenario: Checkpoint response
- **WHEN** a connector call returns a checkpoint error
- **THEN** the kill switch is engaged, no retry occurs, and the approvals or status view shows a notification

### Requirement: Bounded retry for transient errors
For transient errors, the daemon SHALL allow at most a fixed small number of attempts within one scheduled cycle, after which it SHALL wait for the next cycle. The daemon SHALL NOT run an unbounded retry loop.

#### Scenario: Transient failure exhausts budget
- **WHEN** a transient error occurs on every attempt in a cycle
- **THEN** exactly the configured number of attempts is made and the connector is not called again until the next cycle

### Requirement: Connector calls are audited
Every connector invocation, its outcome, and any kill-switch or circuit-breaker transition SHALL be written to the audit log.

#### Scenario: Successful fetch audited
- **WHEN** a connector fetch succeeds
- **THEN** an audit record names the connector and the outcome, without message bodies
