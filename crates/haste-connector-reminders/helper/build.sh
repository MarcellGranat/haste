#!/bin/sh
# Build the EventKit helper with the system toolchain and ad-hoc sign it.
# Output: target/helper/haste-reminders-helper (override with the first argument).
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
out="${1:-$here/../../../target/helper}"
mkdir -p "$out"
swiftc -O -o "$out/haste-reminders-helper" "$here/Sources/main.swift" \
  -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker "$here/Info.plist"
codesign --force --sign - --identifier dev.haste.reminders-helper "$out/haste-reminders-helper"
"$out/haste-reminders-helper" --check-fixture "$here/fixtures/protocol.json"
echo "built $out/haste-reminders-helper"
