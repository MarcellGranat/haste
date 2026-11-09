## Why

> **PARKED.** Blocked until Messages is signed in (or Messages in iCloud is enabled) on the hub Mac: `~/Library/Messages/chat.db` is currently empty there (0 messages, 0 chats), so the schema, date unit and NULL-text share cannot be observed. Full Disk Access for the terminal already works. Do not start implementation before the human spike group (tasks section 1) is done.

Conversations the user has in iMessage sit outside the hub, so an agent working in a herdr workspace cannot see who the user is talking to. `chat.db` is local to macOS and reachable from the machine that runs the daemon. This change adds a read-only iMessage connector. Nothing is sent automatically; replies are stored draft proposals only.

This change builds on the shared `Connector`/`Event`-era model (connector framework, default-engaged kill switch descriptor, proposal and policy-gate structure) that is already present once `google-calendar` has been archived. It does not depend on `google-calendar` being unmerged and does not edit its artifacts.

## What Changes

- Add an iMessage connector that reads the local Messages database `~/Library/Messages/chat.db` read-only. Each chat becomes a `Thread` with `source = imessage`; each message becomes a `Message`. Sync is incremental by `message.ROWID`. Requires Full Disk Access for the process that runs the daemon.
- The iMessage connector declares `read = true` and `draft = true` only. A draft is a stored proposal text that a human copies or dismisses. No code path sends an iMessage, and `send` stays false.
- The connector registers with its kill switch engaged (using the default-engaged descriptor flag provided by the connector framework). The human releases it after granting Full Disk Access.
- Project tags for iMessage threads come from herdr workspace labels using the existing scope mechanism; the human sets them. Machine principals see only items tagged for their own machine.
- Per-cycle audit entries contain counts and the source name only: no message text, handles, or chat names.
- **Human spike group first**: Full Disk Access on the daemon binary, and the `chat.db` schema on this macOS version, are verified by the human before implementation begins (blocked, see above).

**BREAKING**: none. The new source usage is additive.

## Capabilities

### New Capabilities
- `connector-imessage`: read-only `chat.db` sync into threads and messages, Full Disk Access handling, draft-only proposals, kill switch on first install, per-cycle counts audit.

### Modified Capabilities
- `data-model`: iMessage chats and messages use the existing `Thread` and `Message` records with `source = imessage`.
- `proposals`: adds the iMessage `Draft` proposal that is never executed.
- `policy-gate`: `imessage.send` is not defined and resolves to `denied`.

## Impact

- Code: `crates/haste-connector-imessage` (new: chat.db reader, mapper), `crates/haste-core` (iMessage `Draft` payload; `Source::Imessage` round-trip), `crates/hasted` (scheduler, default-engaged registration), `crates/haste` (`connector status` line and release/engage verbs), `crates/haste-tui` (iMessage draft view), `crates/haste-store` (message cursor, scope tags for threads).
- Systems: macOS only (Full Disk Access on the daemon's binary or its launching terminal; the spike decides which).
- Dependencies: a SQLite driver already used by the store (read-only open).
- Tests: fakes only. A fake SQLite `chat.db` with the observed schema. No test reads a real Messages database.
- Docs: README section for the iMessage Full Disk Access step.
