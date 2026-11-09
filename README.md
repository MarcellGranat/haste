# haste

A personal inbox hub (mail, chats, reminders, notes) driven from a vi-style TUI and
operated by agents through a CLI. **Agents can read, propose and append to a few notes.
They can never send.** Everything an agent asks for passes a policy gate in a local daemon.

This repository contains the core (`core-policy-gate`: daemon, store, policy gate, proposals,
approvals TUI, audit log, connector framework) and the first real connector (`gmail-connector`).
The other connectors are separate, later changes (see the roadmap below).

## Layout

| Crate | Role |
|---|---|
| `haste-core` | data model, policy gate, proposal state machine, audit log, tokens, connector trait |
| `haste-proto` | length-prefixed JSON wire protocol (4-byte big-endian length, UTF-8 JSON) |
| `haste-store` | SQLite (WAL), single writer thread, migrations |
| `hasted` | the daemon: Unix socket server, schedulers, result delivery |
| `haste` | agent CLI. Build with `--features human` for the human-shell verbs |
| `haste-tui` | ratatui interface: inbox, approvals, connectors and agents |
| `haste-herdr` | herdr link: local socket feed, the hub's `--machine` CLI feed, safe command builder, pinned protocol schema |
| `haste-connector-icloud` | iCloud Mail connector (read-only IMAP), authorization helper `haste-icloud-auth`, Keychain secret store |
| `haste-connector-reminders` | Apple Reminders connector: typed helper protocol, runner, and the Swift EventKit helper (`helper/`) |
| `haste-connector-gmail` | Gmail connector (read + plain-text drafts), OAuth helper `haste-gmail-auth`, Keychain token store |

```
 haste CLI (agents) ─┐                     ┌─ SQLite store (single writer)
 haste-tui (human)  ─┼─ unix socket 0700 ─▶ hasted ── policy gate ─┼─ audit.jsonl (hash chain)
                     │   token per caller  └─ connectors (trait) ──┴─ notes root (append only)
```

## Install and run (macOS)

```sh
cargo build --release -p hasted -p haste-tui
cargo build --release -p haste                        # agent CLI (no human verbs)
cargo build --release -p haste --features human \
    --target-dir target/human                         # human shell CLI

sed -e "s|__HASTED_BINARY__|$PWD/target/release/hasted|" -e "s|__HOME__|$HOME|g" \
    deploy/com.haste.hasted.plist > ~/Library/LaunchAgents/com.haste.hasted.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.haste.hasted.plist
```

Stop and roll back:

```sh
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.haste.hasted.plist
rm ~/Library/LaunchAgents/com.haste.hasted.plist
```

Data lives in `~/Library/Application Support/haste/` (override with `HASTE_HOME`; notes root
with `HASTE_NOTES_ROOT`). Bootstrap and rollback leave the store and audit files in place.

### Token issuance flow

1. First start creates the `human` principal and writes its token to `human.token` (mode `0600`).
2. The human registers an agent, either printing the token once or saving it to a file:
   `haste agent register researcher --allow read --allow draft --allow note.append --write-token`
   (human build). The daemon keeps only a salted SHA-256 hash.
3. The agent runs with `HASTE_AGENT=researcher` (or `HASTE_TOKEN_FILE=...`). That variable only
   chooses which token file the CLI reads. **The daemon identifies callers by token alone.**
4. `haste token revoke researcher` kills the token immediately and permanently.

## Gmail

The connector can **read** mail and, after you approve a draft proposal, **create a plain-text
Gmail draft**. It cannot send, archive, label or delete, and it only asks Google for the
`gmail.readonly` and `gmail.compose` scopes. You send drafts yourself from Gmail.

### One-time Google Cloud setup

A project in "Testing" status makes refresh tokens expire after 7 days, so use a personal
project set to production:

