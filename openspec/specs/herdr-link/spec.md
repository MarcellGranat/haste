# herdr-link Specification

## Purpose
Define how the haste daemon connects to herdr on the local and remote machines to observe agent status and deliver prompts.

## Requirements

### Requirement: Local machine over the herdr socket
The daemon SHALL observe the local herdr machine, labelled `local`, through its unix socket API using the JSON request/response protocol. The socket path SHALL default to `~/.config/herdr/herdr.sock` and MAY be overridden by `HASTE_HERDR_SOCKET`.

#### Scenario: Local machine connects by default
- **WHEN** the daemon starts and the local herdr socket exists
- **THEN** the daemon opens a connection to it and reports the `local` machine as connected

#### Scenario: Missing socket
- **WHEN** the local socket does not exist
- **THEN** the machine is reported as unreachable and the link retries with backoff

### Requirement: Remote machines through the hub's herdr
The daemon SHALL observe every enabled saved herdr machine by running `herdr --machine <label> api snapshot` through the hub's herdr CLI at a constant interval of 15 seconds, and SHALL derive status transitions by comparing consecutive snapshots. The daemon SHALL NOT run `ssh` itself and SHALL NOT require forwarded sockets.

#### Scenario: Remote snapshot polled
- **WHEN** machine `m1` is enabled and reachable
- **THEN** the daemon takes a snapshot of `m1` every 15 seconds and updates that machine's agent records from it

#### Scenario: Transition found by diff
- **WHEN** a pane is `working` in one snapshot of `m1` and `blocked` in the next
- **THEN** the daemon records a transition to `blocked` for that pane

### Requirement: Machine discovery from herdr
The daemon SHALL discover machines from `herdr machine list --json` at start and every 5 minutes. Machines that are not enabled SHALL NOT be observed. A machine that disappears from the list or becomes disabled SHALL be dropped together with its agent rows.

#### Scenario: Enabled machines observed
- **WHEN** the list contains machines `m1`, `m2` and `mbp`, all enabled
- **THEN** the daemon observes `local` plus those three

#### Scenario: Disabled machine ignored
- **WHEN** `m2` is disabled in herdr
- **THEN** the daemon stops polling `m2` and shows no rows for it

### Requirement: Only fixed, non-interactive herdr invocations
The daemon SHALL construct herdr CLI calls only in four shapes: `machine list --json`, `--machine <label> api snapshot`, `agent prompt <name> <text>` and `--machine <label> agent prompt <name> <text>`. It SHALL NOT construct any call that adds, removes, enables, disables, renames or reconnects a machine, reads pane output, sends keys, or changes workspaces, tabs or panes. Every call SHALL run with standard input closed and a hard timeout of 20 seconds after which the process is killed.

#### Scenario: Forbidden call not constructible
- **WHEN** the herdr command builder is compiled
- **THEN** it has no variant for machine management, pane read, send keys, start, rename or focus

#### Scenario: A prompt for authentication cannot block
- **WHEN** a `--machine` call would wait for an SSH password or host-key answer
- **THEN** the call fails at the timeout, the process is killed, and the failure counts as one failed attempt

### Requirement: Unreachable machines are reported, not guessed
A machine SHALL be marked unreachable after 2 consecutive failed snapshot calls and SHALL return to connected after one successful call. While unreachable it SHALL show no agent rows as current. Failure retries SHALL back off from 15 seconds to a cap of 5 minutes. State changes SHALL be reported once per transition, not once per attempt.

#### Scenario: Two failures mark unreachable
- **WHEN** two consecutive snapshot calls for `m1` fail
- **THEN** `m1` is reported as unreachable once and its agent rows are not shown as current

#### Scenario: One success recovers
- **WHEN** a snapshot call for an unreachable `m1` succeeds
- **THEN** `m1` is reported connected and its rows are replaced by the snapshot

### Requirement: Snapshot on connect and on reconnect
For the local machine, on every successful connection the link SHALL request a snapshot before applying any subscription event and SHALL replace the stored agent state for that machine with the snapshot contents atomically. For a remote machine, each successful snapshot SHALL replace that machine's stored state atomically.

#### Scenario: Stale blocked flag cleared on reconnect
- **WHEN** an agent was `blocked` before a disconnect and the snapshot after reconnect reports it `working`
- **THEN** the stored state for that agent becomes `working` and no `blocked` record remains

#### Scenario: Events during snapshot are not lost
- **WHEN** a `pane_agent_status_changed` event arrives between subscribing and receiving the snapshot
- **THEN** the event is applied after the snapshot and the final state reflects it

