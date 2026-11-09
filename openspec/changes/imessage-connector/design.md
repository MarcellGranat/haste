## Context

**PARKED:** blocked until Messages is signed in / Messages in iCloud is enabled on the hub Mac. `chat.db` there currently holds 0 messages and 0 chats, so the schema-dependent decisions below are provisional. Full Disk Access for the terminal already works.

The hub already has the pattern this change needs. Each connector implements the `Connector` trait, and the daemon calls it only through `Guarded` (kill switch, capability flags, circuit breaker, bounded transient retry, audit). `Source` already includes `Imessage`. This change relies on the shared connector framework (including the default-engaged kill switch descriptor flag) being present once `google-calendar` is archived.

Constraints: iMessage replies are proposals only and are never sent; kill switch engaged on first install; per-cycle audit with counts only; project tags from herdr workspace labels; scope filter for machine principals; fakes-only tests.

## Goals / Non-Goals

**Goals:**
- Read iMessage chats into the unified model, incrementally and read-only.
- Keep the connector safe on first install: kill switch engaged until the human grants Full Disk Access and releases it.

**Non-Goals:**
- Sending iMessages or any draft that leaves the hub automatically. Drafts are text in a proposal only.
- Reading iMessage attachments, or remote machines' Messages databases.

## Decisions

### D1. iMessage reads `chat.db` directly, read-only, from a copy-free handle
Open `~/Library/Messages/chat.db` with `SQLITE_OPEN_READ_ONLY` through the store's existing SQLite driver. Never write to it, never create `-wal`/`-shm` files beside it if avoidable (open with `immutable=0` and `query_only` pragma; the spike decides whether WAL sidecars are readable under read-only open). Sync reads `message` joined to `chat_message_join` and `chat` with `handle` for addresses, filtered by `message.ROWID > cursor`. The cursor is the max ROWID seen, stored per connector as `Cursor`.
- Alternative: copy `chat.db` to a scratch file each cycle. Rejected: copies duplicate message content on disk and the copy can be stale; read-only open is enough.
- Dates: `message.date` is nanoseconds since 2001-01-01 on modern macOS; the mapper converts to Unix time and the spike confirms the divisor on this macOS.
- Body text: `message.text` may be NULL on newer macOS with the body only in `attributedBody` (typedstream). v1 stores `text` when present; for NULL-text rows it stores the body as unavailable rather than parsing typedstream. Parsing typedstream is a follow-up unless the spike shows most rows are NULL-text.

### D2. iMessage is read plus draft proposals only
The connector declares `read = true`, `draft = true`, `send = false`, `archive = false`. A draft is a `Draft` proposal whose body is text the human can copy. It is never dispatched anywhere. There is no `send` method on the connector, so `Guarded` refuses any send call with a capability-missing error. The TUI shows drafts with the target thread and a dismiss action; approval only marks the draft "approved for manual copy" and does not transmit it.

### D3. Full Disk Access is a human precondition, detected not requested
Without Full Disk Access, opening `chat.db` fails with a permission error. The connector maps that to a new `PermissionNeeded` outcome (treated like a checkpoint for the human, but not a challenge): the kill switch stays engaged, the status view says "Full Disk Access needed for the daemon binary", and nothing retries in a loop. The human grants FDA to the daemon's binary and releases the connector. The daemon never requests FDA itself.

### D4. Kill switch engaged on first install
The connector sets `default_engaged = true` on its descriptor (the flag is provided by the connector framework). The human releases it with `haste connector release imessage`.

### D5. Scope tags from herdr workspace labels
Threads get project tags through the existing machine-scope mechanism: herdr workspace labels are candidate tags, and the human confirms them. Machine principals see only items whose tags exist on their own machine, enforced in the daemon read path. The exhaustive per-principal scope-filter test is extended to iMessage threads.

### D6. Audit is counts only
Each iMessage cycle writes one entry: source, counts of new threads and messages, any cursor reset, plus the error class. Audit records never include message text, handles, chat names, or attachment names.

### D7. Tests are fakes only
A fake SQLite file built by the test with the observed `message`, `chat`, `handle`, `chat_message_join` schema, including a NULL-text row and a row with the nanosecond date. No test reads a real `chat.db`. Real access is covered only by the manual smoke group.

## Risks / Trade-offs

- **[chat.db schema drift across macOS versions]** -> The spike records the schema on this macOS; the mapper reads only the columns it needs and fails closed with an `Other` error if a required column is missing.
- **[NULL `text` with body in `attributedBody`]** -> v1 marks those bodies unavailable; the spike measures how many rows are affected before deciding whether typedstream parsing is worth a follow-up.
- **[Full Disk Access granted to the wrong binary]** -> The status view names the exact binary path the daemon runs from; the spike verifies the grant on that binary.
- **[WAL sidecar under read-only open]** -> If read-only open cannot see recent rows without `-wal`/`-shm`, the spike chooses between opening read-only with WAL access and a documented latency trade-off; no write to the Messages directory is allowed in any option.
- **[Change is parked]** -> Provisional decisions may change once real data exists; the spike group updates this document before code starts.

## Migration Plan

1. Unblock: sign in to Messages / enable Messages in iCloud on the hub Mac and let `chat.db` populate.
2. Human spike group completes; findings recorded in `design.md` Open Questions and the specs.
3. Connector, store changes and the draft proposal kind land behind a default-engaged kill switch, so the daemon can ship before any authorization.
4. Rollback: engage the kill switch (`haste connector engage imessage`) or remove the connector registration. No Messages data is modified in any step.

## Open Questions

- `chat.db` on this macOS: the schema, the `message.date` divisor, the share of NULL-text rows, and whether WAL sidecars are needed for a current read. Unanswered: `chat.db` is empty on this Mac (Messages is not signed in or Messages in iCloud is off).
- Whether Full Disk Access applies to the built `hasted` binary or to the launching terminal in this setup; the answer sets the status text and the manual smoke steps. Known so far: FDA for the terminal works.

## Spike results (2026-11-09, real accounts)

- `chat.db` is empty on this Mac (0 messages, 0 chats): Messages is not signed in or Messages in iCloud is off. The iMessage schema, date unit and NULL-text share are still open.
- Full Disk Access for the terminal works.
