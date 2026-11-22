---
name: reauth-google
description: Re-authorise the haste Gmail and Google Calendar connectors after their kill switches engage (checkpoint / invalid_grant / "stopped again"), using the OAuth client in .env and the Chrome browser tools, then release them and verify sync.
---

# Re-authorise haste Gmail / Google Calendar

Use when `haste connector status` shows `gmail` or `gcal` with `"engaged": true`, the log shows `breaker_opened` with `error_class: checkpoint`, or a plain `haste connector release` re-engages at once (the refresh token is rejected or unreadable).

The user has pre-approved this flow: use the `.env` client and click through the Google consent in Chrome. Never print the client secret. Do not restart `hasted`; releases take effect without it.

## 1. Diagnose

```
haste connector status
grep -E '"source":"(gmail|gcal)"' ~/Library/Logs/hasted.log | grep -v refused_kill_switch | tail
```

Look for `breaker_opened` and `error_class`. Note `last_synced` of each connector.

## 2. Run auth (once per connector that needs it)

`auth` blocks until the browser callback arrives, so run it in the background with output to the scratchpad. Load `.env` in the same command (`HASTE_GMAIL_CLIENT_ID` / `HASTE_GMAIL_CLIENT_SECRET`). gcal reuses the same variables.

```
cd <haste repo>
set -a && . ./.env && set +a && nohup haste connector gmail auth > <scratchpad>/gmail_auth.out 2>&1 &
sleep 4; grep -o 'https://accounts.google.com[^ ]*' <scratchpad>/gmail_auth.out
```

Same for `haste connector gcal auth` (output `gcal_auth.out`). The local redirect port (`127.0.0.1:<port>`) differs per run, so use the URL printed by that run.

The background-task completion notice only means the launcher exited; the auth process (`pgrep -fl "connector gmail auth"`) is still waiting for the browser.

## 3. Consent in Chrome

Load the tools first: ToolSearch `select:mcp__claude-in-chrome__tabs_context_mcp,mcp__claude-in-chrome__navigate,mcp__claude-in-chrome__browser_batch,mcp__claude-in-chrome__computer,mcp__claude-in-chrome__find`. Call `tabs_context_mcp` (createIfEmpty), then batch:

1. `navigate` the tab to the printed auth URL.
2. Click the account in "Choose an account" (screenshot first for coordinates).
3. "Google hasn't verified this app" is expected for the user's own project: click **Advanced**, then **Go to tase (unsafe)**.
4. Tick **Select all**, scroll down, click **Continue** (use `find` for the button ref if coordinates fail). Gmail asks for 3 scopes (read, compose, modify); calendar asks for 2 (calendar list read, events).
5. A "Permission denied for this domain" error on a screenshot after Continue is just the `127.0.0.1` redirect; check the auth output instead.

Success: the `.out` file ends with `gmail authorization finished` / `gcal authorization finished`.

## 4. Release and verify

```
haste connector release gmail
haste connector release gcal
haste connector status          # engaged: false
```

Within about five minutes `last_synced` advances for both (gmail every ~5 min, gcal every ~5 min). Poll with a backgrounded `until ... ; do sleep 5; done` loop, not a foreground sleep.

## Notes

- If it breaks again within a few days, the OAuth consent screen is probably in "Testing" mode (7-day refresh-token expiry). Publish the app to "In production" in the Google Cloud console to stop that.
- `hasted` does not log the checkpoint reason (invalid_grant vs. missing/unreadable Keychain token); the class is all you get.
- `.env` is gitignored and holds your own OAuth client; the app name on the consent screen (`tase` in step 3) is whatever you named your Google Cloud project.
- Tell the user whether `hasted` was restarted (it normally is not).
