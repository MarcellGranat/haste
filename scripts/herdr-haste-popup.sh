#!/bin/sh
# Open haste in a Herdr popup that keeps running when you close it.
#
# tmux holds the TUI in a session of its own (on its own socket, `-L haste`); the popup only
# attaches to it. Ctrl-q in the TUI detaches (the popup closes, the TUI keeps running) and
# Ctrl-Q or Ctrl-c quits it. The next `prefix+i` attaches again, where you left off.
#
#   [[keys.command]]
#   key = "prefix+i"
#   type = "popup"
#   command = "~/.config/herdr/haste-popup.sh"
#   width = "92%"
#   height = "90%"
CONF="${HASTE_TMUX_CONF:-$HOME/.config/herdr/haste-tmux.conf}"
exec tmux -L haste -f "$CONF" new-session -A -s haste \
  -e "HASTE_BACKGROUND_COMMAND=tmux -L haste detach-client" haste-tui
