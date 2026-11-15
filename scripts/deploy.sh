#!/bin/bash
# Reinstall haste's binaries, sign them with a stable identity and restart hasted.
#
# Signing keeps the macOS Keychain "Always Allow" valid across rebuilds: the approval is tied to
# the signature (your certificate and the binary's name), which does not change when the code
# does. Unsigned rebuilds look like a new program every time and ask again.
#
# hasted runs from a small app bundle (~/Applications/Haste.app) so macOS gives it its own
# privacy identity: a Reminders permission granted to Terminal does not reach a launchd job.
# ~/.cargo/bin/hasted is a symlink to the bundle's executable, so the launchd job is unchanged.
#
# Usage: scripts/deploy.sh [hasted] [tui] [cli] [helpers] [reminders]   (default: all but reminders)
# Env:   HASTE_SIGN_IDENTITY  certificate hash to sign with (default: the first Apple Development
#                             identity found by `security find-identity`)
set -euo pipefail
cd "$(dirname "$0")/.."

targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(hasted tui cli helpers)

identity="${HASTE_SIGN_IDENTITY:-$(security find-identity -v -p codesigning \
    | awk '/Apple Development/ {print $2; exit}')}"
if [ -z "$identity" ]; then
    echo "no code-signing identity found; set HASTE_SIGN_IDENTITY (see: security find-identity -v -p codesigning)" >&2
    exit 1
fi

bin="$HOME/.cargo/bin"
signed=()
sign() {
    for name in "$@"; do
        codesign --force --sign "$identity" "$bin/$name"
        signed+=("$name")
    done
}

for t in "${targets[@]}"; do
    case "$t" in
    hasted)
        # cargo would write through the symlink into the signed bundle: remove it first
        [ -L "$bin/hasted" ] && rm "$bin/hasted"
        cargo install --path crates/hasted --features gmail,icloud,gcal,beeper --locked
        app="$HOME/Applications/Haste.app"
        mkdir -p "$app/Contents/MacOS"
        cp "$bin/hasted" "$app/Contents/MacOS/hasted"
        cp packaging/Haste-Info.plist "$app/Contents/Info.plist"
        codesign --force --sign "$identity" "$app"
        rm "$bin/hasted"
        ln -s "$app/Contents/MacOS/hasted" "$bin/hasted"
        signed+=(Haste.app)
        ;;
    tui)
        cargo install --path crates/haste-tui --locked
        sign haste-tui
        ;;
    cli)
        cargo install --path crates/haste --features human --locked
        sign haste
        ;;
    helpers)
        cargo install --path crates/haste-connector-gmail --locked
        cargo install --path crates/haste-connector-gcal --locked
        cargo install --path crates/haste-connector-beeper --locked
        cargo install --path crates/haste-connector-icloud --locked
        sign haste-gmail-auth haste-gcal-auth haste-beeper-auth haste-icloud-auth \
            haste-icloud-diag
        ;;
    reminders)
        # The Swift EventKit helper; its Reminders permission follows the signature too.
        crates/haste-connector-reminders/helper/build.sh target/helper
        cp target/helper/haste-reminders-helper "$bin/haste-reminders-helper"
        codesign --force --sign "$identity" --identifier dev.haste.reminders-helper \
            "$bin/haste-reminders-helper"
        signed+=(haste-reminders-helper)
        ;;
    *)
        echo "unknown target: $t (hasted, tui, cli, helpers, reminders)" >&2
        exit 2
        ;;
    esac
done
echo "signed: ${signed[*]}"

# Restart the daemon only when it is the launchd job; never touch one started by hand.
uid="$(id -u)"
if launchctl print "gui/$uid/com.haste.hasted" >/dev/null 2>&1; then
    case " ${targets[*]} " in
    *" hasted "* | *" helpers "* | *" reminders "*)
        launchctl kickstart -k "gui/$uid/com.haste.hasted"
        echo "hasted restarted (launchd)"
        ;;
    *) echo "hasted unchanged; no restart needed" ;;
    esac
elif pgrep -x hasted >/dev/null; then
    echo "hasted is running outside launchd: quit it and start it again by hand (or install the launchd job)"
else
    echo "hasted is not running and the launchd job is not installed"
fi
