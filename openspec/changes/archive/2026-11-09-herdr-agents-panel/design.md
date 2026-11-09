## Context

Haste's daemon (`hasted`) owns the store, the policy gate, the proposal lifecycle, the connector scheduler and the `Event` stream that the TUI consumes. Result delivery to agents currently goes through `crates/hasted/src/notifier.rs`, which shells out to `herdr agent prompt <agent> <message>` with a bounded three-attempt backoff. Nothing in haste observes herdr state.

Facts verified on the user's machine (herdr 0.9.5, protocol 22):
- One **hub** computer runs herdr with three saved SSH machines (`herdr machine list --json` returns `id`, `label`, `target`, `session`, `enabled`). All three report reachable.
- `herdr --machine <label> api snapshot`, run on the hub, returns that remote's snapshot (workspaces, panes, agents, and a `protocol` field) over the existing SSH setup. No socket forwarding is involved.
- A workspace record has `label`, `workspace_id`, `agent_status`, `pane_count`, `tab_count`; a pane record has `pane_id`, `workspace_id`, `cwd`, `agent_status`. The workspace **label** is the workspace's name, and it is not the same as the folder name (0 of 6 panes matched), so project tags come from labels.
- The local herdr exposes a unix socket (`~/.config/herdr/herdr.sock`) with request methods including `agent.prompt` (`target`, `text`), `events.subscribe` and `snapshot`, and a `pane_agent_status_changed` event with `pane_id`, `workspace_id`, `agent_status`. Streaming through `--machine` has not been observed, so remotes are polled.

Constraints from the existing system: agent and machine identity is token based (`agent-auth`); the daemon is the only process that talks to external systems; audit entries carry counts and ids, never content; the human-only admin surface is compiled only in the human CLI build; all wire types live in `haste-proto`.

## Goals / Non-Goals

**Goals:**
- One status model fed by a live socket for the local machine and by polled snapshots for each remote, recovering from disconnects without keeping stale state.
- Typed herdr bindings checked against the exported schema, with a pinned protocol number and a loud failure on mismatch.
- Workspace labels as project tags; proposals attributed to the agent's workspace; an Approvals view organised by project; a machine scope enforced in the daemon.
- Result delivery through `agent.prompt` locally and through the hub's herdr for remote machines.
- Tests that run without a real herdr (a fake unix socket server and a fake `herdr` executable).

**Non-Goals:**
- `pane.read` or any read of pane text; any keystroke; any machine, workspace, tab or pane management.
- Transport from a remote agent's haste CLI to the hub's daemon.
- Tags settable by agents, tags on reminder/todo records, the "task done" payload, folder scanning.

## Decisions

### D1. Crate boundary: `haste-herdr` owns the herdr I/O, `hasted` owns state

`crates/haste-herdr` contains the socket client, the CLI feed, the protocol types and the allowed-invocation builder. It has no dependency on `hasted` or the store; it yields `MachineUpdate` values (`Snapshot`, `Transition`, `State(Connected|Unreachable|Mismatch)`) per machine label. `hasted` owns the supervisor, the status store, the scope filter, project resolution and the notifier. Store writes stay in one place and the herdr code is testable against fakes.

*Alternative considered:* put the client in `hasted`. Rejected: a separate crate lets the fake-server and schema tests run without the daemon.

### D2. Protocol types are checked against the exported schema

`HERDR_PROTOCOL: u32 = 22` is a constant in `haste-herdr`. A committed copy of the schema (`crates/haste-herdr/schema/herdr-api-22.json`, from `herdr api schema --json`) is checked by a test that asserts every method, event and field haste uses exists with compatible names and enum values. At runtime the pin is checked twice: against the protocol the socket server reports, and against the `protocol` field of **every** snapshot (socket or CLI). A mismatch puts that machine in `Mismatch`: no subscription, no parsing of unknown shapes by guessing, a visible `Notice` naming both numbers, and a retry only on daemon restart or a human request. A message missing a required field is dropped and counted in the audit log without changing state.

*Trade-off:* the committed schema must be refreshed when herdr upgrades. That is intended: the refresh is a reviewed change, not silent drift.

### D3. Local machine: socket, snapshot first, then events

The lifecycle is connect → `snapshot` → `events.subscribe` (agent status plus the pane and workspace lifecycle kinds needed to drop closed panes) → apply events. Events arriving between subscribe and the snapshot response are buffered and replayed after the snapshot, deduplicated by `pane_id` and status order (the subscription event carries no sequence number; a transition missed in that narrow window is an accepted trade-off for a status display). A snapshot on every reconnect replaces that machine's state atomically, so a `blocked` flag from before a disconnect cannot survive. Reconnect uses exponential backoff with full jitter, base 1 s, cap 60 s, reset only after 5 minutes of healthy streaming. State changes are reported once per transition, never per retry.

### D4. Remote machines: the hub's herdr CLI, polled

