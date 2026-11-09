## 1. Protocol crate, schema pinning and the command builder

- [x] 1.1 Create `crates/haste-herdr` with a `HERDR_PROTOCOL` constant set to 22 and a module layout for socket client, CLI feed, protocol types, and the subscription state machine
- [x] 1.2 Commit a copy of `herdr api schema --json` under `crates/haste-herdr/schema/` and document how to refresh it
- [x] 1.3 Define request, event and snapshot types for `agent.prompt`, `snapshot`, `events.subscribe`, `pane_agent_status_changed` and the pane and workspace lifecycle kinds, with no variant for pane read, send keys, start, rename, or focus
- [x] 1.4 Add a test that checks every type and method haste uses exists in the committed schema with matching required fields and enum values
- [x] 1.5 Implement a `HerdrCommand` builder that can construct only `machine list --json`, `--machine <label> api snapshot`, `agent prompt` and `--machine <label> agent prompt`, runs each with stdin closed and a 20 s kill timeout, and has no variant for machine management or pane operations
- [x] 1.6 Implement protocol checks for the socket server and the `protocol` field of every snapshot, returning a typed mismatch error that names the expected and observed numbers

## 2. Local feed: socket client and fake server

- [x] 2.1 Implement newline-delimited JSON request/response over a unix socket with per-request ids, honouring `HASTE_HERDR_SOCKET`
- [x] 2.2 Build a fake unix socket server test helper that replays scripted snapshots and events, injects disconnects, and returns protocol errors
- [x] 2.3 Implement connect, snapshot, subscribe sequencing with buffering of events that arrive before the snapshot response
- [x] 2.4 Implement reconnect with exponential backoff with full jitter, base 1 s, cap 60 s, reset only after 5 minutes of healthy streaming
- [x] 2.5 Test: snapshot replaces stale state atomically after reconnect
- [x] 2.6 Test: events during snapshot are applied after it and not lost
- [x] 2.7 Test: backoff never exceeds 60 s and flapping connections do not reset it
- [x] 2.8 Test: a response missing a required field is dropped and counted, without changing state

## 3. Remote feed: the hub's herdr CLI

- [x] 3.1 Build a fake `herdr` executable test helper (a script in a temp directory, selected with `HASTE_HERDR_BIN`) that prints scripted machine lists and snapshots, fails on demand, hangs on demand, and records every argument list it receives
- [x] 3.2 Implement machine discovery from `herdr machine list --json` at start and every 5 minutes, observing enabled machines only and dropping a removed or disabled machine with its rows
- [x] 3.3 Implement the per-machine poll loop (`REMOTE_POLL_SECS = 15`) that parses snapshots, checks the protocol pin, and diffs consecutive snapshots into transitions
- [x] 3.4 Mark a machine unreachable after 2 consecutive failures and connected after one success, with failure backoff from 15 s to a 5 minute cap, reporting each state change once
- [x] 3.5 Treat the first snapshot after start, reconnect or recovery as a baseline with no transition output
- [x] 3.6 Test: a hanging call is killed at the timeout and counts as one failure
- [x] 3.7 Test: the recorded argument lists contain only the four allowed shapes across a full run
- [x] 3.8 Test: a disabled machine is no longer polled and its rows disappear

## 4. Daemon status model and link supervisor

- [x] 4.1 Add `HerdrMachine` and `AgentStatusRecord` types in `haste-core` keyed by machine label, workspace id, and pane id with the workspace label, and no pane text, title, working directory, or label-value fields
- [x] 4.2 Add the in-memory agent status store in `hasted` with atomic per-machine replacement
- [x] 4.3 Add the link supervisor task in `hasted` that runs the local feed and one remote poller per discovered machine and writes to the status store
- [x] 4.4 Persist the last known workspace-label set per machine on every snapshot
- [x] 4.5 Emit `Event::Notice` for link state transitions only (connected, unreachable, mismatch), with source `herdr:<machine>`
- [x] 4.6 Test with the fakes: a status event sets `blocked`, an unreachable machine shows no current rows, a mismatch raises a notice and no subscription

## 5. Notices, rate limiting and audit

- [x] 5.1 Implement a per-(machine, agent, kind) token bucket with burst 3 and refill 1 per 30 s, counting suppressed notices
- [x] 5.2 Coalesce blocked-transition notices to one per machine per 10 s and raise none for baseline snapshots
- [x] 5.3 Add audit kinds `herdr.link`, `herdr.blocked`, `herdr.prompt` and `tag.change` with counts and identifiers only
- [x] 5.4 Test: 50 rapid changes produce at most 3 immediate notices plus a suppressed count
- [x] 5.5 Test: three blocked transitions within 10 s produce one notice
- [x] 5.6 Test: starting with two agents already blocked raises no notice and no `herdr.blocked` record
- [x] 5.7 Test: audit records contain no title, label value, working directory, or pane text

## 6. Project tags and machine scope

