#!/usr/bin/env python3
"""Ключи OpenCode Console: список, выпуск, живая проверка.

Механизм (измерен 20.11.2026, см. AGENTS.md «OpenCode Go»):

* Анонимный доступ — это НЕ ключ. Клиент opencode без авторизации шлёт
  литеральную строку ``public`` как API-ключ (в бинаре: ``apiKey = "public"``).
  Так работают только бесплатные модели Zen; платные площадка отклоняет
  «Missing API key». Квота анонимного пула общая на клиента, не на сессию.
* Настоящие ключи выпускает консоль OpenCode под учётной записью:
  сервис-аккаунты → ключи. Формат ключа: ``oc_sk_<12 hex>_<32 симв.>``
  либо ``sk-<64 симв.>``; у ключа есть permissions (all/read/restricted/
  inference-only), scopes (в т.ч. ``keys:write``) и срок жизни.
* План Go — платная подписка (Go $10/мес, Go Plus $40/мес), квоты привязаны
  к подписке, а не к числу ключей.

Требуется своя сессия консоли (Bearer-токен OAuth из device-flow либо
cookie сессии). Учётные записи скрипт не создаёт.

Примеры::

    OPENCODE_CONSOLE_TOKEN=... python3 scripts/opencode_keys.py list
    OPENCODE_CONSOLE_TOKEN=... python3 scripts/opencode_keys.py mint --count 5
    python3 scripts/opencode_keys.py probe --key-file ~/.heterion-router/opencode-keys.json
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

CONSOLE_BASE = "https://opencode.ai/console/api"
ZEN_BASE = "https://opencode.ai/zen/v1"
GO_BASE = "https://opencode.ai/zen/go/v1"

# Сессионный идентификатор для проб: площадка требует стабильный
# x-opencode-session внутри разговора (ADR-контракт «OpenCode Console»).
PROBE_SESSION = "ses_" + "0" * 32


def _auth_headers(args) -> dict[str, str]:
    headers = {"Content-Type": "application/json", "User-Agent": "heterion-router/1.0"}
    if args.console_token:
        headers["Authorization"] = f"Bearer {args.console_token}"
    if args.console_cookie:
        headers["Cookie"] = args.console_cookie
    return headers


def request(method: str, url: str, body=None, headers=None, timeout: int = 45):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers=headers or {}, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", "replace")
            return resp.status, raw
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read().decode("utf-8", "replace")
    except urllib.error.URLError as exc:
        return None, f"transport error: {exc.reason}"


def jload(raw: str):
    try:
        return json.loads(raw)
    except Exception:
        return None


def print_error(status, raw: str) -> None:
    print(f"ошибка: http={status}", file=sys.stderr)
    parsed = jload(raw)
    if isinstance(parsed, dict):
        err = parsed.get("error")
        if isinstance(err, dict):
            print(f"  {err.get('type') or ''} {err.get('message') or ''}".strip(), file=sys.stderr)
            return
        if err:
            print(f"  {err}", file=sys.stderr)
            return
        print(f"  {json.dumps(parsed, ensure_ascii=False)[:300]}", file=sys.stderr)
        return
    if raw:
        print(f"  {raw[:300]}", file=sys.stderr)


# ---------------------------------------------------------------- чтение


def cmd_list(args) -> int:
    headers = _auth_headers(args)
    status, raw = request("GET", f"{CONSOLE_BASE}/service-accounts", headers=headers)
    if status != 200:
        print_error(status, raw)
        return 1
    parsed = jload(raw) or []
    accounts = parsed.get("accounts") if isinstance(parsed, dict) else parsed
    if not isinstance(accounts, list):
        accounts = []
    print(f"сервис-аккаунтов: {len(accounts)}")
    for acc in accounts:
        print(f"  {acc.get('serviceAccountID')}  {acc.get('name')}")
        for key in acc.get("keys") or []:
            hint = key.get("tokenHint") or key.get("keyPrefix") or ""
            print(
                f"      ключ {key.get('serviceAPIKeyID')}  {hint}  "
                f"perm={key.get('permissions')}  status={key.get('status')}  "
                f"expires={key.get('expiresAt')}"
            )
    return 0


# ---------------------------------------------------------------- выпуск


def cmd_mint(args) -> int:
    headers = _auth_headers(args)
    created = []
    for index in range(args.count):
        name = f"{args.prefix}-{index + 1:02d}"
        status, raw = request(
            "POST",
            f"{CONSOLE_BASE}/service-accounts",
            body={"name": name, "ipAllowlist": args.ip_allowlist},
            headers=headers,
        )
        if status not in (200, 201):
            print_error(status, raw)
            return 1
        account = jload(raw) or {}
        account_id = account.get("serviceAccountID") or account.get("id")
        if not account_id:
            print_error(status, raw)
            return 1

        payload = {
            "name": name,
            "permissions": args.permissions,
            "expiresAt": args.expires_at,
        }
        if args.scopes:
            payload["scopes"] = args.scopes
        status, raw = request(
            "POST",
            f"{CONSOLE_BASE}/service-accounts/{account_id}/keys",
            body=payload,
            headers=headers,
        )
        if status not in (200, 201):
            print_error(status, raw)
            return 1
        issued = jload(raw) or {}
        key = issued.get("key") or {}
        token = issued.get("token") or key.get("token")
        created.append(
            {
                "name": name,
                "service_account_id": account_id,
                "service_api_key_id": key.get("serviceAPIKeyID"),
                "token": token,
                "permissions": key.get("permissions"),
                "scopes": key.get("scopes"),
                "expires_at": key.get("expiresAt"),
            }
        )
        hint = key.get("tokenHint") or (token[:16] + "…" if token else "?")
        print(f"выпущен {name}: {account_id}  ключ {hint}")

    if args.out:
        path = os.path.expanduser(args.out)
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(created, handle, ensure_ascii=False, indent=2)
        os.chmod(path, 0o600)
        print(f"ключи записаны в {path} (0600)")
    else:
        for item in created:
            print(json.dumps(item, ensure_ascii=False))
    return 0


# ---------------------------------------------------------------- проверка


def probe_key(token: str, base: str, model: str) -> tuple[int, str]:
    headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json",
        "x-opencode-session": PROBE_SESSION,
        "User-Agent": "heterion-router/1.0",
    }
    status, raw = request(
        "POST",
        f"{base}/chat/completions",
        body={
            "model": model,
            "messages": [{"role": "user", "content": "say ok"}],
            "max_tokens": 16,
        },
        headers=headers,
    )
    return status, raw


def cmd_probe(args) -> int:
    items = []
    if args.key_file:
        parsed = jload(open(os.path.expanduser(args.key_file), encoding="utf-8").read()) or []
        items = parsed if isinstance(parsed, list) else []
    elif args.key:
        items = [{"name": "ad-hoc", "token": args.key}]
    if not items:
        print("нечего проверять: нужен --key или --key-file", file=sys.stderr)
        return 1

    failures = 0
    for item in items:
        token = item.get("token") or ""
        if not token:
            continue
        for label, base in (("zen", ZEN_BASE), ("go", GO_BASE)):
            status, raw = probe_key(token, base, args.model)
            note = ""
            parsed = jload(raw)
            if isinstance(parsed, dict):
                err = parsed.get("error")
                if isinstance(err, dict):
                    note = (err.get("message") or err.get("type") or "")[:90]
                elif err:
                    note = str(err)[:90]
                elif parsed.get("choices"):
                    note = "ответ получен"
            elif raw:
                note = raw[:90]
            ok = status == 200
            failures += 0 if ok else 1
            print(f"{item.get('name')} [{label}] http={status} {'OK' if ok else '—'} {note}")
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Ключи OpenCode Console")
    parser.add_argument("--console-token", default=os.environ.get("OPENCODE_CONSOLE_TOKEN"))
    parser.add_argument("--console-cookie", default=os.environ.get("OPENCODE_CONSOLE_COOKIE"))
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("list", help="показать сервис-аккаунты и ключи")

    mint = sub.add_parser("mint", help="выпустить сервис-аккаунты с ключами")
    mint.add_argument("--count", type=int, default=1)
    mint.add_argument("--prefix", default="router")
    mint.add_argument("--permissions", default="inference-only")
    mint.add_argument("--scopes", nargs="*", default=["models:request", "models:read"])
    mint.add_argument("--expires-at", default=None)
    mint.add_argument("--ip-allowlist", nargs="*", default=[])
    mint.add_argument("--out", default=None)

    probe = sub.add_parser("probe", help="живая проверка ключей против zen/go")
    probe.add_argument("--key")
    probe.add_argument("--key-file")
    probe.add_argument("--model", default="big-pickle")

    args = parser.parse_args()
    if args.command == "list":
        return cmd_list(args)
    if args.command == "mint":
        return cmd_mint(args)
    return cmd_probe(args)


if __name__ == "__main__":
    sys.exit(main())
