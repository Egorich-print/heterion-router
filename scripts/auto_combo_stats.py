#!/usr/bin/env python3
"""Статистика живого прогона комбо `auto` по журналу шлюза.

Источник — `call_logs` в `~/.heterion-router/storage.sqlite`: каждая строка
один запрос к шлюзу с провайдером, моделью, статусом, длительностью и
токенами. Тела запросов и ответов скрипт не читает.

Скрипт ничего не интерпретирует «на глаз»: счётчики и квантили считаются по
строкам журнала, поэтому отчёт можно сверить с `sqlite3` напрямую.

    ./scripts/auto_combo_stats.py --minutes 30
    ./scripts/auto_combo_stats.py --minutes 10 --watch --interval 60
    ./scripts/auto_combo_stats.py --minutes 30 --out docs/research/auto-stats.md
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import statistics
import sys
import time
from collections import Counter, defaultdict
from datetime import datetime, timedelta, timezone

DB = os.path.expanduser("~/.heterion-router/storage.sqlite")

# Комбо «авто» живёт в шлюзе под этим именем; `requested_model` — то, что
# просил клиент (в MCode это `auto` или `heterion-router/auto`).
COMBO_NAMES = ("heterion-router/auto",)
REQUESTED_MODELS = ("auto", "heterion-router/auto")

# Коды, которые для провайдера означают «не дефект модели» (ADR-032/ADR-026):
# 429 — квота, 402 — нужен оплаченный баланс, 403 — доступ/тариф.
QUOTA_STATUSES = (402, 403, 429)


def _where(since: datetime | None, combo_only: bool) -> tuple[str, list]:
    clauses = ["1=1"]
    params: list = []
    if since is not None:
        clauses.append("timestamp >= ?")
        params.append(since.strftime("%Y-%m-%d %H:%M:%S"))
    if combo_only:
        marks = ",".join("?" for _ in COMBO_NAMES)
        clauses.append(f"(combo_name IN ({marks}) OR requested_model IN (?,?))")
        params.extend(COMBO_NAMES)
        params.extend(REQUESTED_MODELS)
    return " AND ".join(clauses), params


def _columns(connection: sqlite3.Connection) -> set[str]:
    """Колонки журнала: у части баз их ещё нет (старый бинарь шлюза)."""
    return {
        row[1]
        for row in connection.execute("PRAGMA table_info(call_logs)").fetchall()
    }


def fetch(since: datetime | None, combo_only: bool) -> list[sqlite3.Row]:
    if not os.path.exists(DB):
        raise SystemExit(f"журнал не найден: {DB}")
    where, params = _where(since, combo_only)
    connection = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        # Колонки исхода хода появились не сразу: у базы, которую писал старый
        # бинарь, их нет — тогда отчёт говорит, что данных нет, а не падает.
        available = _columns(connection)
        outcome = [c for c in ("finish_reason", "tool_called", "had_tools") if c in available]
        outcome_sql = ", " + ", ".join(outcome) if outcome else ""
        sql = (
            "SELECT timestamp, status, provider, model, requested_model, combo_name, "
            "combo_step_id, duration, tokens_in, tokens_out, error_summary, error_type, "
            "detail_state, api_key_name"
            f"{outcome_sql} "
            f"FROM call_logs WHERE {where} ORDER BY timestamp"
        )
        return connection.execute(sql, params).fetchall()
    finally:
        connection.close()


def percentile(values: list[float], share: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(round(share * (len(ordered) - 1)))))
    return float(ordered[index])


def build(rows: list[sqlite3.Row]) -> dict:
    total = len(rows)
    ok = [r for r in rows if (r["status"] or 0) < 400]
    failed = [r for r in rows if (r["status"] or 0) >= 400]
    # `duration` в журнале — миллисекунды; в отчёте — секунды.
    durations = [float(r["duration"] or 0) / 1000.0 for r in ok if (r["duration"] or 0) > 0]
    tokens_in = sum(int(r["tokens_in"] or 0) for r in rows)
    tokens_out = sum(int(r["tokens_out"] or 0) for r in rows)

    by_provider = Counter((r["provider"] or "?") for r in rows)
    by_model = Counter((r["model"] or "?") for r in rows)
    by_status = Counter(str(r["status"]) for r in rows)
    by_key = Counter((r["api_key_name"] or "?") for r in rows)
    by_step = Counter(str(r["combo_step_id"]) for r in rows if r["combo_step_id"] is not None)

    errors = Counter()
    error_by_provider = Counter()
    for row in failed:
        summary = (row["error_summary"] or row["error_type"] or "").strip()
        # Отрезаем идентификаторы запросов: уникальный хвост не несёт смысла
        # для статистики, но делает каждый счётчик уникальным.
        summary = summary.split("(")[0].strip()[:120] or f"HTTP {row['status']}"
        errors[summary] += 1
        error_by_provider[f"{row['provider'] or '?'} → {summary}"] += 1

    per_minute = Counter()
    for row in rows:
        stamp = (row["timestamp"] or "")[:16]
        per_minute[stamp] += 1

    # Исход агентского хода: клиент дал инструменты, модель либо вызвала
    # инструмент, либо закрыла ход текстом. Текстовый ход на агентском
    # запросе — это «прогон встал без причины»: именно его доля по моделям
    # решает, кого понижать в лестнице.
    turns: dict[str, dict[str, int]] = defaultdict(
        lambda: {"agent_turns": 0, "tool_called": 0, "text_only": 0, "cut": 0}
    )
    for row in rows:
        keys = row.keys()
        if "had_tools" not in keys or not row["had_tools"]:
            continue
        model = row["model"] or "?"
        entry = turns[model]
        entry["agent_turns"] += 1
        if "tool_called" in keys and row["tool_called"]:
            entry["tool_called"] += 1
        elif "finish_reason" in keys and row["finish_reason"] == "length":
            entry["cut"] += 1
        else:
            entry["text_only"] += 1
    text_only_share = {
        model: round(100.0 * entry["text_only"] / entry["agent_turns"], 1)
        for model, entry in turns.items()
        if entry["agent_turns"]
    }

    # Пиковый параллелизм: сколько вызовов старнули в пределами одной секунды
    # (грубая, но полезная мера «сколько агентов одновременно»).
    per_second = Counter((row["timestamp"] or "")[:19] for row in rows)
    peak_per_second = max(per_second.values()) if per_second else 0

    return {
        "total": total,
        "ok": len(ok),
        "failed": len(failed),
        "success_rate": round(100.0 * len(ok) / total, 1) if total else 0.0,
        "providers": len(by_provider),
        "models": len(by_model),
        "duration_s": {
            "median": round(statistics.median(durations), 2) if durations else 0.0,
            "p90": round(percentile(durations, 0.90), 2),
            "p95": round(percentile(durations, 0.95), 2),
            "max": round(max(durations), 2) if durations else 0.0,
            "mean": round(statistics.fmean(durations), 2) if durations else 0.0,
        },
        "tokens": {"in": tokens_in, "out": tokens_out},
        "by_provider": dict(by_provider.most_common()),
        "by_model": dict(by_model.most_common()),
        "by_status": dict(by_status.most_common()),
        "by_api_key": dict(by_key.most_common()),
        "by_combo_step": dict(by_step.most_common()),
        "errors": dict(errors.most_common()),
        "error_by_provider": dict(error_by_provider.most_common()),
        "per_minute": dict(sorted(per_minute.items())),
        "peak_calls_per_second": peak_per_second,
        "turn_outcome": {model: dict(entry) for model, entry in turns.items()},
        "text_only_share": text_only_share,
    }


def render(stats: dict, window_minutes: int) -> str:
    lines: list[str] = []
    add = lines.append
    add(f"# Статистика комбо `auto` — последние {window_minutes} мин")
    add("")
    add(f"Снято: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')} · источник `call_logs`.")
    add("")
    add("## Итог")
    add("")
    add("| метрика | значение |")
    add("| --- | --- |")
    add(f"| вызовов | {stats['total']} |")
    add(f"| успешных | {stats['ok']} |")
    add(f"| с ошибкой | {stats['failed']} |")
    add(f"| успешность | {stats['success_rate']}% |")
    add(f"| провайдеров | {stats['providers']} |")
    add(f"| моделей | {stats['models']} |")
    add(f"| пик запросов в секунду | {stats['peak_calls_per_second']} |")
    duration = stats["duration_s"]
    add(
        f"| длительность (медиана / p90 / p95 / макс), с | "
        f"{duration['median']} / {duration['p90']} / {duration['p95']} / {duration['max']} |"
    )
    add(f"| токены вход / выход | {stats['tokens']['in']} / {stats['tokens']['out']} |")
    add("")

    add("## Провайдеры")
    add("")
    add("| провайдер | вызовов |")
    add("| --- | --- |")
    for name, count in stats["by_provider"].items():
        add(f"| {name} | {count} |")
    add("")

    add("## Модели")
    add("")
    add("| модель | вызовов |")
    add("| --- | --- |")
    for name, count in stats["by_model"].items():
        add(f"| {name} | {count} |")
    add("")

    if stats["by_combo_step"]:
        add("## Шаги комбо")
        add("")
        add("| шаг | вызовов |")
        add("| --- | --- |")
        for name, count in stats["by_combo_step"].items():
            add(f"| {name} | {count} |")
        add("")

    add("## Ходы агента: вызов инструмента или текст")
    add("")
    if not stats["turn_outcome"]:
        add(
            "В журнале нет колонок исхода хода — сборка шлюза старше той, "
            "которая их пишет."
        )
    else:
        add(
            "Считаются только ходы, где клиент дал инструменты. "
            "«Текстом» — ход закрылся ответом без вызова инструмента."
        )
        add("")
        add("| модель | агентских ходов | с вызовом инструмента | текстом | доля текстовых, % |")
        add("| --- | --- | --- | --- | --- |")
        for model, entry in sorted(
            stats["turn_outcome"].items(), key=lambda item: -item[1]["agent_turns"]
        ):
            add(
                f"| {model} | {entry['agent_turns']} | {entry['tool_called']} | "
                f"{entry['text_only']} | {stats['text_only_share'].get(model, 0.0)} |"
            )
    add("")

    add("## Ошибки")
    add("")
    if not stats["errors"]:
        add("Ошибок нет.")
    else:
        add("| провайдер → причина | вызовов |")
        add("| --- | --- |")
        for name, count in stats["error_by_provider"].items():
            add(f"| {name} | {count} |")
    add("")

    add("## По минутам")
    add("")
    add("| минута | вызовов |")
    add("| --- | --- |")
    for stamp, count in stats["per_minute"].items():
        add(f"| {stamp} | {count} |")
    add("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description="Статистика комбо auto по журналу шлюза")
    parser.add_argument(
        "--minutes", type=int, default=30, help="окно в минутах (по умолчанию 30)"
    )
    parser.add_argument("--all", action="store_true", help="вся история, не только окно")
    parser.add_argument(
        "--all-calls",
        action="store_true",
        help="считать все вызовы шлюза, а не только комбо auto",
    )
    parser.add_argument("--watch", action="store_true", help="печатать снимок каждые N секунд")
    parser.add_argument("--interval", type=int, default=60, help="интервал --watch, секунд")
    parser.add_argument("--out", help="записать markdown-отчёт в файл")
    parser.add_argument("--json", help="записать JSON-отчёт в файл")
    args = parser.parse_args()

    def snapshot() -> dict:
        # Журнал пишет `timestamp` в UTC, а машина может быть в другом
        # поясе — окно считаем в UTC, иначе «последние N минут» окажутся
        # пустыми (или, наоборот, захватят чужие часы).
        since = (
            None
            if args.all
            else datetime.now(timezone.utc).replace(tzinfo=None) - timedelta(minutes=args.minutes)
        )
        rows = fetch(since, combo_only=not args.all_calls)
        return build(rows)

    if not args.watch:
        stats = snapshot()
        report = render(stats, args.minutes)
        print(report)
        if args.out:
            with open(args.out, "w", encoding="utf-8") as handle:
                handle.write(report)
        if args.json:
            with open(args.json, "w", encoding="utf-8") as handle:
                json.dump(stats, handle, ensure_ascii=False, indent=2)
        return

    while True:
        stats = snapshot()
        print(f"\n=== {datetime.now().strftime('%H:%M:%S')} ===")
        print(
            f"вызовов {stats['total']} · успех {stats['success_rate']}% · "
            f"провайдеров {stats['providers']} · ошибок {stats['failed']}"
        )
        for name, count in list(stats["by_provider"].items())[:8]:
            print(f"  {name}: {count}")
        if stats["errors"]:
            print("  ошибки:")
            for name, count in list(stats["error_by_provider"].items())[:5]:
                print(f"    {name}: {count}")
        sys.stdout.flush()
        time.sleep(args.interval)


if __name__ == "__main__":
    main()
