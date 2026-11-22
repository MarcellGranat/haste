## Context

- `apfel` (v1.17.1, `/opt/homebrew/bin/apfel`) wraps Apple's on-device model. Relevant flags: `--system-file`, `--messages <path|->` (OpenAI-style message list, one-shot), `--schema <path>` (guaranteed-valid JSON output), `--max-tokens`, `--count-tokens`, `--model-info`, `--retry`, `-o json`. It also has `--serve`, an OpenAI-compatible HTTP server.
- haste's rules that constrain the design: the daemon owns all data and the only socket (no TCP listener); the TUI and CLI are thin clients; agents only propose, humans send; operational logs never carry message text, chat ids, addresses or tokens (`haste-core/src/log.rs`, README "Logging"); forward-only SQLite migrations; one writer thread.
- Existing building blocks to reuse: the `allowed_senders` file and the regex line format of `haste-core/src/ignore.rs` (`IgnoreList`); the scheduler tick pattern in `hasted/src/scheduler.rs`; the proposal-free "human saves" path used by `SaveTodo` in `haste-tui/src/app.rs`; the reply form and TODO popup in `app.rs`/`ui.rs`.
- The machine is a single Mac with load average around 5 at the time of writing (core count to be measured, see Open Questions).

## Goals / Non-Goals

**Goals**
- Ghost-text reply drafts and TODO suggestions for approved senders, produced in the background without slowing the machine.
- A user-triggered rewrite popup that always works when asked.
- Memories as plain local files that steer the model per channel or per sender.
- Every model call is observable (duration, outcome) without leaking content.
- Works when the TUI runs on another machine over the ssh tunnel.

**Non-Goals**
- No automatic send, archive, or task creation. The human saves every TODO and sends every reply.
- No agent or machine access to these results (they are human-only requests).
- No `apfel --serve` and no new listening socket or port.
- No cloud model, no network call from the AI path.
- No training, fine-tuning or embedding index; memory selection is by rule, not by similarity.

## Decisions

### D1. The daemon runs apfel, one process per call
`hasted` spawns `apfel` as a child process per request, with a bounded queue (one call at a time, queue depth 8) and a per-call timeout (default 60 s, child killed on expiry). The TUI never spawns it.
- Why: one place for the resource gate, the cache, the logs and the audit; a TUI on another machine gets results through the daemon socket with no extra path.
- Alternative rejected: `apfel --serve` keeps the model warm but opens a local HTTP listener, breaking the "no network listener" rule and needing a token. Revisit only if per-call start-up is too slow (measure first).

### D2. Content goes through stdin or files, never argv
Message and memory text is written to stdin (`--messages -`) or to a temporary file in the daemon's 0700 run directory with mode 0600, deleted after the call. The system prompt is a file (`--system-file`). The TODO JSON Schema is a constant file written at daemon start.
- Why: argv is visible to other users' `ps`; this keeps content off the command line and out of the log.

### D3. Structured TODO output via `--schema`
The schema is fixed: `{ has_task: bool, title: string (max 120), notes: string (max 500), due: "YYYY-MM-DD" | null, priority: "none" | "low" | "medium" | "high" }`. The daemon parses it with serde and validates lengths and the date; a failing answer is treated as "no suggestion", logged as `invalid_output`. The TUI never builds a TODO from raw model text.

### D4. Reply draft suggestions are cached, not stored as drafts
The result of tools 1 and 3 is stored in a new table `ai_results` keyed by `(kind, thread_id, thread_version, memory_fingerprint)`, with `payload` (JSON text), `created_at`, and `outcome`. A reopened thread with an unchanged version and memory set returns the cached result without a model call. Rows older than 14 days are deleted by the daemon's expiry tick. Suggestions are never written into the provider draft or the local draft table.

### D5. Background jobs: queue, gate, retry
Opening a thread from an approved sender enqueues `draft` and `todo` jobs (deduplicated per key). A scheduler tick (30 s) takes jobs whose resource gate passes. Gate (pure function in `haste-core`, values from constants):
- `load1 / ncpu < BACKGROUND_MAX_LOAD_PER_CORE` (initial 0.5), read from `sysctl -n vm.loadavg` and `sysctl -n hw.ncpu`;
- free memory at least `BACKGROUND_MIN_FREE_MB` (initial 4096), from `memory_pressure -Q` (free percentage) or `vm_stat`;
- memory pressure not `critical`;
- no interactive request waiting in the apfel queue.
Jobs live in memory only (not persisted): after a restart they are re-enqueued the next time the thread is opened. A skipped job stays queued with `next_attempt_at = now + 5 min` and is dropped after 24 h. Constants live together in `hasted/src/apfel.rs` (one `CONSTANTS` block) so they are easy to tune.

