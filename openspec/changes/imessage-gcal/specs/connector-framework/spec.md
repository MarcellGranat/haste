## ADDED Requirements

### Requirement: Connectors may register with the kill switch engaged by default
A connector descriptor SHALL declare whether its kill switch is engaged by default. A connector with no persisted kill-switch state and a default-engaged descriptor SHALL start engaged. Once the human releases it, the released state SHALL persist like any other kill-switch state.

#### Scenario: Default-engaged on first start
- **WHEN** a default-engaged connector is registered and no persisted state exists
- **THEN** its kill switch is engaged and all its calls are refused

#### Scenario: Release persists
- **WHEN** the human releases a default-engaged connector and the daemon restarts
- **THEN** the connector remains released
