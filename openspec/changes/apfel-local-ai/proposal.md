## Why

Triage and replying are the slowest parts of haste's inbox for the human. Most mail from people the
user has approved needs a short, predictable answer, and some of it hides a task. The Apple on-device
model behind `apfel` is free, local and already installed, so it can draft replies, polish a draft
and pull out TODOs without any data leaving the Mac. Running it inside `hasted` keeps the
"human sends, daemon owns everything" rule: the model only ever proposes text, and the TUI decides.

## What Changes

- **Auto draft (background).** When the user opens a thread from an approved sender, the daemon
  asks `apfel` for a suggested reply and caches it. The reply form shows it as dimmed ghost text;
  `Right` or `Tab` inserts it into the body. It is never sent automatically.
- **Rewrite my reply (user-triggered).** A key in the reply form opens a popup: custom instruction,
  check grammar, more formal, more fluent, more friendly. The result previews in the draft and is
  undoable. Always runs when asked.
- **Auto TODO (background).** For approved-sender threads the daemon asks `apfel` for a structured
  answer (`--schema`): whether there is a task, title, notes, due date and priority. Pressing `t` on
  a thread opens the existing TODO popup prefilled with it, fully editable. Nothing is created
  without the user saving the popup.
- **Memories.** Local markdown files with front matter under the haste data folder
  (`memories/*.md`). Each memory has trigger rules (channel or sender pattern). Only memories whose
  rules match a thread are passed to the tools.
- **Resource gate for background work.** Tools 1 and 3 run only when the machine has enough free
  capacity (load average and free memory against configurable constants); otherwise the job is
  skipped and retried later. Tool 2 always runs when the user asks.
- **Logging.** Every `apfel` call is recorded in the operational log (tool, thread id, duration,
  outcome, never message or memory text) and in the audit trail as a non-secret record.
- **Local cache.** Results are stored in the SQLite store keyed by thread and thread version, so a
  reopened thread does not call the model again while nothing changed.
- **BREAKING** none for existing clients; the protocol gains new request kinds, so the protocol
  version is bumped and the TUI and daemon ship together.

## Capabilities

### New Capabilities
- `apfel-runner`: daemon-side wrapper around the `apfel` CLI: invocation, timeouts, resource gate for background jobs, result cache, and logging.
- `ai-memories`: the local memory file format, trigger rules (channel and sender), and how matching memories are selected for a thread.
- `ai-reply-assist`: auto draft ghost text and the rewrite popup in the reply form, including preview and undo.
- `ai-auto-todo`: structured TODO extraction for approved senders and prefilling the existing TODO popup with it.

### Modified Capabilities
- `daemon-ipc`: new human-only request kinds for suggestion lookup, rewrite and TODO suggestion; agent and machine principals are denied.

## Impact

- **Code**: `haste-proto` (new requests, protocol version bump), `haste-store` (cache table and a forward-only migration), `hasted` (new `apfel` module, background job in `scheduler.rs`, dispatch arms in `dispatch.rs`), `haste-core` (memory rule matching, resource-gate policy as pure functions), `haste-tui` (`app.rs`, `ui.rs`: ghost text, rewrite popup, `t` key).
- **Data**: a new `memories/` folder under the data directory; no change to existing tables beyond the cache.
- **Dependencies**: runtime dependency on `/opt/homebrew/bin/apfel` (path configurable). No new crates expected beyond what the workspace has.
- **Privacy**: all model calls are local; prompts contain message text and memory text, so the operational log and audit trail must never carry them (existing log rules apply).
- **Operations**: `apfel` runs on the hub only; a TUI on another machine over the ssh tunnel gets results from the daemon.