### Requirement: First snapshot is a baseline
The first snapshot of a machine after daemon start, reconnect or recovery SHALL fill the stored state without producing transition notices or `herdr.blocked` audit records for agents already in a state.

#### Scenario: Startup does not announce existing blocked agents
- **WHEN** the daemon starts while two agents are already `blocked`
- **THEN** both appear in the panel and no blocked notice is raised for them

### Requirement: Status subscription for the local machine
After the protocol check, the local link SHALL subscribe with `events.subscribe`. Because herdr's `pane.agent_status_changed` subscription is per pane and requires a `pane_id`, the link SHALL send one such subscription for every pane in the latest snapshot, plus the pane-less layout subscriptions (`pane.created`, `pane.closed`, `pane.exited`, `pane.agent_detected`, `workspace.created`, `workspace.closed`, `workspace.renamed`) whose events only cause a fresh snapshot to be read. When a fresh snapshot shows a different set of panes, the link SHALL renew its subscriptions. The link SHALL NOT subscribe to pane output, pane read, or any output-matching kind. Every subscription the link sends SHALL carry all fields the pinned schema requires for its type.

#### Scenario: One status subscription per pane
- **WHEN** the snapshot contains three panes
- **THEN** the subscribe request holds three `pane.agent_status_changed` subscriptions, each with its own `pane_id`, plus the layout subscriptions

#### Scenario: New pane is watched
- **WHEN** a pane is created after the link subscribed
- **THEN** a fresh snapshot is read, the subscriptions are renewed, and the new pane's status is watched

#### Scenario: Status event applied
- **WHEN** herdr emits `pane.agent_status_changed` with `agent_status` `blocked` for a pane
- **THEN** the stored status of that pane's agent becomes `blocked`

#### Scenario: Requests satisfy the schema
- **WHEN** the link builds its subscription request
- **THEN** every subscription includes every field the committed schema marks required for its type

#### Scenario: Output events never requested
- **WHEN** the link builds its subscription request
- **THEN** the request contains no pane output or pane read kind

### Requirement: Bounded reconnect backoff for the socket
After a lost socket connection the link SHALL retry with exponential backoff with jitter, starting at 1 second and capped at 60 seconds. The backoff SHALL reset only after a connection has streamed healthily for at least 5 minutes.

#### Scenario: Backoff capped
- **WHEN** the socket stays unavailable for an extended period
- **THEN** successive retry delays never exceed 60 seconds

#### Scenario: Flapping connection does not reset backoff
- **WHEN** a connection drops again within 5 minutes of being established
- **THEN** the next retry delay continues from the previous backoff value rather than restarting at 1 second

### Requirement: Protocol pinning and loud mismatch
The link SHALL pin one herdr protocol number, currently 22, and SHALL check it against the protocol reported by the socket server, against the `protocol` field of every snapshot, and against a committed copy of the exported API schema. On a mismatch the machine SHALL NOT be observed, unknown message shapes SHALL NOT be parsed by guessing, a visible notice SHALL name the expected and observed numbers, and a `herdr.link` audit entry SHALL be recorded.

#### Scenario: Protocol mismatch refuses observation
- **WHEN** a snapshot or the server reports protocol 23 while the pinned protocol is 22
- **THEN** that machine is not observed, a notice names both numbers, and it is shown as `protocol mismatch`

#### Scenario: Unknown required field
- **WHEN** a response lacks a field that the pinned schema marks as required
- **THEN** that message is dropped, counted in the audit log, and does not change stored state

### Requirement: Agent name mapping for prompts
A haste principal id SHALL map to a herdr agent name only if it matches the pattern `[a-z][a-z0-9_-]{0,31}`. A principal whose id does not map SHALL NOT receive a prompt.

#### Scenario: Valid name maps
- **WHEN** the principal id is `agent-a`
- **THEN** the herdr agent name `agent-a` is used as the prompt target

#### Scenario: Invalid name refused
- **WHEN** the principal id is `Agent A`
- **THEN** no prompt is sent and the refusal is audited

### Requirement: Read-only use of herdr apart from result prompts
The link SHALL NOT read pane output, send keys, start, rename, focus or wait on agents, or change workspaces, tabs or panes. The only request that writes to a pane SHALL be `agent.prompt` carrying a proposal result message.

#### Scenario: Prompt only for results
- **WHEN** the daemon issues `agent.prompt`, locally or through `--machine`
- **THEN** the text is a proposal result message and no other write reaches a pane
