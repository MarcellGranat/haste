## Context

Haste is a greenfield Rust workspace. There is no existing code or spec. The core must serve three kinds of clients: a human TUI shell, a `haste` CLI that agents call from herdr panes, and (later) connectors that touch external services. The daemon must be the single writer to the SQLite store and the only component that decides whether an action proceeds. Agents are semi-trusted: they may read everything, draft and propose, and append to a narrow set of notes, but they must never send and must never be able to bypass the gate by talking to the store or connectors directly.

Constraints:
- macOS only for now, managed by launchd.
- Single user; a handful of local agents and herdr machines.
- Connectors are out of scope, so the Connector trait is defined but has no real implementations, only in-tree test doubles.

## Goals / Non-Goals

**Goals:**
- One daemon owns state, policy, audit, and connector access.
- Every side-effecting agent request goes through the policy gate and produces an audit record.
- Agents get non-blocking proposal IDs and learn outcomes later without polling the TUI.
- Send is a human-only action, enforced by the absence of the command in the agent CLI and by the daemon refusing `send` for agent tokens.
- Unified data model is usable by read paths before any connector exists (tests seed it directly).

**Non-Goals:**
- Any real connector (Gmail, iCloud IMAP, herdr panel, reminders, iMessage, calendar, Obsidian, WhatsApp, Messenger). These are roadmap stubs in the proposal.
- Unified inbox rendering across sources beyond the data model support (per-source filter is a later view change).
- Undo log for archive/snooze. Explicitly excluded by the decided matrix.
- Multi-user support, remote (non-local) access, or network listeners.
- Encryption at rest for the SQLite file beyond filesystem permissions.

## Decisions

### D1. Daemon with Unix socket, thin clients
The daemon runs under launchd (`~/Library/LaunchAgents/`), listens on a Unix socket inside a directory created with mode `0700` (for example under `~/Library/Application Support/haste/run/`). Clients speak a length-prefixed JSON request/response protocol defined in a shared `haste-proto` crate using serde. Streaming updates (TUI refresh) use a subscribe request that keeps the connection open.

*Alternatives considered:* a TCP localhost port (rejected: any local process can connect, and authentication becomes harder); embedding SQLite access directly in each client (rejected: concurrent writers and no single gate point); gRPC (rejected: extra dependency for a local-only protocol).

### D2. SQLite via rusqlite, single writer task
The daemon opens one SQLite connection in a dedicated blocking task fed by a bounded channel; all writes are serialized through it. WAL mode is enabled. Reads may use a small pool of read-only connections. Schema is versioned with an integer `user_version` and forward-only migrations.

*Alternatives considered:* sqlx (async, but adds a heavier dependency and compile-time query checking not needed here); sled (rejected: less mature tooling for ad hoc inspection).

### D3. Unified data model
Tables: `threads` (id, source, external_id, subject, participants, last_activity_at), `messages` (id, thread_id, source, external_id, sender_person_id, body_text, sent_at), `people` (id, display_name, identifiers as JSON array), and `triage` (object_id, object_kind, priority, snoozed_until, set_aside, archived, updated_at). `source` is a closed enum on the Rust side with an `Other(String)` escape hatch for future connectors. Typed links live in a `links` table (from_kind, from_id, to_kind, to_id, link_type) with a check on allowed `link_type` values defined in the data-model spec.

Triage state is stored in its own table so that connector re-syncs cannot overwrite local triage.

*Alternatives considered:* storing triage as columns on `threads` (rejected: triage must also apply to messages and people later, and re-sync would need to preserve columns carefully).

### D4. Policy gate as a pure function over a matrix
The matrix is a static table compiled into the daemon, keyed by `(principal, action)` where principal is either an agent id or a herdr machine id. Each entry yields `auto | pending | denied`. `note.append` carries an extra path predicate. The gate function is pure: `evaluate(principal, action, target) -> Decision`, and it is unit-tested exhaustively against the decided matrix. Decisions are recorded in the audit log whatever the outcome.

Matrix as shipped:

| Action | Default outcome |
|---|---|
| read | auto |
| draft | pending |
| archive | pending |
| snooze | pending |
| reminder.create | pending |
| note.append (allow-listed path) | auto |
| note.append (other path) | denied |
| send | not available to agents (no CLI verb, daemon denies agent tokens) |

*Alternatives considered:* a configuration file editable by the user at runtime (deferred: a static table is easier to test; configuration can be added later without changing the gate contract).

### D5. Note writes are append-only and daemon-performed
`note.append` never gives an agent a file handle. The agent sends `{path, text, idempotency_key}`; the daemon canonicalizes the path against the notes root, rejects any path whose canonical form leaves the allow-listed prefixes (`people/**`, `tasks/**`), rejects symlinks at any component (checked with `symlink_metadata` on every segment), opens the file with `O_APPEND|O_NOFOLLOW`, and writes an attribution comment line (`<!-- haste: appended by <agent_id> at <ts> -->`) followed by the text. There is no edit or delete action for agents at all.

*Alternatives considered:* letting the agent write the file directly and checking afterwards (rejected: race and no atomic guarantee).

### D6. Proposals as a state machine with human-only transitions
A proposal row holds `id` (ULID), `idempotency_key` (unique per principal), `principal`, `action`, `payload` (JSON), `source_thread_id` and `source_thread_version` (for stale detection), `state`, timestamps, and `result`. Creation returns immediately with `{proposal_id, state: pending}`. If the same `(principal, idempotency_key)` arrives again, the existing proposal is returned unchanged.

