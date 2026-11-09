## MODIFIED Requirements

### Requirement: Result delivery to the agent
When a proposal reaches `sent`, `rejected`, or `expired`, the daemon SHALL deliver a short result message to the originating agent through herdr `agent.prompt`: over the herdr socket when the proposal's `origin_machine` is `local` or absent, and through `herdr --machine <label> agent prompt` when it is a remote machine. The herdr agent name SHALL be derived from the principal id by the mapping in `herdr-link`, and a principal that does not map SHALL NOT receive a prompt. Delivery SHALL be attempted at most three times with backoff, and each failure SHALL be recorded in the audit log. While the local herdr link is disconnected or in protocol mismatch, delivery to a local agent SHALL use the same three-attempt policy through the plain herdr CLI.

#### Scenario: Local delivery succeeds
- **WHEN** a proposal from a local agent is rejected and the agent pane is reachable
- **THEN** the agent receives one prompt over the socket containing the proposal ID and the result

#### Scenario: Remote delivery goes through the hub
- **WHEN** a proposal whose `origin_machine` is `m1` is rejected
- **THEN** the prompt is sent with `herdr --machine m1 agent prompt` and not over the local socket

#### Scenario: Delivery fails repeatedly
- **WHEN** three delivery attempts fail
- **THEN** no further attempts are made and the failure is recorded, and the result remains available through `haste proposal status`

#### Scenario: Unmappable principal
- **WHEN** a proposal result is due for a principal id that is not a valid herdr agent name
- **THEN** no prompt is sent, the refusal is audited, and the result remains available through `haste proposal status`

#### Scenario: Socket down uses fallback
- **WHEN** the local herdr link is disconnected and a result is due for a local agent
- **THEN** delivery uses the plain CLI fallback with the same three-attempt limit

## ADDED Requirements

### Requirement: Project attribution from the agent's workspace
A proposal SHALL carry an optional `project` and an optional `origin_machine`. When the agent CLI supplies a machine hint (`HASTE_MACHINE`, default `local`) and a pane hint (`HERDR_PANE_ID`), the daemon SHALL look the pane up in its status model for that machine and, if it is found, SHALL store the pane's workspace label as `project` and the machine as `origin_machine`. If the machine or pane is unknown, the proposal SHALL be created without a project and without error. The hints SHALL be used only for routing and attribution and SHALL NOT affect authentication, the capability of the principal, or the gate outcome.

#### Scenario: Workspace becomes the project
- **WHEN** an agent in the pane `w1:p1` of workspace `haste` on machine `local` creates a proposal with those hints
- **THEN** the proposal stores `project` = `haste` and `origin_machine` = `local`

#### Scenario: Unknown pane gives no project
- **WHEN** the pane hint matches no pane in the machine's status model
- **THEN** the proposal is created with no project and no error

#### Scenario: Hints do not grant anything
- **WHEN** an agent sends hints naming another agent's workspace
- **THEN** its identity, capability and gate outcome are unchanged and only its own proposal's project label differs