- [x] 6.1 Add a `ProjectTag` newtype in `haste-core` validating 1 to 64 characters with no control characters
- [x] 6.2 Add a forward-only migration with a nullable `scope_tags` column on threads, nullable `project` and `origin_machine` columns on proposals, and a table of last known workspace labels per machine, with no backfill
- [x] 6.3 Compute the known tag set from the status store plus the persisted label sets
- [x] 6.4 Implement the pure function `visible_to(principal, thread_tags, machine_labels)` in `haste-core` with unit tests for human, machine, unknown machine, and agent principals
- [x] 6.5 Implement `scope_filter(principal, items)` in `hasted` and route every read of threads, proposals, and agent status through it before serialisation
- [x] 6.6 Test: a machine principal lists threads and receives exactly those tagged with its machine's labels
- [x] 6.7 Test: a machine principal with no visible objects, or an id matching no machine, receives an empty result, not an authorisation error
- [x] 6.8 Add an integration test that enumerates every read request for every principal kind and fails if one bypasses the filter
- [x] 6.9 Test: a matching tag does not change the gate outcome for `draft` or any other write

## 7. Project attribution of proposals

- [x] 7.1 Add optional `project` and `origin_machine` to the proposal type and the wire `Proposal`, and optional machine and pane hints to `Propose`
- [x] 7.2 Make the agent CLI send `HERDR_PANE_ID` and `HASTE_MACHINE` (default `local`) as hints, with no change to token handling
- [x] 7.3 Resolve the hints in the daemon against the status model, storing the pane's workspace label as `project` and the machine as `origin_machine`; unknown hints give neither and no error
- [x] 7.4 Test: an agent in workspace `haste` on `local` creates a proposal that stores `project = haste`
- [x] 7.5 Test: an unknown pane or machine creates the proposal without a project
- [x] 7.6 Test: hints never change the principal, capability, or gate outcome

## 8. Result delivery

- [x] 8.1 Implement `agent_name_for(principal)` with the pattern `[a-z][a-z0-9_-]{0,31}` and a typed mapping error
- [x] 8.2 Implement `SocketNotifier` that sends `agent.prompt` with `target` and `text` and no `wait` option for local agents
- [x] 8.3 Route a proposal whose `origin_machine` is remote through `herdr --machine <label> agent prompt`
- [x] 8.4 Keep the three-attempt backoff and per-failure audit, and use the plain CLI only while the local link is disconnected or in mismatch
- [x] 8.5 Test: a local principal receives one prompt over the fake socket with the proposal id and result
- [x] 8.6 Test: a remote-origin proposal is delivered with the `--machine` form and not over the local socket
- [x] 8.7 Test: an invalid principal id sends no prompt and the refusal is audited
- [x] 8.8 Test: three failed attempts stop further attempts and leave the result available through `haste proposal status`

## 9. Human-only administration

- [x] 9.1 Add human-build verbs `haste tag add <thread> <tag>`, `haste tag remove <thread> <tag>`, `haste tag list`, and `haste machine status`, absent from the agent build
- [x] 9.2 Add the matching daemon requests, accepting only tags in the known set, and deny them for every agent and machine principal with an audited denial
- [x] 9.3 Audit every tag change with thread id, tag, and acting principal

## 10. TUI: Agents panel, project grouping and blocked group

- [x] 10.1 Add daemon read requests for agent status records and machine states, both passing through the scope filter
- [x] 10.2 Add the Agents panel in `haste-tui` grouped by machine then workspace label, showing status and time in status, updating from daemon events
- [x] 10.3 Show unreachable and protocol-mismatch machines as their own state without stale agent rows
- [x] 10.4 Add the group-by-project mode to the Approvals view with a `no project` group, show the project on each row, and extend the filter to match it
- [x] 10.5 Add the read-only `Blocked agents` group to the Approvals view ordered by time in `blocked`, excluded from batch selection and confirmation counts
- [x] 10.6 Test the view model: blocked entries appear and disappear with events, project grouping and filtering work, and the batch count excludes blocked entries

## 11. Documentation and verification

- [x] 11.1 Add README sections for the herdr link (local socket and remotes through the hub), protocol pinning and refresh, the Agents panel, project tags, and the agent-side hints (`HASTE_MACHINE`, `HERDR_PANE_ID`)
- [x] 11.2 Document the security boundary in README: no pane read, no keystrokes except proposal results, token identity only, herdr hints are routing not identity, and that remote agents' route to the hub's daemon is out of scope
- [x] 11.3 Run `cargo test` and confirm no test requires a real herdr process or SSH
- [x] 11.4 Run `cargo clippy` with strict lints and resolve all warnings in the new and changed crates
- [x] 11.5 Run `openspec validate herdr-agents-panel --strict` and resolve any issues
- [x] 11.6 Manual smoke check against the real hub: the Agents panel lists `local`, `m1`, `m2` and `mbp`, a blocked agent shows in Approvals, and a proposal from a local agent carries its workspace as project