Transitions:
- `pending` → `approved` (human, TUI or `haste approve` run by the human shell; agents cannot call it)
- `approved` → `sent` (human, separate action; the daemon runs the connector `send`)
- `pending` → `rejected` (human)
- `pending` → `expired` (daemon timer, configurable TTL, default 7 days)
- `pending` or `approved` → `superseded` (a newer proposal with the same `principal`, `action`, and `source_thread_id` is created)

Stale-thread detection: before approval and before send the daemon compares the current thread version with `source_thread_version`. If it changed, the proposal is flagged `stale` (a flag, not a state) and the approval UI shows a warning; the human must acknowledge before approving.

Result delivery: after `sent` or `rejected` or `expired`, the daemon calls herdr `agent.prompt` on the originating agent's pane with a short result message, and `haste proposal status <id>` returns the same information on demand. Delivery failures are recorded and retried only by a bounded schedule (three attempts, backoff), never in a tight loop.

*Alternatives considered:* blocking CLI calls until a human decides (rejected by the brief: must be non-blocking).

### D7. Approve and send are separate actions
Approval records intent; send is a second, explicit action that performs the external side effect. This keeps the human's review step meaningful even when a connector is slow or fails after approval. Batch approval applies only to `approve`; batch send is not offered in this change.

### D8. Agent identity via daemon-issued tokens
On first registration an agent (or herdr machine) is issued a random 256-bit token by the daemon. The token is shown to the human once, stored in the daemon as a salted hash, and bound to a principal id and an allowed-actions subset. Clients present the token in every request. The herdr pane environment variable (for example `HASTE_AGENT`) is accepted only as a lookup hint that selects which token to present; the daemon never trusts it as identity. Tokens can be revoked by the human.

*Alternatives considered:* using the herdr pane ID as identity (rejected: pane IDs are reusable and forgeable by any local process that can set env).

### D9. Audit log is append-only JSONL
Every gate decision, proposal transition, note append, and connector call writes one JSON line to a file opened in append mode. Each record carries a monotonically increasing sequence number, the previous record's SHA-256 hash (hash chain, so truncation is detectable), timestamp, principal, action, outcome, and a reference to the object. The daemon is the only writer. The file is created with mode `0600`.

### D10. Connector trait with kill switch and circuit breaker
```rust
pub trait Connector: Send + Sync {
    fn source(&self) -> Source;
    fn capabilities(&self) -> Capabilities; // read, draft, send, archive flags
    async fn fetch(&self, cursor: Option<Cursor>) -> Result<Batch, ConnectorError>;
    async fn send(&self, draft: &Draft) -> Result<SendReceipt, ConnectorError>;
}
```
(Decided: boxed futures, object-safe, no extra dependency.) Capability flags gate which methods the daemon will call even if the trait is implemented. A per-connector kill switch is a persisted boolean checked before every call. The circuit breaker opens immediately on a `ConnectorError::Challenge` or `Checkpoint` variant, which means: no retries, kill switch set, human notified via the TUI. Transient errors use a fixed small attempt budget per cycle and then wait for the next scheduled cycle; there is no retry loop.

### D11. Crate layout
- `haste-proto`: request/response types, serde, framing.
- `haste-core`: data model, policy gate, proposals state machine, audit, connector trait.
- `haste-store`: rusqlite schema, migrations, repositories.
- `hasted`: daemon binary (socket server, scheduler, herdr delivery, launchd plist template).
- `haste`: CLI binary (clap).
- `haste-tui`: TUI binary (ratatui, crossterm), including the approvals view.

## Risks / Trade-offs

- **Single writer bottleneck** → Acceptable for a personal hub; the channel is bounded and reads use separate connections.
- **Token leakage via shell history or env** → Tokens are shown once and stored hashed; revocation is supported. The env var holds only the lookup hint, never the token itself, so leaking the env does not leak identity.
- **Hash-chained audit is tamper-evident, not tamper-proof** → A same-user process can rewrite the whole file. Acceptable threat model for a personal tool; documented as a limitation.
- **herdr `agent.prompt` delivery may fail if the pane is gone** → Delivery failure is recorded, `haste proposal status` remains the authoritative fallback.
- **Static matrix requires a rebuild to change** → Accepted for this change; config-driven policy is a later change.
- **Stale detection depends on connector thread versions** → Until connectors exist, versions are seeded by tests; the stale path is tested with synthetic version bumps.
- **Async trait ergonomics** → Minor; pick one approach in tasks and keep it consistent.

## Migration Plan

Greenfield: no existing data to migrate. Deployment steps:
1. Build `hasted`, `haste`, `haste-tui` in release mode.
2. Install the launchd plist template to `~/Library/LaunchAgents/`, pointing at the installed `hasted` binary.
3. `launchctl bootstrap` the agent; the daemon creates the `0700` socket directory, runs migrations, and creates the audit file.
4. Register agents with `haste agent register` (human-run) and distribute tokens.

Rollback: `launchctl bootout` the agent and remove the plist. Store and audit files are left in place; they are inert without the daemon.

## Open Questions

- Exact `link_type` vocabulary (for example `replies_to`, `about`, `references`) is deferred to the data-model spec; this design only requires that it is closed and validated.
- Whether the herdr machine principal should be able to hold more than the `read` and `draft` actions is deferred to the first herdr panel change.
- Whether batch approval should require a single confirmation keystroke or per-item review is a TUI UX decision for the approvals-tui spec.