1. In the [Google Cloud console](https://console.cloud.google.com/) create a project and enable the **Gmail API**.
2. **OAuth consent screen**: user type External, add the scopes `gmail.readonly` and `gmail.compose`, and add yourself as a user.
3. **Publishing status**: click *Publish app* so it reads **In production**. Leave it unverified: for one user (the cap is 100) no verification is needed.
4. **Credentials**: create an OAuth client ID of type *Desktop app* and note the client id and secret.
5. Build the daemon with Gmail, then authorize once:

```sh
cargo build --release -p hasted --features gmail -p haste-connector-gmail
cargo build --release -p haste --features human --target-dir target/human
export HASTE_GMAIL_CLIENT_ID=...  HASTE_GMAIL_CLIENT_SECRET=...
target/human/release/haste connector gmail auth     # opens the browser
launchctl kickstart -k gui/$(id -u)/com.haste.hasted
```

Google shows "hasn't verified this app" once: choose *Advanced*, then continue. The refresh
token goes to the macOS Keychain (service `com.haste.gmail`); `gmail.json` in the data directory
holds only the client id, secret and your address (mode `0600`). If the token is ever revoked
(`invalid_grant`), the connector engages its kill switch and notifies you: run the auth command
again, then release the connector.

### Connector administration (human only)

```sh
haste connector status            # every connector and whether its kill switch is engaged
haste connector release gmail     # resume; sync continues from the saved cursor
haste connector engage gmail      # stop talking to Gmail at once (cursor is kept)
```

The same actions live in the TUI's Connectors tab (`R` release, asking for `y`; `E` engage).
Agents cannot use any of this. The circuit breaker engages a switch by itself on a Google
restriction or challenge, with no retry, and never releases it.

### Manual smoke check (live account)

1. `haste connector gmail auth` finishes and prints your address; `haste connector status` shows `gmail` running.
2. After five minutes (or a daemon restart) `haste thread list --source gmail` shows recent threads.
3. As an agent: `haste draft --thread <id> --body "Thanks, noon works."`
4. In `haste-tui` approve the proposal: a plain-text draft appears in that Gmail thread, and the proposal's result reads `draft:<id>`.
5. `haste connector engage gmail`, wait one cycle: no new Gmail requests; `release` resumes it.

On first use macOS may ask whether `hasted` may read the Keychain item created by the auth
helper: choose *Always Allow*.

## iCloud Mail

Read-only: the connector reads iCloud Mail over IMAP (`imap.mail.me.com:993`, implicit TLS,
certificate verification on) and never writes to the mailbox. It sends only LOGIN, EXAMINE,
UID SEARCH, UID FETCH (always `BODY.PEEK`, so mail is never marked read) and LOGOUT. Attachment
bodies are never downloaded; attachments appear as a metadata line (name, type, size). One text
part per message is fetched: `text/plain` if there is one, otherwise `text/html`, which is
reduced to text on your machine (tags, `<style>`, `<script>` and comments removed). HTML is never
rendered and nothing it links to (images, tracking pixels) is requested.

### Setup

1. At [appleid.apple.com](https://appleid.apple.com) open *Sign-In and Security*, then *App-Specific Passwords*, and create one for haste. (Your main Apple ID password is refused by iCloud.)
2. Build and authorize:

```sh
cargo build --release -p hasted --features icloud -p haste-connector-icloud
cargo build --release -p haste --features human --target-dir target/human
target/human/release/haste connector icloud auth \
    --address you@icloud.com --folder INBOX --folder "Sent Messages" --backfill 500
launchctl kickstart -k gui/$(id -u)/com.haste.hasted
```

The helper asks for the app-specific password without echo, checks it with a real login and
`EXAMINE INBOX`, and only then stores it in the macOS Keychain (service `com.haste.icloud`).
Nothing is stored if the login fails.

### Your settings

`icloud.json` in the data directory (mode `0600`, never contains the password) holds what you
chose; edit it any time and restart `hasted`:

```json
{
  "address": "you@icloud.com",
  "folders": [
    { "name": "INBOX" },
    { "name": "Sent Messages", "backfill": 1000 }
  ],
  "backfill_count": 200
}
```

* `folders`: which folders to sync (INBOX only if empty). A folder that does not exist is skipped and noted in the audit log; the others still sync.
* `backfill_count`: how many of the newest messages the first sync fetches per folder (default 200). A folder's own `backfill` overrides it. The same size is used if a folder's `UIDVALIDITY` changes on the server.

### If the password stops working

A rejected login is terminal: the connector engages its kill switch, notifies you, and does
**not** retry. Create a new app-specific password, run `haste connector icloud auth` again, then
`haste connector release icloud`. Network trouble is different: it is retried a few times within
one cycle, then waits for the next one, and never trips the breaker.

### Manual smoke check (live account)

1. `haste connector icloud auth ...` prints `Authorized you@icloud.com`; `haste connector status` lists `icloud` running.
2. After a restart and up to five minutes, `haste thread list --source icloud` shows recent threads, and they are still unread in the Mail app.
3. Send yourself a mail: it appears after the next cycle and only that one is fetched (the audit log line shows `INBOX:new=1`).
4. `haste connector engage icloud`, wait a cycle: no IMAP traffic; `release` resumes.

## Herdr: agents, projects and machines

Haste watches [herdr](https://herdr.dev) so you can see what your agents are doing and so a
proposal knows which project it belongs to. It works from your **hub** computer, the one whose
herdr has the saved SSH machines:

* the **local** machine over herdr's unix socket (live status events);
* each **enabled saved machine** (`herdr machine list`) through the hub's own
  `herdr --machine <label> api snapshot`, polled every 15 seconds. Haste never runs `ssh`, never
  needs forwarded sockets, and never changes your saved machines. Every herdr call it makes has
  no stdin and a 20 second kill timeout, so an SSH prompt can never hang the daemon.

Turn it off with `HASTE_HERDR=0`. `HASTE_HERDR_SOCKET` and `HASTE_HERDR_BIN` override the socket
path and the `herdr` executable.

### The Agents tab and blocked agents

`haste-tui` has an **Agents** tab: every machine with its state (connected, unreachable, or
protocol mismatch) and, while it is connected, its workspaces and agents with their status and how
long they have had it. An unreachable machine shows no rows rather than stale ones. Agents that
herdr reports as `blocked` (waiting on an approval or question) also appear in the **Approvals**
tab, in a read-only *Blocked agents* group, oldest first. Haste never sends keystrokes or reads
pane output; the tab only shows who and where.

You get a notice when an agent becomes blocked or done (rate limited: a burst of 3, then one per
30 seconds per agent, blocked notices at most one per 10 seconds per machine). Starting haste
announces nothing for agents that were already blocked.

### Projects: workspace names are tags

A herdr **workspace name** is a project tag. Herdr shows labels like `[4] haste`; the number is only the workspace's position on that machine, so the tag is `haste`. The same name on two machines is one project, and reordering workspaces changes no tag.

```sh
haste tag list                       # the known projects (labels from all machines)
haste tag add <thread-id> haste      # tag a thread; only a known label is accepted
haste tag remove <thread-id> haste
haste machine status                 # machines, their state and their agents
```

(Human build only.) Tags are set by you, never by agents. A thread with no tag is visible only to
you and to agents, as before; a **machine** principal (an id equal to a herdr machine label) sees
only threads tagged with a project that has a workspace on its own machine.

### Telling haste where an agent runs

When an agent proposes something from a herdr pane, the `haste` CLI sends two hints:
`HERDR_PANE_ID` (herdr sets this) and `HASTE_MACHINE` (the herdr machine label; `local` if unset).
The daemon looks the pane up and stores the pane's **workspace label as the proposal's project**,
and the machine as its origin. In Approvals, press `P` to group by project instead of by agent;
`/` also filters by project. Set `HASTE_MACHINE=m1` in the shell profile on machine `m1`.

Hints are routing only. Who the caller is still comes from the token alone, and a hint can never
grant anything. How the CLI on a *remote* machine reaches the hub's daemon is out of scope here.

### Results go back to the agent

When a proposal is sent, rejected or expired, the agent is prompted with `agent.prompt`: over the
socket for a local agent, through `herdr --machine <label> agent prompt` for an agent on a remote
machine. If the local socket is down the plain CLI is used instead. Three attempts, then it stops
and `haste proposal status <id>` still has the result. An agent whose id is not a valid herdr
agent name (`[a-z][a-z0-9_-]{0,31}`) is never prompted.

### Protocol pinning

Haste speaks herdr **protocol 22** and checks it on the socket and in every snapshot. If herdr
reports another number, that machine is not observed and you get one notice naming both numbers.
The committed copy of herdr's schema (`crates/haste-herdr/schema/herdr-api-22.json`) is checked by
a test. After upgrading herdr: `herdr api schema --json > crates/haste-herdr/schema/herdr-api-NN.json`,
review the diff, bump `HERDR_PROTOCOL`.

### Security boundary

Haste never reads pane output and never sends keys. The only write to a pane is the result message
above. Titles, working directories and workspace contents are dropped on arrival and never stored,
audited or announced; the audit log records machine labels, ids and counts only.

### Manual smoke check (real hub)

1. Start `hasted`; `haste machine status` lists `local` and each enabled saved machine as connected.
2. Make an agent wait on an approval in a herdr pane: it appears under *Blocked agents* in Approvals.
3. From an agent in workspace `haste`, run `haste draft --thread <id> --body hi`: the proposal row shows `[haste]`.
4. `haste tag add <thread-id> haste` works; `haste tag add <thread-id> nonsense` is refused.

## Apple Reminders

Reminders are mirrored read-only into haste, and an agent can *propose* creating a reminder
(for example from a mail thread) or marking one done. Nothing is written until you approve the
proposal **and** run it. haste never deletes or edits a reminder.

```
 agent ── propose ─▶ pending ─ approve ─▶ approved ─ run (s) ─▶ executing ─▶ sent
                                                        └─ failed before any effect ─▶ approved
```

### Setup (macOS only)

1. Build the helper (a small EventKit program, ad-hoc signed, with a usage description):
   `crates/haste-connector-reminders/helper/build.sh` (writes `target/helper/haste-reminders-helper`).
   Put it next to `hasted` or point `HASTE_REMINDERS_HELPER` at it.
2. From a **normal Terminal window** (not an agent shell) run `haste connector reminders auth`
   and allow access. macOS attributes the grant to the process that asks.
3. Start `hasted`. The Reminders connector starts **engaged** (nothing runs) the first time.
   Release it with `haste connector release reminders`; `haste connector reminders status` then
   shows permission, kill switch and counts (never titles).

### Using it

- Email to reminder: `haste email to-reminder <thread-id> [--title ..] [--project ..] [--due ..]`
  (or `T` on a thread in the TUI). It creates a proposal; approve it (`a`), then run it (`s`, or
  `haste proposal execute <id>`).
- Agents: `haste reminder create --title ".." --from-thread <id>`, `haste reminder complete <id>`,
  `haste reminder list`. Their `project` defaults to the workspace of their herdr pane and must be
  a known workspace label.
- Tags: `haste tag add-reminder <reminder-id> <project>` (human only). A machine sees a reminder
  only when it is tagged with a workspace that exists on that machine.
- The created reminder's notes contain only `haste:thread/<id>`, never the mail subject, sender or body.

### Known limits and the crash rule

- macOS only. No delete, no edit. Notes are mirrored truncated.
- If the helper times out or the daemon dies while a write is in flight, the proposal stays
  `executing` (shown as NEEDS CHECK) and is **never re-run automatically**. Look in Reminders, then
  `haste proposal resolve <id> --done` (it is there) or `haste proposal resolve <id>` (it is not).
- Missing permission is reported as `permission needed`; it never trips the circuit breaker.
- TUI reminder tagging and linked reminders on a thread view are not built yet.

### Manual smoke check (real Reminders)

1. `haste connector reminders auth` from Terminal prints `granted`.
2. Release the kill switch; `haste connector reminders status` shows counts only.
3. `haste tag add-reminder <id> <project>` on one reminder; a machine principal sees it only on that machine.
4. `haste email to-reminder <thread>`, approve, run: the reminder appears with only the identifier in its notes.
5. Propose and run a completion for it, then delete it by hand in Reminders.

## Policy

| Action | Agent outcome |
|---|---|
| read | auto |
| draft, archive, snooze, reminder.create | pending: a proposal is created, nothing happens until a human approves |
| note.append | auto inside `people/**` and `tasks/**` only; daemon-performed, append-only, attributed |
| send, approve, reject, manage | denied: human only; `send` has no CLI verb at all |

Proposals return a pending id immediately. They expire after **7 days**, are superseded by a
newer proposal for the same thread and action, and carry a stale flag when the thread changed.
Approve and send are separate human actions. The agent hears the outcome through
`herdr agent prompt` (three attempts, backoff) and can always run `haste proposal status <id>`.

## Threat model

Protects against: a misbehaving or prompt-injected agent sending messages, editing or deleting
notes, writing outside the allow-list (including `..` and symlink tricks), approving its own
proposals, or reading other agents' proposals.

Does **not** protect against: another process running as the same macOS user. It can read
`human.token`, tamper with the files and rewrite the whole audit chain (the hash chain is
tamper-evident, not tamper-proof). The herdr environment variable is a hint, never identity.
There is no network listener and no encryption at rest beyond file permissions.

Connectors for risky services (WhatsApp, Messenger) will sit behind a per-connector kill
switch and circuit breaker: a challenge or checkpoint error stops the connector at once, with
no retry, and notifies the human.

## Spec coverage notes

Every spec scenario is covered by a unit or end-to-end test except these, which hold by
construction and have a manual check:

| Scenario | Why it holds | Manual check |
|---|---|---|
| No network listener | `hasted` only creates a `UnixListener` | `lsof -a -p $(pgrep hasted) -i` prints nothing |
| Client cannot bypass daemon | `haste` and `haste-tui` do not depend on `rusqlite` or `haste-store` | `cargo tree -p haste -p haste-tui \| grep -E 'rusqlite\|haste-store'` prints nothing |
| Agent requests note edit or delete | no such action exists in the protocol or CLI | `haste note --help` lists only `append` |
| Token not retrievable | only a salted hash is stored | `sqlite3 haste.db 'select * from tokens'` shows hashes only |
| iCloud never writes to the mailbox | the session wrapper has no method for STORE/APPEND/COPY/MOVE/EXPUNGE | the integration test records every command the fake server receives |
| Clients link no HTTP or Keychain code | the OAuth flow is a separate helper binary | `cargo tree -p haste -p haste-tui \| grep -E 'reqwest\|security-framework'` prints nothing |
| Gmail cannot send or change labels | no such client method or scope exists | the consent URL lists only `gmail.readonly` and `gmail.compose` |

## Develop

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p haste --features human
```

## Roadmap (separate OpenSpec changes)

1. ~~`gmail-connector` (OAuth, sync, drafts)~~ done
2. ~~`icloud-imap-connector` (IMAP with an app-specific password in the keychain)~~ done
3. ~~`herdr-agents-panel` (status feeds, blocked agents, workspace tags)~~ done
4. ~~`reminders-and-links` (EventKit helper, email to reminder)~~ implemented, awaiting live smoke check
5. `imessage-calendar`
6. `obsidian-notes` (Person notes, allow-listed append)
7. `whatsapp-messenger-spike` (go/no-go, flagged connectors)
