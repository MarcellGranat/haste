## ADDED Requirements

### Requirement: AI requests are human-only
The daemon SHALL accept the requests `AiSuggestionGet`, `AiRewrite` and `AiMemoriesList` only from the human principal. Agent and machine principals SHALL be denied these requests by the policy gate, and the denial SHALL be audited like other denied requests.

#### Scenario: Agent asks for a suggestion
- **WHEN** an agent token sends `AiSuggestionGet`
- **THEN** the request is denied and nothing is queued or run

#### Scenario: Human asks for a rewrite
- **WHEN** the human TUI sends `AiRewrite` with a thread id, mode and draft
- **THEN** the daemon returns the rewritten text or a typed outcome

### Requirement: Suggestion request returns cached state
`AiSuggestionGet` SHALL return the stored draft and TODO suggestions for a thread with one of the states `ready`, `pending` (a job is queued or gated), `none` (no approved sender, or no suggestion), or `unavailable`. If no stored result exists for the current thread version and memory fingerprint, the daemon SHALL enqueue the missing jobs.

#### Scenario: Result not yet computed
- **WHEN** the TUI asks for a suggestion that has not been computed
- **THEN** the response state is `pending` and the job is queued

#### Scenario: Result ready
- **WHEN** the stored result matches the current thread version
- **THEN** the response state is `ready` with the stored text

### Requirement: Protocol version bump
Adding these requests SHALL raise the protocol version from 11 to 12. Requests with another version SHALL be rejected as before.

#### Scenario: Old client
- **WHEN** a client with protocol version 11 connects
- **THEN** it receives the existing version-mismatch error