For each enabled machine from `herdr machine list --json` (re-read every 5 minutes and at start; a disabled or removed machine is dropped with its rows), the supervisor runs `herdr --machine <label> api snapshot` every `REMOTE_POLL_SECS = 15`, parses it, checks the protocol pin and diffs it against the previous snapshot to produce transitions. Safety properties, all enforced in one `HerdrCommand` builder:
- Only four command shapes can be constructed: `machine list --json`, `--machine <label> api snapshot`, `agent prompt <name> <text>` and `--machine <label> agent prompt <name> <text>`. There is no variant for `machine add/remove/enable/disable/reconnect/rename`, `pane`, `agent send-keys` and the like.
- stdin is closed and every call has a hard timeout (20 s, process killed), so an SSH password or host-key prompt can never block the daemon. A call that fails or times out counts as one failure.
- A machine is `Unreachable` after 2 consecutive failures and back to `Connected` after one success; per-machine failure backoff grows from 15 s to a 5 minute cap. State changes are reported once, not per poll.
- The **first** snapshot after start, reconnect or recovery is a baseline: it fills the store but raises no transition notices, so starting haste does not announce every already-blocked agent.

Reachability is judged by whether the snapshot call succeeds, not by parsing `herdr machine status` text.

*Alternatives considered:* operator-managed forwarded sockets (rejected: the CLI already reaches the remotes, so nothing needs forwarding); running `ssh` from haste (rejected: a second credential path to maintain).

### D5. Status store and what is kept

An in-memory model in `hasted`: per machine a connection state and a set of agent records `{workspace_id, workspace_label, pane_id, agent, status, status_since}`. It is not written to proposal tables. No pane text, titles, `cwd` or `state_labels` are stored; they are dropped at ingestion. The only persisted herdr-derived data is, per machine, the **last known set of workspace labels** (so tagging and scope still work while a machine is briefly unreachable, and after a restart).

### D6. Project tags are workspace labels

A tag is a workspace label: 1 to 64 characters, no control characters, compared exactly (case sensitive). The known tag set is the union of the labels in the latest snapshots plus the persisted last-known sets. The same label on several machines is one project; the machine is a separate attribute. Threads carry `scope_tags`, a JSON array in a nullable column. A thread with no tag is visible only to the human. Only the human can add or remove a tag, through a human-build verb or the TUI, and only a tag in the known set is accepted (so a typo cannot create a phantom project). Agents and machines cannot change tags, and neither can a proposal in this change.

*Alternative considered:* a `machine:` / `project:` prefix family. Rejected after the user's decision: the tag is just the workspace name.

### D7. Scope enforcement in the daemon read path

A pure function `visible_to(principal, object_tags, machine_labels)` lives in `haste-core`; every daemon read handler for threads, proposals and agent status passes its result through one `scope_filter` in `hasted` before serialisation. Rules: the human sees everything; a **machine principal** is identified by its id equalling a herdr machine label and sees a thread only if one of the thread's tags is in that machine's last-known label set (an id matching no known machine sees nothing); an **agent principal** sees its own proposals as today and no other agent's status. A machine with no visible objects gets an empty result, never an error, so hidden objects are not revealed. The filter only narrows reads; machine principals stay read-only under the policy gate. Unit tests cover the pure function; an integration test enumerates every read request for every principal kind.

### D8. Project attribution of proposals

The agent CLI adds two hints to `Propose`: `HERDR_PANE_ID` and `HASTE_MACHINE` (default `local`). The daemon looks the pane up in its snapshot of that machine; if it finds it, it stores the pane's workspace label as `project` and the machine as `origin_machine` on the proposal. If the machine or pane is unknown it stores neither and the proposal is created normally. The hints are routing information: they cannot raise capability, change the principal, or bypass the gate, and an agent that lies about them only mislabels its own proposal, which the human sees and can ignore. A forward-only store migration adds nullable `project` and `origin_machine` to proposals and the per-machine label table, with no backfill.

### D9. Result delivery

For `origin_machine = local` (or none) the notifier sends `agent.prompt` over the socket with `target` = the herdr agent name and `text` = the result message. For a remote origin it runs `herdr --machine <label> agent prompt <name> <text>` through the hub. The agent name comes from `agent_name_for(principal)`, valid only for `[a-z][a-z0-9_-]{0,31}`; a principal that does not map is refused with an audited error and nothing is sent. Three attempts with backoff and an audit entry per failure, as today. The plain local CLI call remains as a fallback only while the local link is down or in mismatch.

### D10. Identity stays token based

Pane id, agent name, machine label and workspace label are hints for display, routing and project attribution. They never authorise a request and never create an identity.

### D11. Approvals: project grouping and the blocked group

The Approvals view can group by agent (today) or by project (a key toggles the mode), and the existing `/` filter also matches the project. A separate read-only **Blocked agents** group lists agents with status `blocked` (machine, workspace label, agent name), oldest first. Blocked entries cannot be marked, approved, rejected or sent and do not count toward batch confirmation.

### D12. Notices and rate limiting

Herdr-originated happenings become `Event::Notice` with source `herdr:<machine>`. A token bucket per (machine, agent, kind) allows a burst of 3 and refills 1 per 30 s; suppressed notices are counted and reported in the next one. Transitions to `blocked` are always audited but their notices are coalesced to one per machine per 10 s. Baseline snapshots raise none.

### D13. Audit