### D6. Tool 2 bypasses the gate and the queue priority
The rewrite request is synchronous: the TUI sends it, shows a spinner, and the daemon runs it at the head of the apfel queue. It runs whatever the load. Its result is returned, never cached, and the TUI keeps the previous draft for undo.

### D7. Memories: YAML files
Location `<data-dir>/memories/*.yml`, at most 50 files, each at most 4 KB, read at daemon start and on each job (cheap; mtime-checked). Parsed with `serde_yaml_ng` into a struct with `deny_unknown_fields` off (unknown keys are ignored with a warning naming the file).

```yaml
id: kata-biro-tone
title: Kata Biro is a client, keep it formal
enabled: true
channels: [gmail]            # optional, any of the channel names
senders: ['kata\s+biro']     # optional, regex lines as in allowed_senders
text: |
  Always address her as "Kata". Mention the invoice number if one appears.
```

- A memory matches a thread when its `channels` contains the thread's channel, or any `senders` pattern matches a sender of the thread (case-insensitive, same engine as `IgnoreList`). A memory with neither field never matches.
- Matching memories are ordered by id, capped at 3 per call, and their `text` capped at 1 KB total.
- The memory fingerprint (SHA-256 of the matched ids and texts) is part of the cache key, so editing a memory invalidates cached suggestions.
- A file that is not valid YAML, lacks `id` or `text`, or exceeds the size limit is skipped with a logged warning naming the file and the reason, never the content.

