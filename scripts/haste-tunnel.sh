#!/bin/sh
# Share this machine's hasted socket with another machine, over ssh (the tailnet).
#
#   scripts/haste-tunnel.sh <ssh-host>
#
# The other machine then runs the TUI against it with
#   HASTE_HOME=~/.haste-remote haste-tui
# after you copy this machine's human.token to ~/.haste-remote/human.token there (see README,
# "Running the TUI on another machine"). Only the socket is shared: the token is still needed for
# every request, and nothing else of this machine is exposed.
#
# It runs in the foreground and exits if the connection drops; run it from a launchd job with
# KeepAlive (deploy/com.haste.tunnel.plist) to have it come back by itself.
set -eu
host="${1:?usage: haste-tunnel.sh <ssh-host>}"
local_sock="${HASTE_HOME:-$HOME/Library/Application Support/haste}/run/hasted.sock"
[ -S "$local_sock" ] || { echo "no hasted socket at $local_sock" >&2; exit 1; }
remote_home="$(ssh -o BatchMode=yes "$host" 'printf %s "$HOME"')"
remote_dir="$remote_home/.haste-remote/run"
remote_sock="$remote_dir/hasted.sock"
# a socket left by an earlier connection would make the forward fail
ssh -o BatchMode=yes "$host" "mkdir -p '$remote_dir' && chmod 700 '$remote_home/.haste-remote' && rm -f '$remote_sock'"
exec ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 \
    -o ServerAliveCountMax=3 -R "$remote_sock:$local_sock" "$host"