New kinds: `herdr.link` (connected, unreachable, mismatch; machine label and attempt), `herdr.blocked` (machine, workspace id, pane id, count of blocked agents), `herdr.prompt` (proposal id, attempt, outcome) and `tag.change` (thread id, tag, acting principal). No field carries pane text, titles or message bodies.

### D14. Testing without herdr

A fake unix-socket server (scripted snapshots, events, disconnects, protocol errors) tests the local feed. A fake `herdr` executable (a small script in a temp directory, selected through a `HASTE_HERDR_BIN` setting) tests the CLI feed: scripted machine lists and snapshots, a failing call, a hanging call that must be killed at the timeout, and a recorder that proves only the four allowed command shapes are ever run. No test needs a real herdr process or SSH.

### D15. Human-only administration

Human-build verbs `haste tag add <thread> <tag>`, `haste tag remove <thread> <tag>`, `haste tag list` and `haste machine status` (machine states from the daemon) are denied to agent and machine principals by the daemon and absent from the agent CLI build, like the `connector` verbs. Haste has no machine configuration of its own: machines come from herdr.

## Risks / Trade-offs

- [Protocol drift between herdr upgrades] → committed schema, pinned number, per-snapshot check, mismatch stops observation and shows a notice.
- [Lost transitions] → local: snapshot replaces state on reconnect, buffered events replayed. Remote: polling sees the state every 15 s; a status that flips and flips back inside one interval is invisible (accepted for a status display).
- [SSH cost of polling 3 machines] → one short call per machine per 15 s; backoff when failing; the user's SSH connection sharing is their concern.
- [A hung SSH call] → closed stdin, 20 s hard timeout and process kill; failures back off.
- [Scope leak through a missed read path] → one filter choke point and an integration test over every read request and principal kind.
- [A lying agent mislabels its own proposal's project] → hints are routing only; the human sees the project and the agent's identity from the token; no capability depends on the hint.
- [Workspace renamed or closed] → tags are label strings; a renamed workspace is a new label (the old tag stays in the persisted known set until the human removes it).
- [Result delivery during an outage] → fallback for the local machine; remote delivery fails after three audited attempts and the result stays available through `haste proposal status`.

## Migration Plan

1. Add `haste-herdr` and the committed schema, behind a `herdr` cargo feature in `hasted` and `haste` (default on for the human build).
2. Apply the store migration (nullable columns and one small table, no backfill); existing threads stay human-only and existing proposals have no project.
3. Ship the link in observe-only mode first: populate the store, no notices, no delivery change. Verify with the local herdr and then one remote machine.
4. Enable notices, the Agents panel, project grouping and the blocked group.
5. Switch result delivery to the socket and the `--machine` path, keeping the local CLI fallback.

Rollback: disable the `herdr` feature or restart the previous build; the notifier returns to the CLI call, which is today's behaviour. The nullable columns are ignored by older code.

## Implementation Notes

Where the build refined the plan above:

- **No cargo feature.** Herdr observation is a runtime switch (`HASTE_HERDR=0` turns it off) because the crate adds no heavy dependency; the migration plan's `herdr` feature was not needed.
- **Lifecycle events only trigger a snapshot.** The socket feed does not parse pane or workspace lifecycle payloads: any such event schedules a fresh `session.snapshot` (debounced 300 ms), which also picks up renamed workspaces.
- **Two snapshots on connect.** The protocol is checked with a first snapshot *before* subscribing (so a mismatch subscribes to nothing), then a second snapshot is taken after subscribing; buffered events are applied after it.
- **Panes map to workspaces even without an agent.** The status model keeps every pane's workspace label from the snapshot, so a proposal from a pane herdr has not detected an agent in still gets its project.
- **The protocol number is checked first.** A snapshot from another protocol is reported as a mismatch, not as malformed, even if its fields differ.
- **Mismatch recovery.** A local mismatch stops until the daemon restarts; a remote machine in mismatch is looked at again every 5 minutes, so an upgraded or rolled-back herdr recovers by itself.
- **Agents keep unscoped thread reads.** The scope function narrows only machine principals; agents read threads as before (their writes are gated), and see no other agent's status.
- **Status events are per pane (found against the real herdr).** Herdr's `pane.agent_status_changed` subscription requires a `pane_id`, so the feed subscribes once per pane in the snapshot and renews the subscriptions whenever the set of panes changes; the first version assumed one global subscription and real herdr rejected it. The schema test now checks that every subscription haste builds contains all fields the schema requires for its type, and the fake servers reject a pane-less status subscription as herdr does.
- **Labels carry a position prefix (found against the real herdr).** Herdr labels read `[3] haste`; the number is the workspace's position on that machine. The project tag is the name after the marker (`project_name`), so `haste` on two machines is one project and reordering workspaces changes nothing.
- **Audit names.** Result delivery is now audited as `herdr.prompt` (it was `delivery`); refused deliveries use outcome `refused_invalid_name` and are not retried.

## Open Questions

None. The remote-access question is resolved by using the hub's `herdr --machine <label>` (verified), event ordering is accepted as described in D3, and tags attach to threads now with reminders and todo records following in their own change.
