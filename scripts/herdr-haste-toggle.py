#!/usr/bin/env python3
"""Keep haste running in a Herdr tab of its own and flip to it and back with one key.

  herdr-haste-toggle.py          on any tab: go to the haste tab (made the first time); on the
                                 haste tab: go back to the tab you came from
  herdr-haste-toggle.py --leave  go back to the tab you came from (the TUI calls this on `q`)

The tab keeps the TUI running, so it is exactly where you left it. `Q` in the TUI really quits
it, which closes the tab; the next toggle makes a new one. Only the Herdr CLI is used.
Bind it in Herdr's config.toml:

  [[keys.command]]
  key = "prefix+i"
  type = "shell"
  command = "~/.config/herdr/haste-toggle.py"
"""

import json
import os
import subprocess
import sys
from pathlib import Path

HERDR = os.environ.get("HERDR_BIN_PATH") or "herdr"
STATE_DIR = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state") / "haste-herdr"
LABEL = "haste"
TUI_COMMAND = "haste-tui; exit"


def herdr(*args: str) -> dict[str, object]:
    """One Herdr CLI call; its JSON answer, or an empty one when there is none."""
    done = subprocess.run([HERDR, *args], capture_output=True, text=True, check=False)
    try:
        answer = json.loads(done.stdout)
    except ValueError:
        return {}
    return answer if isinstance(answer, dict) else {}


def read(name: str) -> str:
    path = STATE_DIR / name
    return path.read_text().strip() if path.exists() else ""


def write(name: str, value: str) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    (STATE_DIR / name).write_text(value)


def open_tab_ids() -> set[str]:
    result = herdr("tab", "list").get("result")
    tabs = result.get("tabs", []) if isinstance(result, dict) else []
    return {t["tab_id"] for t in tabs if isinstance(t, dict) and "tab_id" in t}


def go_back(ids: set[str]) -> None:
    previous = read("previous")
    if previous in ids:
        herdr("tab", "focus", previous)


def make_tab(current: str) -> None:
    write("previous", current)
    me = str(Path(__file__).resolve())
    answer = herdr(
        "tab", "create", "--label", LABEL, "--cwd", str(Path.home()), "--focus",
        "--env", "HASTE_HERDR_TAB=1", "--env", f"HASTE_HERDR_TOGGLE={me}",
    )
    result = answer.get("result")
    if not isinstance(result, dict):
        return
    tab, pane = result.get("tab"), result.get("root_pane")
    if isinstance(tab, dict) and isinstance(pane, dict):
        write("haste_tab", str(tab.get("tab_id", "")))
        herdr("pane", "run", str(pane.get("pane_id", "")), TUI_COMMAND)


def main(argv: list[str]) -> None:
    ids = open_tab_ids()
    haste = read("haste_tab")
    if "--leave" in argv:
        go_back(ids)
        return
    current = os.environ.get("HERDR_ACTIVE_TAB_ID", "")
    if haste in ids:
        if current == haste:
            go_back(ids)
        else:
            write("previous", current)
            herdr("tab", "focus", haste)
    else:
        make_tab(current)


if __name__ == "__main__":
    main(sys.argv[1:])
