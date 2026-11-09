## Why

Haste is meant to be a personal email/chat/todo hub that agents can operate on, but nothing exists yet to hold state, mediate agent access, or keep a human in the loop for side-effecting actions. Without a core, every future connector and agent integration would invent its own storage, auth, and approval logic. This change builds that core once, with the policy gate as the central control point, before any real connector is wired in.

## What Changes

- Add a long-running daemon (launchd agent) that owns a SQLite store and exposes a Unix socket in a `0700` directory. Thin clients (the TUI shell and the `haste` CLI) talk only to the daemon.
- Add a unified data model: `Thread` (with `source`), `Message`, `Person`, local triage state (`priority`, `snoozed_until`, `set_aside`, `archived`), and typed links between objects.
- Add a policy gate with a capability matrix per agent and per herdr machine. Each action resolves to an outcome of `auto`, `pending`, or `denied`. Decided matrix:
  - `read` = auto
  - `draft` = pending
  - `archive` / `snooze` = pending (no auto execution, no undo log)
  - `reminder.create` = pending
  - `note.append` = auto only inside allow-listed paths (`people/**`, `tasks/**`); the daemon performs the append-only write with an attribution comment, enforces path-escape and symlink checks, and agents cannot edit or delete notes
  - `send` is not present in the agent CLI at all; it exists only as a human TUI action
- Add a proposal workflow: the agent CLI returns a pending proposal ID immediately (non-blocking) and accepts an idempotency key. Proposals move through `pending` → `approved` → `sent` (human action), and can end in `rejected`, `expired`, or `superseded`. Stale-thread detection flags proposals whose source thread changed after proposal creation. Approve and send are separate human actions. Results are delivered back to the agent later via herdr `agent.prompt` and are also queryable via `haste proposal status`.
- Add an approvals TUI view with vi-style keys, batch approval, and per-agent grouping.
- Add an append-only JSONL audit log of every gate decision, proposal transition, and daemon write.
- Add per-agent tokens issued by the daemon. The herdr pane environment variable is only a hint and is never trusted as identity.
- Add a Connector trait with capability flags (`read`, `draft`, `send`, `archive`), a per-connector kill switch, and a circuit breaker that stops on challenge/checkpoint responses and never retry-loops.
- **BREAKING**: none (greenfield repository).

### Roadmap (out of scope, stubs only)

Each of the following is a later change and is listed here only as a placeholder. No code, schema, or policy entries for them are introduced by this change:

- Gmail connector (`connector-gmail`)
- iCloud IMAP connector (`connector-icloud-imap`)
- herdr panel integration (`herdr-panel`)
- Apple Reminders connector (`connector-reminders`)
- iMessage connector (`connector-imessage`)
- Calendar connector (`connector-calendar`)
- Obsidian vault connector (`connector-obsidian`)
- WhatsApp connector (`connector-whatsapp`)
- Messenger connector (`connector-messenger`)
- Unified inbox across sources with per-source filter (`unified-inbox`)

## Capabilities

### New Capabilities
- `daemon-ipc`: launchd-managed daemon, Unix socket in a `0700` directory, SQLite store lifecycle, and thin client protocol (TUI shell and `haste` CLI).
- `data-model`: `Thread{source}`, `Message`, `Person`, local triage state, and typed links between objects.
- `policy-gate`: capability matrix per agent and per herdr machine, `auto | pending | denied` outcomes, and the decided action rules including the `note.append` allow-list and path/symlink safety.
- `agent-auth`: daemon-issued per-agent tokens, token scope binding, and the rule that herdr pane environment is only a hint.
- `proposals`: non-blocking proposal creation with idempotency keys, lifecycle states (`pending`, `approved`, `sent`, `rejected`, `expired`, `superseded`), stale-thread detection, result delivery to agents, and `haste proposal status`.
- `approvals-tui`: approvals view with vi keys, batch approval, per-agent grouping, and the human-only `send` action.
- `audit-log`: append-only JSONL audit log of gate decisions, proposal transitions, and daemon writes.
- `connector-framework`: Connector trait with `read`/`draft`/`send`/`archive` capability flags, per-connector kill switch, and circuit breaker on challenge/checkpoint.

### Modified Capabilities
<!-- None: the repository has no existing specs. -->

## Impact

- **New code**: Rust workspace (`haste`) with crates for the daemon, shared protocol and data model, the policy gate, the TUI shell (ratatui, crossterm), and the CLI (clap). Async runtime is tokio; persistence uses rusqlite; serialization uses serde.
- **Dependencies**: ratatui, crossterm, tokio, rusqlite, clap, serde (and serde_json for JSON/JSONL).
- **Filesystem**: launchd plist for the daemon; Unix socket in a `0700` runtime directory; SQLite database file; append-only JSONL audit log; note files under `people/**` and `tasks/**` written only by the daemon.
- **External systems**: herdr `agent.prompt` is used to deliver proposal results back to agents. No external email, chat, or calendar service is contacted in this change.
- **Security surface**: the daemon is the only writer; agents never receive `send` capability; tokens are the sole identity source.