### D8. Which messages go to the model
- Auto draft: the latest inbound message plus at most 4 earlier loaded messages of the thread, oldest first.
- Rewrite: at most 2 earlier messages plus the current draft text.
- Auto TODO: the latest inbound message plus at most 4 earlier messages.
- Token budget: before each call the daemon runs `apfel --count-tokens` on the assembled prompt and drops the oldest earlier messages (then memory bodies) until it fits a constant `PROMPT_TOKEN_BUDGET` (initial 2500, leaving room for the answer on the on-device model's small window). A prompt that still does not fit is skipped with `too_long`.

### D9. Approved senders reuse `allowed_senders`
A thread is "from an approved sender" when a sender matches a pattern in `<data-dir>/allowed_senders` (existing `ALLOWED_FILE`). Auto jobs are enqueued only for such threads. Rewrite works for any thread the user is replying to.

### D10. Human-only protocol
New request kinds, all denied to agent and machine principals in the policy gate and in `dispatch.rs`:
- `AiSuggestionGet { thread_id }` returns the cached draft and TODO suggestion (or `pending` / `none`) and enqueues jobs if missing;
- `AiRewrite { thread_id, draft, mode, instruction }` (synchronous, returns text);
- `AiMemoriesList` for the TUI to show which memories apply to a thread (read-only, no content beyond titles).
`PROTOCOL_VERSION` moves from 11 to 12. The TUI and daemon ship together; an old TUI gets the existing version-mismatch notice.

### D11. TUI behaviour
- Reply form: a `suggestion: Option<String>` field on the compose state, rendered dim after the cursor. `Right` at the end of the body, or `Tab` at any cursor position, inserts it (appends a newline first if the body is non-empty). While a suggestion is shown, `Tab` is taken by this and does not move focus in `haste-tui/src/compose.rs` (`Key::Tab`, line ~218); with no suggestion shown it keeps moving focus. Any typed character dismisses it. Opening the form polls `AiSuggestionGet` once; the suggestion appears when ready.
- Rewrite: `Ctrl-r` in the reply form (`Ctrl-g` is the external editor; `Ctrl-h/j/s/l/f/x/a/e` are taken; `Ctrl-r` is only bound in the list view, where it refreshes) opens a popup with the five modes; a custom mode takes a one-line instruction. The result is previewed and `Enter` replaces the body; `Ctrl-z` in the reply restores the previous body (one level of undo; `u` would type a letter into the text, so it cannot be the key).
- TODO: `t` on a thread calls `AiSuggestionGet`; if `has_task` is true the existing TODO popup opens prefilled; if false it opens empty with a dim note "no task suggested". Saving uses the existing `SaveTodo` path (human action, no proposal).

### D12. Logging and audit
- Operational log, component `apfel`: `tool` (draft, rewrite, todo), `thread` (short hash, see Open Questions), `duration_ms`, `outcome` (ok, timeout, refused, invalid_output, too_long, gated, unavailable), `queue_depth`, `load1_x100`, `free_mb`. Never prompt, output, memory text or the instruction.
- Audit (hash chain): one record per call, action `ai.call`, principal `human`, target the short thread hash, outcome as above. No content.
- Both follow the existing field rules, so free text cannot reach them by construction.

### D13. Failure handling
- `apfel` missing or `--model-info` failing: `unavailable`; the feature shows nothing, no error popup for background jobs; rewrite shows a one-line message.
- Guardrail refusal (apfel exit code for refusal): `refused`; no retry; the suggestion is not shown.
- `--retry` is not used: a single attempt per job; background jobs retry through the queue instead, and rewrite is never retried silently (the user can press the key again).

## Risks / Trade-offs

- [Model output is poor or unsafe to paste] → ghost text is never auto-inserted; the user must press Right/Tab; rewrite previews and undo; send keeps its existing confirmation countdown.
- [Prompt injection through a message or a memory file] → the model output is only ever text or the fixed schema; the daemon never executes it; TODO fields are length-limited and typed; memories are local files the user writes.
- [Background load slows the Mac] → gate on load per core and free memory, one call at a time, 60 s timeout, skip instead of waiting.
- [Small context window truncates important history] → token counting before each call, oldest messages dropped first, and a `too_long` outcome rather than silent truncation.
- [Stale suggestion after the thread changes] → cache key includes the thread version and memory fingerprint; the reply form shows nothing stale.
- [Logs leak content] → fixed labels and hashed thread ids only; the field-type rule in `log.rs` enforces it by construction.
- [Gate constants are wrong for this machine] → constants in one block, measured values recorded in the README before release.

## Migration Plan

1. Land the store migration (table `ai_results`, forward-only) and the pure functions (memory parsing and matching, gate, token-budget trimming) with unit tests.
2. Land the daemon `apfel` module behind a settings file `<data-dir>/apfel.json` with `"enabled": false` by default.
3. Land the protocol v12 requests and dispatch arms, denied for non-human principals, with tests of the gate outcome.
4. Land the TUI pieces (ghost text, rewrite popup, `t` prefill), then enable on the owner's machine by setting `"enabled": true`.
5. Rollback: set `"enabled": false` (no restart of the TUI needed; the daemon needs a restart since it reads the file at start). The table can stay; nothing reads it when disabled. Reinstalling binaries requires restarting `hasted` (per the user's reinstall note).

## Decisions from review
- **Accept key:** `Tab` and `Right` both accept the suggestion; `Tab` does not move focus while one is shown.
- **Rewrite key:** `Ctrl-r` in the reply form.
- **Thread id in logs:** a short SHA-256 prefix, never the raw id.
- **Approved senders:** reuse `allowed_senders`; no separate file.
- **Memory format:** YAML files (`memories/*.yml`) parsed with `serde_yaml_ng`; no YAML crate exists in `vendor/` or `Cargo.lock` yet, so this adds one dependency.

- **Auto TODO reruns only on new mail:** the job is keyed by the thread version; a reopen with no new inbound message uses the cached result and never calls apfel again. Auto draft follows the same rule.
- **Refusals:** apfel exit codes are 3 guardrail blocked (outcome `refused`), 4 context overflow (`too_long`), 5 model unavailable (`unavailable`), 6 rate limited or busy (`busy`, retried later), 1 runtime error (`error`). A refusal is cached for that thread version so it is not retried.
- **Settings:** `apfel.json` in the data directory (same style as `gmail_tabs.json`).

## Measured (2026-11-22)
- This Mac: 12 cores, 32 GB. Load average about 5 to 6 while working (0.43 to 0.51 per core), 67% memory free. The initial `APFEL_MAX_LOAD_PER_CORE` of 0.5 is kept; it will hold jobs back about half the time, and `apfel.json` can raise it.
- apfel's window is 4096 tokens (`--count-tokens` reports a limit of 3584 for the prompt). Latest plus 4 earlier messages of 25 real mail threads: 134 to 3557 tokens, median 1402, p90 2104. `APFEL_PROMPT_TOKEN_BUDGET` stays 2500.
- `apfel --count-tokens` takes the text on stdin only (not `--messages`) and `-o json` with it hangs, so the daemon counts the system and user text joined, with a 15 s kill.
- `t` is bound on the lists only; the reader deliberately ignores it (a test says so), so it stays that way.
- A draft is not made when the human already wrote after the latest message from the other side (nothing to answer).

## Open Questions

1. **Context size.** Measure `--count-tokens` on real threads to set `PROMPT_TOKEN_BUDGET` (4 and 2 messages are the requested caps).
2. **Calibration.** Core count and typical load on this Mac set `BACKGROUND_MAX_LOAD_PER_CORE` and `BACKGROUND_MIN_FREE_MB`.
