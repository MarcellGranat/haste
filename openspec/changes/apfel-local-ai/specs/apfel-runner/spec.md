## ADDED Requirements

### Requirement: Daemon is the only caller of apfel
The `hasted` daemon SHALL be the only process that runs the `apfel` CLI. The TUI, the CLI and the connectors SHALL NOT spawn `apfel`. The daemon SHALL NOT run `apfel --serve` and SHALL NOT open any network listener for model access.

#### Scenario: TUI requests a suggestion
- **WHEN** the TUI needs an AI result
- **THEN** it sends a request over the daemon socket and does not start a process

#### Scenario: No server mode
- **WHEN** the daemon starts with AI enabled
- **THEN** no `apfel --serve` process is started and no new listening address is bound

### Requirement: One model call at a time, bounded by time
The daemon SHALL run at most one `apfel` process at a time, with a queue of at most `APFEL_QUEUE_DEPTH` (8) waiting calls. Each call SHALL be killed when it exceeds `APFEL_CALL_TIMEOUT` (60 seconds) and its outcome SHALL be `timeout`.

#### Scenario: Queue full
- **WHEN** a background job is enqueued while 8 calls are waiting
- **THEN** the job is not queued and its outcome is recorded as `gated` for that attempt

#### Scenario: Call exceeds its timeout
- **WHEN** an `apfel` process runs longer than 60 seconds
- **THEN** the daemon kills the process and records the outcome `timeout`

### Requirement: Content never on the command line
Message text, memory text and instructions SHALL be passed to `apfel` only through standard input or a file in the daemon's private run directory (mode `0700`, file mode `0600`). The file SHALL be deleted after the call, including on timeout or error.

#### Scenario: Prompt delivery
- **WHEN** the daemon runs `apfel` for a draft
- **THEN** the prompt is written to standard input (`--messages -`) and no argument of the process contains message or memory text

#### Scenario: Temporary file removed
- **WHEN** a call ends with any outcome
- **THEN** no file containing its prompt remains in the run directory

### Requirement: Token budget before each call
Before each call the daemon SHALL count the prompt tokens with `apfel --count-tokens` and SHALL drop the oldest earlier messages, then the lowest-ordered memory bodies, until the count is at most `APFEL_PROMPT_TOKEN_BUDGET` (2500). A prompt that still exceeds the budget SHALL NOT be sent and its outcome SHALL be `too_long`.

#### Scenario: Long thread
- **WHEN** a thread's prompt exceeds the budget
- **THEN** the oldest earlier messages are removed first and the call is made with the rest

#### Scenario: Still too long
- **WHEN** the latest inbound message alone exceeds the budget
- **THEN** no model call is made and the outcome is `too_long`

### Requirement: Structured output is validated
Output requested with `--schema` SHALL be parsed and validated by the daemon. An output that does not match the schema, or whose values exceed their length limits, SHALL be discarded with outcome `invalid_output`. The daemon SHALL NOT pass unvalidated model text to any client as structured data.

#### Scenario: Invalid JSON from the model
- **WHEN** the model output cannot be parsed against the TODO schema
- **THEN** the result is `invalid_output` and no suggestion is stored or shown

### Requirement: Results are cached by thread version
The daemon SHALL store each successful or refused result in table `ai_results` keyed by kind, thread id, thread version and memory fingerprint. A request with an unchanged key SHALL return the stored result without a model call. Rows older than 14 days SHALL be deleted by the expiry tick.

#### Scenario: Reopened thread
- **WHEN** a thread is opened again and its version and memory fingerprint are unchanged
- **THEN** the stored suggestion is returned and no `apfel` process is started

#### Scenario: Thread changed
- **WHEN** a new message arrives and the thread version increases
- **THEN** the cached suggestion is not returned and a new job is queued

### Requirement: Background jobs pass a resource gate
Background jobs (draft and TODO) SHALL run only when the resource gate passes: the one-minute load average divided by the core count is below `APFEL_MAX_LOAD_PER_CORE` (0.5), free memory is at least `APFEL_MIN_FREE_MB` (4096), and memory pressure is not `critical`. The gate SHALL be a pure function of the sampled values and the constants, and the constants SHALL be defined in one block in `crates/hasted/src/apfel.rs`.

#### Scenario: Machine busy
- **WHEN** the load per core is 0.8 and a draft job is due
- **THEN** the job is not run, its `next_attempt_at` moves 5 minutes ahead, and the outcome `gated` is logged

#### Scenario: Machine free
- **WHEN** load per core is 0.2, free memory is 8 GB and pressure is normal
- **THEN** the due job runs

#### Scenario: Gate sampling fails
- **WHEN** the load or memory value cannot be read
- **THEN** the gate fails closed and the job is skipped

### Requirement: Rewrite always runs when asked
A rewrite request SHALL NOT be subject to the resource gate and SHALL be placed at the front of the queue. It SHALL NOT be cached and SHALL NOT be retried silently.

#### Scenario: Busy machine
- **WHEN** the user asks for a rewrite while the load per core is 0.8
- **THEN** the rewrite runs and its result is returned

### Requirement: Single attempt per call
The daemon SHALL call `apfel` without `--retry`. Background jobs SHALL be retried only through the job queue, and a failed rewrite SHALL be reported to the user without a retry.

#### Scenario: Refusal
- **WHEN** apfel reports a guardrail refusal
- **THEN** the outcome is `refused`, no retry is made, and no suggestion is shown

### Requirement: Every call is logged without content
Every `apfel` call SHALL produce one operational log line and one audit record. The operational line SHALL carry component `apfel`, the tool (`draft`, `rewrite`, `todo`), a short hash of the thread id, `duration_ms`, `outcome`, `queue_depth`, the load and free memory sampled for the gate. The audit record SHALL carry action `ai.call`, the same short hash and the outcome. Neither record SHALL contain message text, memory text, instructions, model output, names, addresses or the full thread id.

#### Scenario: Successful draft
- **WHEN** a draft call completes with outcome `ok`
- **THEN** one `apfel` log line and one `ai.call` audit record are written with the duration and outcome and no content

#### Scenario: Log line content check
- **WHEN** the operational log for a call is inspected
- **THEN** every field value matches the existing log identifier rule and contains no free text

### Requirement: Model unavailable
If `apfel` is missing, not executable, or `apfel --model-info` reports the model is unavailable, the daemon SHALL record outcome `unavailable`, disable AI jobs until the next daemon start or until the settings file changes, and SHALL NOT retry in a loop.

#### Scenario: apfel not installed
- **WHEN** the configured path to `apfel` does not exist
- **THEN** outcome `unavailable` is logged once and background jobs are not enqueued
