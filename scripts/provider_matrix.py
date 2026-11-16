#!/usr/bin/env python3
"""Матрица провайдеров: кто обслуживается, кто ждёт ключа, кто мёртв.

Реестр (`crates/heterion-router-providers/registry.json`) говорит, как
площадка *выглядит*; живой ответ говорит, что она *есть*. Скрипт задаёт
каждой записи реестра один вопрос — «отвечаешь ли ты без ключа?» — и
раскладывает ответы по причинам:

* `served`            — есть живой бэкенд;
* `needs_key`         — `401`/`403`: нужен ключ;
* `credits_required`  — `402`: нужны кредиты;
* `dead_endpoint`     — `404`/`410`, площадка закрыта;
* `not_a_chat_api`    — хост жив, но чат-маршрута нет;
* `unsupported_format`— формат реестра шлюз не говорит;
* `no_web_adapter`    — у веб-исполнителя нет адаптера у драйвера;
* `no_connection_row` — keyless без строки подключения;
* `not_served`        — подключение есть, бэкенд не поднялся.

Пробу и классификацию делает сам шлюз (`POST /api/providers/{id}/probe`),
поэтому отчёт не может разойтись с тем, что видит админ-API.

Использование:
    scripts/provider_matrix.py --out docs/research/provider-matrix-2026-11-16.md
    scripts/provider_matrix.py --json /tmp/matrix.json --no-probe
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REGISTRY = ROOT / "crates/heterion-router-providers/registry.json"
KEY_FILE = Path.home() / ".heterion-router/desktop/operator-key.json"
ROUTER = "http://127.0.0.1:20128"

# Форматы, у которых есть своя точка входа для пробы.
PROBEABLE = {"openai", "openai-responses", "claude", "gemini"}

# Порядок причин в отчёте: от «работает» к «нужен код».
REASON_ORDER = (
    "served",
    "needs_key",
    "credits_required",
    "no_connection_row",
    "not_served",
    "no_web_adapter",
    "unsupported_format",
    "not_a_chat_api",
    "dead_endpoint",
    "unknown",
)


def operator_key() -> str:
    return KEY_FILE.read_text().strip()


def api(path: str, method: str = "GET") -> tuple[int, str]:
    request = urllib.request.Request(
        f"{ROUTER}{path}",
        method=method,
        headers={"Authorization": f"Bearer {operator_key()}"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", "replace")
    except urllib.error.URLError as error:
        return 0, str(error)


def registry_entries() -> list[dict]:
    data = json.loads(REGISTRY.read_text(encoding="utf-8"))
    entries = data["providers"] if isinstance(data, dict) and "providers" in data else data
    if isinstance(entries, dict):
        entries = list(entries.values())
    return entries


def probeable(entry: dict) -> bool:
    """Whether the gateway has an endpoint it can ask a question at."""
    base_url = entry.get("base_url") or ""
    if not base_url.startswith(("http://", "https://")):
        return False
    return (entry.get("format") or "openai") in PROBEABLE


def probe(entry: dict) -> dict | None:
    """Ask the gateway to measure one provider."""
    status, body = api(f"/api/providers/{entry['id']}/probe", method="POST")
    if status != 200:
        return None
    try:
        return json.loads(body)
    except json.JSONDecodeError:
        return None


def live_catalogue() -> set[str]:
    """The models the router serves right now, as `provider/model`."""
    status, body = api("/v1/models")
    if status != 200:
        return set()
    try:
        payload = json.loads(body)
    except json.JSONDecodeError:
        return set()
    return {entry.get("id", "") for entry in payload.get("data", [])}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="", help="куда писать markdown-отчёт")
    parser.add_argument("--json", default="", help="куда писать машинный отчёт")
    parser.add_argument(
        "--no-probe",
        action="store_true",
        help="не переопрашивать площадки, показать сохранённые пробы",
    )
    parser.add_argument("--workers", type=int, default=8)
    args = parser.parse_args()

    entries = registry_entries()
    if not args.no_probe:
        candidates = [entry for entry in entries if probeable(entry)]
        print(f"probing {len(candidates)} providers…", file=sys.stderr)
        with ThreadPoolExecutor(max_workers=args.workers) as pool:
            results = list(pool.map(probe, candidates))
        measured = sum(1 for result in results if result)
        print(f"probed {measured}/{len(candidates)}", file=sys.stderr)

    status, body = api("/api/providers")
    if status != 200:
        print(f"the gateway does not answer /api/providers: {status}", file=sys.stderr)
        return 1
    matrix = json.loads(body).get("matrix", [])
    catalogue = live_catalogue()

    counts = Counter(entry["reason"] for entry in matrix)
    served_models = sum(
        len([m for m in catalogue if m.startswith(f"{entry['id']}/")]) for entry in matrix
    )
    registry_models = sum(entry["models"] for entry in matrix)

    report = {
        "registry_providers": len(entries),
        "matrix_entries": len(matrix),
        "live_catalogue_models": len(catalogue),
        "served_models": served_models,
        "registry_models": registry_models,
        "by_reason": {reason: counts.get(reason, 0) for reason in REASON_ORDER},
        "providers": sorted(matrix, key=lambda entry: (REASON_ORDER.index(entry["reason"]), entry["id"])),
    }

    if args.json:
        Path(args.json).write_text(
            json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        print(f"json: {args.json}", file=sys.stderr)

    if args.out:
        lines = [
            f"# Матрица провайдеров — {len(entries)} записей реестра",
            "",
            "Каждая причина — измеренный ответ площадки, а не заявление реестра.",
            "",
            "| причина | провайдеров |",
            "|---|---|",
        ]
        for reason in REASON_ORDER:
            lines.append(f"| `{reason}` | {counts.get(reason, 0)} |")
        lines += [
            "",
            f"Живой каталог: **{len(catalogue)}** моделей; в реестре: **{registry_models}**.",
            "",
            "| провайдер | причина | моделей | формат | обслуживается |",
            "|---|---|---|---|---|",
        ]
        for entry in report["providers"]:
            lines.append(
                f"| `{entry['id']}` | `{entry['reason']}` | {entry['models']} | "
                f"{entry['format'] or '—'} | {'да' if entry['served'] else 'нет'} |"
            )
        Path(args.out).write_text("\n".join(lines) + "\n", encoding="utf-8")
        print(f"report: {args.out}", file=sys.stderr)

    print(json.dumps(report["by_reason"], ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
