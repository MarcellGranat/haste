## ADDED Requirements

### Requirement: Shared OAuth code is parameterised without changing Gmail behavior
The OAuth consent, PKCE, loopback listener and token-refresh code SHALL be usable by more than one Google connector through a parameter set of scopes and Keychain service. The Gmail connector SHALL continue to use exactly the scopes `gmail.readonly` and `gmail.compose` and the Keychain service `com.haste.gmail`, and its existing requirements and tests SHALL pass unchanged.

#### Scenario: Gmail parameters unchanged
- **WHEN** the Gmail connector authorizes after the refactor
- **THEN** it requests exactly `gmail.readonly` and `gmail.compose` and stores its refresh token under `com.haste.gmail`

#### Scenario: Existing Gmail tests pass
- **WHEN** the Gmail connector test suite runs after the refactor
- **THEN** every existing test passes without modification
