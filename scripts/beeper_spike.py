"""Beeper live spike (tasks 1.2-1.7): structure-only, read-only.

Run:  uv run python scripts/beeper_spike.py
The token is typed at a hidden prompt, kept in memory, never written anywhere.
Only GET requests are made. Output has key names, value types, counts and the value set of
enum-like fields; never a chat name, message text, account name or id. Paste the output back.
"""

import getpass
import json
import re
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any

BASE = "http://127.0.0.1:23373"
TIMEOUT_SECS = 10
ENUMISH = re.compile(
    r"network|type|connection|kind|status|provider|bridge|platform|source",
    re.IGNORECASE,
)
MAX_ENUM_LEN = 30
PAGE_LIMIT = 5


@dataclass
class Reply:
    status: int
    body: Any


def get(path: str, token: str | None) -> Reply:
    request = urllib.request.Request(BASE + path)
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECS) as response:
            raw = response.read()
            status = response.status
    except urllib.error.HTTPError as e:
        raw, status = e.read(), e.code
    except OSError as e:
        return Reply(0, f"unreachable: {type(e).__name__}")
    try:
        return Reply(status, json.loads(raw))
    except ValueError:
        return Reply(status, f"non-json body, {len(raw)} bytes")


def shape_of(value: Any) -> str:
    if isinstance(value, dict):
        return "object"
    if isinstance(value, list):
        return "array"
    if isinstance(value, str):
        return (
            "string(" + re.sub(r"[0-9]", "9", value)[:12] + ")"
            if re.search(r"\d{4}-", value)
            else "string"
        )
    return type(value).__name__


def items_of(body: Any) -> list[dict[str, Any]]:
    if isinstance(body, list):
        return [x for x in body if isinstance(x, dict)]
    if isinstance(body, dict):
        for key in ("items", "data", "accounts", "chats", "messages", "results"):
            if isinstance(body.get(key), list):
                return [x for x in body[key] if isinstance(x, dict)]
    return []


def describe(label: str, reply: Reply) -> list[dict[str, Any]]:
    print(f"\n== {label}: status {reply.status}")
    if not isinstance(reply.body, (dict, list)):
        print(f"   {reply.body}")
        return []
    if isinstance(reply.body, dict):
        print(
            "   top-level keys:",
            {
                k: shape_of(v)
                for k, v in reply.body.items()
                if k not in ("items", "data")
            },
        )
    items = items_of(reply.body)
    print(f"   items: {len(items)}")
    fields: dict[str, set[str]] = {}
    enums: dict[str, set[str]] = {}
    for item in items:
        for key, value in item.items():
            fields.setdefault(key, set()).add(shape_of(value))
            if (
                ENUMISH.search(key)
                and isinstance(value, str)
                and len(value) <= MAX_ENUM_LEN
            ):
                enums.setdefault(key, set()).add(value)
    for key, kinds in sorted(fields.items()):
        print(f"   field {key}: {sorted(kinds)}")
    for key, values in sorted(enums.items()):
        print(f"   VALUES of {key}: {sorted(values)}")
    return items


def _ordered(values: list[Any]) -> bool:
    keys = [str(v) for v in values]
    return keys == sorted(keys) or keys == sorted(keys, reverse=True)


def pick(item: dict[str, Any], *names: str) -> str | None:
    for name in names:
        if isinstance(item.get(name), str):
            return item[name]
    return None


def main() -> None:
    print("1.7a wrong token (expect a 401/403 error shape):")
    describe("GET /v1/info with a wrong token", get("/v1/info", "wrong-token"))
    token = getpass.getpass("Beeper API token (hidden): ")

    describe("GET /v1/info", get("/v1/info", token))
    accounts = describe("GET /v1/accounts", get("/v1/accounts", token))
    per_network: dict[str, int] = {}
    for account in accounts:
        network = pick(account, "network", "type", "platform") or "?"
        per_network[network] = per_network.get(network, 0) + 1
    print("   accounts per network value:", per_network)

    account_id = pick(accounts[0], "id", "accountID") if accounts else None
    if account_id:
        describe("GET /v1/accounts/{id}", get(f"/v1/accounts/{account_id}", token))
        describe(
            "GET /v1/bridges/{id}/logins",
            get(f"/v1/bridges/{account_id}/logins", token),
        )

    chats = describe("GET /v1/chats", get(f"/v1/chats?limit={PAGE_LIMIT}", token))
    chat_id = pick(chats[0], "id", "chatID") if chats else None
    if chat_id:
        describe("GET /v1/chats/{id}", get(f"/v1/chats/{chat_id}", token))
        messages = describe(
            "GET /v1/chats/{id}/messages",
            get(f"/v1/chats/{chat_id}/messages?limit={PAGE_LIMIT}", token),
        )
        for key in ("sortKey", "sort_key", "timestamp", "createdAt", "date"):
            if messages and key in messages[0]:
                values = [m.get(key) for m in messages]
                kinds = {shape_of(v) for v in values}
                print(
                    f"   {key}: shapes {sorted(kinds)}, strictly one direction: "
                    f"{_ordered(values)}"
                )
    print(
        "\nDone. Nothing was sent. Also do 1.7b by hand: quit Beeper Desktop, rerun, paste the 'unreachable' line."
    )


if __name__ == "__main__":
    main()
