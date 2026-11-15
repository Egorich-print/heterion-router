#!/usr/bin/env python3
"""Прогон моделей MCode через роутер: напрямую и через комбо `auto`.

Две части:

1. **Прямая проверка** каждой модели, доступной в MCode (список берётся из
   `~/.minimax/config.yaml`): живёт ли модель, сколько отвечает, какая ошибка.
2. **Лестница Switchyard**: те же запросы уходят на `auto` — единственную
   точку входа, — и по журналу `call_logs` видно, какой бэкенд выбрал роутер
   под каждый запрос.

Особенность, из-за которой прогон может обмануть: кэш роутера живёт час, а
ключ — SHA-256 канонического тела запроса. Поэтому в каждый запрос подставляется
свой nonce: одинаковые запросы не дошли бы до апстрима и «измерили» бы кэш.

Использование:
    scripts/sweep_models.py --mode both --out docs/research/sweep-<date>.md
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROUTER = "http://127.0.0.1:20128"
KEY_FILE = Path.home() / ".heterion-router/desktop/operator-key.json"
CONFIG = Path.home() / ".minimax/config.yaml"
DB = Path.home() / ".heterion-router/storage.sqlite"

PROMPT = "Ответь одним словом: работает? Проверка {nonce}."


def operator_key() -> str:
    return KEY_FILE.read_text().strip()


def mcode_models() -> list[tuple[str, str]]:
    """Models registered for the operator in the MCode config."""
    raw = CONFIG.read_text()
    start = raw.find("custom_provider:")
    if start < 0:
        return []
    block = raw[start:]
    pairs = re.findall(r'^      "([^"]+)":\n        name: "([^"]+)"', block, re.M)
    return [(mid, name) for mid, name in pairs]


def post_chat(model: str, prompt: str, timeout: int, max_tokens: int = 60) -> dict:
    body = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "temperature": 0.2,
        }
    ).encode()
    request = urllib.request.Request(
        f"{ROUTER}/v1/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {operator_key()}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = json.loads(response.read())
        elapsed = int((time.monotonic() - started) * 1000)
        choices = payload.get("choices") or []
        content = ""
        if choices:
            content = (choices[0].get("message") or {}).get("content") or ""
        return {
            "status": 200,
            "ms": elapsed,
            "text": content.strip()[:80],
            "finish": choices[0].get("finish_reason") if choices else None,
        }
    except urllib.error.HTTPError as error:
        elapsed = int((time.monotonic() - started) * 1000)
        try:
            detail = json.loads(error.read()).get("error", {})
        except Exception:  # noqa: BLE001
            detail = {}
        message = detail.get("message") if isinstance(detail, dict) else str(detail)
        return {"status": error.code, "ms": elapsed, "text": (message or "")[:110]}
    except Exception as error:  # noqa: BLE001
        elapsed = int((time.monotonic() - started) * 1000)
        return {"status": 0, "ms": elapsed, "text": f"{type(error).__name__}: {error}"[:110]}


def served_backends(since: str) -> list[tuple[str, str, str, int]]:
    """(requested_model, provider, model, status) from the call log."""
    if not DB.exists():
        return []
    connection = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    try:
        rows = connection.execute(
            "select requested_model, provider, model, status from call_logs "
            "where timestamp >= ? order by timestamp",
            (since,),
        ).fetchall()
    except sqlite3.Error:
        return []
    finally:
        connection.close()
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("direct", "auto", "both"), default="both")
    parser.add_argument("--timeout", type=int, default=60)
    parser.add_argument("--out", default="")
    args = parser.parse_args()

    models = mcode_models()
    if not models:
        print("модели MCode не найдены в конфиге", file=sys.stderr)
        return 2
    print(f"моделей в MCode: {len(models)}")

    stamp = time.strftime("%Y-%m-%d %H:%M:%S", time.gmtime())
    since = stamp
    report: dict = {"stamp": stamp, "direct": [], "auto": []}

    if args.mode in ("direct", "both"):
        print("\n== прямая проверка каждой модели ==")
        for model_id, name in models:
            result = post_chat(model_id, PROMPT.format(nonce=f"d{model_id}{time.time_ns()}"), args.timeout)
            report["direct"].append({"id": model_id, "name": name, **result})
            print(f"  {model_id:45s} {result['status']:>3} {result['ms']:>6} мс  {result['text'][:60]}")

    if args.mode in ("auto", "both"):
        print("\n== лестница Switchyard: те же модели через `auto` ==")
        for model_id, name in models:
            result = post_chat("auto", PROMPT.format(nonce=f"a{model_id}{time.time_ns()}"), args.timeout)
            report["auto"].append({"id": model_id, "name": name, **result})
            print(f"  {model_id:45s} {result['status']:>3} {result['ms']:>6} мс  {result['text'][:60]}")

    rows = served_backends(since)
    report["call_log"] = rows
    print(f"\n== журнал вызовов: {len(rows)} строк ==")
    picked: dict[str, int] = {}
    for requested, provider, model, status in rows:
        key = f"{provider}/{model}" if provider else str(model)
        picked[key] = picked.get(key, 0) + 1
    for key, count in sorted(picked.items(), key=lambda item: -item[1]):
        print(f"  {key:50s} {count}")

    if args.out:
        out = Path(args.out)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, indent=2, ensure_ascii=False))
        print(f"\nотчёт: {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
