## ADDED Requirements

### Requirement: Daemon issues per-principal tokens
The daemon SHALL issue a random token of at least 256 bits to each newly registered agent or herdr machine. The token SHALL be shown to the human once at issuance and SHALL NOT be retrievable afterwards.

#### Scenario: Token issued once
- **WHEN** an agent is registered
- **THEN** a token is returned to the registering human and the daemon stores only a salted hash of it

#### Scenario: Token not retrievable
- **WHEN** a client asks the daemon to show an existing token
- **THEN** the daemon returns no token value

### Requirement: Tokens bind identity and allowed actions
Each token SHALL be bound to exactly one principal and to a subset of actions. Requests SHALL be authorized against both the principal identity and the bound subset before the policy gate is consulted.

#### Scenario: Action outside token subset
- **WHEN** a valid token bound to `read` only requests `draft`
- **THEN** the request is rejected as unauthorized and audited

### Requirement: Herdr environment is a hint only
The herdr pane environment variable (such as `HASTE_AGENT`) SHALL be used only to select which token a client presents. The daemon SHALL NOT treat any environment-provided value as proof of identity.

#### Scenario: Forged environment variable
- **WHEN** a process sets `HASTE_AGENT` to another agent's identifier but presents its own token
- **THEN** the daemon identifies the principal from the token alone and ignores the environment value

#### Scenario: Missing token
- **WHEN** a request arrives with no token
- **THEN** the daemon rejects it with an unauthenticated error

### Requirement: Token revocation
The human SHALL be able to revoke any token. A revoked token SHALL be rejected on every subsequent request and SHALL NOT be reinstated.

#### Scenario: Revoked token rejected
- **WHEN** a token is revoked and then presented
- **THEN** the daemon rejects the request as unauthenticated and audits the attempt

### Requirement: Token secrecy in logs
Tokens SHALL NOT appear in audit records, daemon logs, or error messages.

#### Scenario: Error message redaction
- **WHEN** a request with an invalid token fails
- **THEN** the error and audit record contain no token material
