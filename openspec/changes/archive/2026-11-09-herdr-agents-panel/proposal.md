## Why

Haste already gates agent and machine requests and delivers proposal results back to agents, but it is blind to what those agents are doing in herdr. The human has to switch to herdr to see which agent is working, idle, or stuck on an approval dialog, and haste proposals waiting for a human sit in a different place from agent panes that are blocked waiting for a human. The human works from one hub computer whose herdr already reaches three other machines over saved SSH connections, so haste can see all of them through that one herdr. Herdr's workspace names are also a natural project identifier: an agent working in a workspace can say which project a proposed action belongs to without the human sorting it by hand.

## What Changes

- Add a `haste-herdr` crate with two feeds into one status model. **Local machine:** the herdr unix socket (`~/.config/herdr/herdr.sock`): a `snapshot` on connect and on every reconnect, then one long-lived `events.subscribe` for agent status changes. **Remote machines:** the hub's herdr CLI, `herdr --machine <label> api snapshot`, polled at a constant interval (15 s) and diffed to find transitions. Remote machines are discovered from `herdr machine list --json` (enabled ones only). No forwarded sockets, no `ssh` run by haste itself, no mutating machine command, and no authentication prompt (stdin closed, hard timeout per call).
- Pin the herdr protocol number (currently 22) and the request/event types to the schema exported by `herdr api schema --json`; every snapshot also carries its protocol number and is checked against the pin. On a mismatch the machine stays unobserved and a visible notice names the expected and observed protocol.
- Track each herdr agent's status (`idle`, `working`, `blocked`, `done`, `unknown`) grouped by machine and workspace, as observed state held by the daemon, and add an Agents panel to the TUI. A machine that is unreachable or in protocol mismatch is shown as such, never with stale rows.
- Treat a herdr **workspace label** as a project tag. The set of known tags is discovered from the snapshots of all machines (the same label on two machines is one project; the machine is recorded separately). Threads carry project tags set by the human; no folder scanning and no new filesystem or SSH access path is added.
- Attribute proposals to a project: when an agent proposes an action from a herdr pane, the haste CLI sends two hints (`HERDR_PANE_ID` and `HASTE_MACHINE`, default `local`); the daemon resolves them against its snapshot to the pane's workspace label and stores it on the proposal as `project`, together with the `origin_machine`. This is routing, never identity. Unresolvable hints give no project, not an error.
- Make the Approvals view group and filter by project as well as by agent, and show agents that are `blocked` as a separate read-only group, so every item waiting for the human is in one place. (The "task is done" proposal payload itself arrives with the later reminders change; this change carries the project tag it will need.)
- Narrow what a machine principal (`PrincipalKind::Machine`, read-only) can read: only threads tagged with a workspace label currently or last known on its own machine. The scope filter is enforced in the daemon read path, never only in the UI.
- Deliver proposal results (`sent`, `rejected`, `expired`) with `agent.prompt`: over the socket for the local machine, and through `herdr --machine <label> agent prompt` for a remote one (the proposal's `origin_machine`), with the existing bounded three-attempt backoff. A principal with no valid herdr agent name is refused with an audited error.
- Raise haste `Notice` events for agent-side happenings, rate limited per agent so a chatty agent cannot flood the TUI. The first snapshot of a machine raises no transition notices. Audit entries for link changes and transitions to `blocked` record counts and identifiers only, never pane text.
- Agent identity stays token based. The herdr pane id, agent name, machine and workspace label are hints only.

### Non-goals

- Reading pane output (`pane.read`) and sending keystrokes to panes. The only write to a pane is the `agent.prompt` result message.
- Creating, closing, or re-arranging herdr panes, tabs, or workspaces, or adding, removing, enabling or reconnecting saved machines.
- How the haste CLI running on a *remote* machine reaches the hub's daemon. This change defines what the daemon does with the machine and pane hints it receives; the transport from a remote agent to the hub is a separate concern.
- Folder scanning for tags, and any way for agents to set tags (including a "tag" proposal). Tags are set by the human only in this change.
- Tags on reminder or todo records, and the "task done" proposal payload: both belong to the later reminders change. Only threads carry tags now.
- Multi-user or remote haste daemons: machines are herdr machines reached through the hub's herdr.

## Capabilities

### New Capabilities
- `herdr-link`: the local socket client (snapshot, subscription, reconnect), the remote CLI snapshot feed with machine discovery, the allowed herdr invocations, protocol pinning and mismatch handling, the first-snapshot rule, and the `agent.prompt` delivery transport with its principal-to-agent-name mapping.
- `agents-panel`: the daemon-held agent status model grouped by machine and workspace, the TUI Agents panel, machine connection states, and rate limiting of agent-side notices.
- `machine-scope`: project tags as workspace labels, thread tags set by the human, and the scope filter applied to machine principal reads in the daemon.

### Modified Capabilities
- `proposals`: result delivery uses the socket (local) or the hub's herdr CLI (remote) instead of a plain CLI call; proposals gain `project` and `origin_machine` from the agent's herdr hints.
- `policy-gate`: machine principals gain a tag scope on read actions, applied before any read result leaves the daemon.
- `approvals-tui`: the Approvals view groups and filters by project and gains a separate read-only group for blocked herdr agents.
- `audit-log`: adds entries for herdr link state changes, transitions to `blocked`, and tag changes, recorded as counts and identifiers only.

## Impact

- **New code**: `crates/haste-herdr` (socket client, CLI feed, protocol types checked against the exported schema, subscription state machine); `crates/hasted` (link supervisor, status store, scope filter in the read path, project resolution, notifier); `crates/haste-core` (tag and scope types, agent status model, proposal fields); `crates/haste-proto` (hints on `Propose`, read requests for agent status and tags, tag-change requests); `crates/haste-tui` (Agents panel, project grouping, blocked group); `crates/haste` (agent CLI sends the two hints; human-build verbs to manage tags).
- **Store**: one forward-only migration adds nullable `scope_tags` to threads, `project` and `origin_machine` to proposals, and a table of the last known workspace labels per machine. No backfill.
- **Dependencies**: no new runtime dependency is expected beyond the existing serde and tokio stack.
- **External systems**: the local herdr unix socket and exactly four `herdr` command shapes (`machine list --json`, `--machine <label> api snapshot`, `agent prompt`, `--machine <label> agent prompt`). SSH is used only by herdr itself, through the user's existing saved machines.
- **Security surface**: the daemon is the only process that talks to herdr. Machine tokens still cannot approve or send; their reads are narrowed to tagged items; herdr text (pane output) is never read or stored; the `agent.prompt` result message is the only write to panes; hints never grant identity or capability.
- **Documentation**: README sections for the herdr link, the Agents panel, project tags, and the agent-side hints.
