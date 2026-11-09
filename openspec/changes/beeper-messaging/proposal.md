## Why

WhatsApp, Facebook Messenger and iMessage conversations sit outside haste, so an agent working in a herdr workspace cannot see who the user is talking to, and the user has to leave haste to answer. Beeper Desktop already connects these networks on the hub Mac and exposes them through a local HTTP API. One connector over that API gives haste read access to the chosen chats and a human-only send path, without adding three separate network integrations. The same API also unparks iMessage, whose local `chat.db` is empty on this Mac.

This is a personal-use integration with a real account-suspension risk. Beeper's own docs warn that sending too many messages may get accounts suspended, and unofficial network integrations can be blocked. The change therefore reads only chosen chats, keeps every token and secret in the macOS Keychain, refuses cloud-bridged accounts, and starts with the kill switch engaged.

**Dependency:** this change reuses the human send lifecycle (`approved -> executing -> sent`, the recipient digest, the at-most-once rule and `SendOutcome`) that `email-send` introduces. It must be applied after `email-send` is archived or at least merged. Until then the send tasks are blocked.

## What Changes

- Add a `haste-connector-beeper` crate: a Beeper Desktop API client over `http://127.0.0.1:23373` (loopback only), a token read from the macOS Keychain, and one connector registration per source: `whatsapp`, `messenger` and `imessage`. Each registration has its own kill switch and circuit breaker.
- Reads are poll-based and restricted to chats the human selected in a non-secret `beeper.json` (account ids and a chat allow-list and deny-list). Nothing is read by default. Reads are bounded by page size, history window and body size caps.
- Message bodies are stored only as bounded local text. Audit, logs and errors carry counts and fixed labels only.
- **On-device only**: before reading or sending, the connector checks each selected account's connection type as exposed by the API. A cloud connection is refused with a clear reason. If the API does not expose the type, the human attests on-device use per account in `beeper.json`, and a spike task confirms whether the API exposes it.
- **Human-only send** into an existing selected chat: one recipient chat per send, a confirmation that shows the chat name and the text, a digest check, at-most-once, never retried. Ambiguous outcomes stay `executing` for a human check. Per-hour and per-day caps are named constants.
- Agents keep proposing replies as `Draft` proposals; approval marks them approved and never calls Beeper. Agents have no send path. `Action::Send` stays human-only in the gate.
- Starting new chats, attachments and reactions are out of scope for v1.
- The scope mechanism applies to chats: human-set project tags from herdr workspace labels, with the machine-principal filter in the daemon read path.
- Human spike group before any code and a manual smoke group that uses a throwaway chat.
- README section with the Terms-of-Service risk acknowledged, the on-device requirement and the setup steps.

**BREAKING**: none. `Source::Whatsapp`, `Source::Messenger` and `Source::Imessage` already exist in `haste-core`.

## Capabilities

### New Capabilities
- `connector-beeper`: the Beeper Desktop API connector: loopback-only access, Keychain token, selected-chat reads, on-device enforcement, bounded human-only sends with the send limits, kill switch on first install, and counts-only audit.

### Modified Capabilities
- `proposals`: a Beeper `Draft` proposal targets an existing chat, and approving it creates nothing at the provider.
- `approvals-tui`: the send confirmation for a chat shows the chat name, the text and the digest, and a Beeper needs-check row shows the outcome label only.
- `machine-scope`: chat threads from Beeper carry the same project tags and machine filter as other threads.

## Impact

- Code: `crates/haste-connector-beeper` (new: client, token store, mapper, config, connector, auth helper); `crates/haste-core` (chat send payload fields, Beeper constants); `crates/haste-store` (per-source send window); `crates/hasted` (registration, scheduler, human-only send path); `crates/haste` (`connector beeper auth` verb, human build only); `crates/haste-tui` (confirmation and needs-check rows).
- External system: Beeper Desktop must be running on the hub Mac, with the API enabled and a token created in its settings. Beeper Desktop is not a dependency of the build.
- Dependencies: `reqwest` (already used by the Google connectors), `serde_json`, the existing Keychain wrapper pattern (`security-framework`). No new SMTP or messaging SDK. The official Beeper SDKs are not used, since the crate talks to the documented HTTP endpoints directly.
- Systems: macOS only (Keychain, Beeper Desktop on this Mac).
- Tests: fakes only, a local fake Beeper server on loopback. No test contacts a real Beeper instance or a real network.
- Docs: README section (setup, on-device requirement, limits, ToS risk).
- Related parked change: `imessage-connector` covers `Source::Imessage` by reading `chat.db`. This change offers an alternative route to the same source. Whether the two coexist or one supersedes the other is an open decision (see design.md).
