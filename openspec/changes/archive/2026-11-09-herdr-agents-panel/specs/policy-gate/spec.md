## ADDED Requirements

### Requirement: Machine read scope is tag based
Machine principals SHALL have read actions narrowed to threads carrying a project tag in their own machine's last known workspace-label set. The gate SHALL continue to resolve each machine action to `auto`, `pending`, or `denied` as before; the tag scope applies to the read results returned after an allowed read and SHALL NOT itself produce `auto`.

#### Scenario: Read allowed but scoped
- **WHEN** a machine principal with read capability requests a thread list
- **THEN** the gate resolves the read as allowed and the returned set is limited to threads tagged with that machine's project labels

#### Scenario: Scope does not widen capability
- **WHEN** a machine principal has no entry for `draft`
- **THEN** the gate resolves `draft` to `denied` regardless of any tags on the target

### Requirement: Herdr-originated actions are gate-evaluated
Any action a herdr machine or agent principal can trigger through the herdr link, including the `agent.prompt` result delivery, SHALL be gate-evaluated under the principal's capability row. The link and the machine, pane and workspace hints SHALL NOT bypass the gate.

#### Scenario: Link event does not grant action
- **WHEN** a herdr status event arrives for a pane
- **THEN** no gate action is performed on behalf of the agent and no capability is inferred from the event
