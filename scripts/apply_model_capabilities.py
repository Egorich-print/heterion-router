#!/usr/bin/env python3
"""Собрать capability-таблицу роутера из research-данных.

Вход: `docs/research/ladder/*.jsonl` — по одной строке на семейство моделей,
с полями `context`, `vision`, `tools`, `reasoning`, `tier` и списком `models`
(точные id из живого каталога).

Выход: строки в `model_capabilities` (что модель умеет) и в
`model_capability_overrides` (класс лестницы под ключом `tier`) — по одной
строке на пару (провайдер, модель). Провайдер берётся из списка живых
моделей: один и тот же id может отдавать несколько площадок.

Измеренные контексты (`model_context_overrides`) не трогаются: роутер
предпочитает измеренное значение объявленному.

Использование:
    scripts/apply_model_capabilities.py --dry-run
    scripts/apply_model_capabilities.py --apply
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LADDER_DIR = ROOT / "docs/research/ladder"
DB = Path.home() / ".heterion-router/storage.sqlite"
KEY_FILE = Path.home() / ".heterion-router/desktop/operator-key.json"
ROUTER = "http://127.0.0.1:20128"

TIERS = ("frontier", "fast", "fallback")


def live_models() -> dict[str, list[str]]:
    """Живой каталог роутера: провайдер → список моделей."""
    key = KEY_FILE.read_text().strip()
    request = urllib.request.Request(
        f"{ROUTER}/v1/models", headers={"Authorization": f"Bearer {key}"}
    )
    payload = json.load(urllib.request.urlopen(request))
    catalog: dict[str, list[str]] = defaultdict(list)
    for entry in payload["data"]:
        identifier = entry["id"]
        if "/" not in identifier:
            continue
        provider, model = identifier.split("/", 1)
        if provider in ("auto", "heterion-router"):
            continue
        catalog[provider].append(model)
    return dict(catalog)


def provider_of_model(catalog: dict[str, list[str]]) -> dict[str, list[str]]:
    """Модель → провайдеры, которые её отдают."""
    mapping: dict[str, list[str]] = defaultdict(list)
    for provider, models in catalog.items():
        for model in models:
            mapping[model].append(provider)
    return dict(mapping)


def read_research() -> list[dict]:
    rows: list[dict] = []
    for path in sorted(LADDER_DIR.glob("*.jsonl")):
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as error:
                print(f"  !! {path.name}:{number} не JSON: {error}", file=sys.stderr)
                continue
            row["_source"] = f"{path.name}:{number}"
            rows.append(row)
    return rows


def normalise(row: dict) -> dict | None:
    """Привести строку research к виду, который пишем в БД."""
    tier = str(row.get("tier", "")).strip().lower()
    if tier not in TIERS:
        return None
    context = row.get("context")
    if not isinstance(context, int) or context <= 0:
        return None
    return {
        "family": str(row.get("family", "")).strip() or None,
        "tier": tier,
        "context": context,
        "vision": bool(row.get("vision")),
        "tools": bool(row.get("tools")),
        "reasoning": bool(row.get("reasoning")),
        "models": [str(model) for model in (row.get("models") or [])],
        "source": row.get("_source"),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--apply", action="store_true", help="записать в БД")
    parser.add_argument("--dry-run", action="store_true", help="только показать")
    args = parser.parse_args()
    if not args.apply and not args.dry_run:
        args.dry_run = True

    catalog = live_models()
    owners = provider_of_model(catalog)
    research = read_research()
    print(f"живых провайдеров: {len(catalog)}, research-строк: {len(research)}")

    planned: dict[tuple[str, str], dict] = {}
    unknown: list[str] = []
    for row in research:
        clean = normalise(row)
        if clean is None:
            print(f"  !! строка без класса/контекста: {row.get('_source')}")
            continue
        for model in clean["models"]:
            providers = owners.get(model)
            if not providers:
                unknown.append(model)
                continue
            for provider in providers:
                planned[(provider, model)] = clean

    print(f"строк к записи: {len(planned)}")
    if unknown:
        print(f"модели вне живого каталога ({len(unknown)}): {', '.join(sorted(set(unknown))[:12])}")

    by_tier: dict[str, int] = defaultdict(int)
    for value in planned.values():
        by_tier[value["tier"]] += 1
    print("по классам:", dict(by_tier))

    if args.dry_run:
        for (provider, model), value in list(planned.items())[:10]:
            print(
                f"  {provider}/{model}: tier={value['tier']} ctx={value['context']} "
                f"vision={value['vision']} tools={value['tools']} reasoning={value['reasoning']}"
            )
        print("черновик: ничего не записано (нужен --apply)")
        return 0

    backup = DB.with_suffix(".sqlite.bak-capabilities")
    backup.write_bytes(DB.read_bytes())
    print(f"бэкап: {backup}")

    connection = sqlite3.connect(DB)
    try:
        connection.execute("BEGIN")
        for (provider, model), value in planned.items():
            connection.execute(
                "INSERT INTO model_capabilities (provider, model_id, tool_call, reasoning, "
                "attachment, limit_context, family, status, last_synced) "
                "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', datetime('now')) "
                "ON CONFLICT(provider, model_id) DO UPDATE SET "
                "tool_call = excluded.tool_call, reasoning = excluded.reasoning, "
                "attachment = excluded.attachment, limit_context = excluded.limit_context, "
                "family = excluded.family, status = excluded.status, "
                "last_synced = excluded.last_synced",
                (
                    provider,
                    model,
                    1 if value["tools"] else 0,
                    1 if value["reasoning"] else 0,
                    1 if value["vision"] else 0,
                    value["context"],
                    value["family"],
                ),
            )
            connection.execute(
                "INSERT INTO model_capability_overrides (provider, model_id, override_key, override_value) "
                "VALUES (?1, ?2, 'tier', ?3) "
                "ON CONFLICT(provider, model_id, override_key) DO UPDATE SET "
                "override_value = excluded.override_value, refreshed_at = datetime('now')",
                (provider, model, value["tier"]),
            )
        connection.commit()
    finally:
        connection.close()
    print("записано")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
