## ADDED Requirements

### Requirement: Calendar actions are pending for agents
The policy gate SHALL evaluate `calendar.create` and `calendar.update` for agent and machine principals as `pending`, never `auto`. The gate SHALL NOT define a `calendar.delete` action.

#### Scenario: Agent calendar create pending
- **WHEN** an agent requests `calendar.create`
- **THEN** the outcome is `pending` and a proposal is created

#### Scenario: Agent calendar update pending
- **WHEN** an agent requests `calendar.update`
- **THEN** the outcome is `pending` and a proposal is created

#### Scenario: Calendar delete undefined
- **WHEN** any principal requests `calendar.delete`
- **THEN** the gate returns `denied` because the action is not defined
