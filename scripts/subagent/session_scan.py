#!/usr/bin/env python3
"""Scan a Minimax Code session transcript for repetition.

`repeat_check.py` measures one answer. An agent session is many turns of the
same model, and the interesting failures live *between* turns:

  * the runtime continues a truncated answer and the model restarts it;
  * the same paragraph is emitted again in a later turn;
  * the model loops inside its thinking block while the answer stays clean.

This script reads `messages.jsonl` of a session directory and reports, per
session: the final answer, the concatenated assistant text, the thinking text,
cross-turn duplication (8-word windows shared between two turns), restarts (a
later turn opening with the same words as an earlier one), and the stop reasons.

Usage:
  ./scripts/subagent/session_scan.py ~/.minimax/v2/sessions/2026/10/26/<dir>
  ./scripts/subagent/session_scan.py --sessions-root ~/.minimax/v2/sessions --since 30
"""

import argparse
import base64
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from repeat_check import measure, words  # noqa: E402

NGRAM = 8

# A turn that opens like an earlier one is only a restart when the opening is
# long enough to be an answer, not a narration: "Let me check whether this
# work overlaps the ..." is how agents talk, not how they repeat.
RESTART_WORDS = 16

# Cross-turn overlap is measured on longer windows and needs a turn worth
# comparing, so a short narration does not count as a repeated answer.
CROSS_WINDOW = 12
CROSS_MIN_WORDS = 40
CROSS_RATIO = 0.30


def session_dir_for(session_id):
    """The session directory of a `mvs_...` id, if it is on disk."""
    encoded = base64.urlsafe_b64encode(session_id.encode()).decode().rstrip("=")
    for root in (Path.home() / ".minimax/v2/sessions").glob("*/*/*/*"):
        if root.is_dir() and encoded in root.name:
            return root
    return None


def load_messages(session_dir):
    path = Path(session_dir) / "messages.jsonl"
    if not path.exists():
        return []
    out = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def parts_of(message):
    """Split a message body into text, thinking and tool-call parts."""
    content = message.get("content")
    texts, thinking, tools = [], [], []
    if isinstance(content, str):
        texts.append(content)
    elif isinstance(content, list):
        for part in content:
            kind = part.get("type")
            if kind == "text":
                texts.append(part.get("text") or "")
            elif kind in ("thinking", "reasoning", "reasoning_content"):
                thinking.append(part.get("thinking") or part.get("text") or "")
            elif kind in ("toolCall", "tool_call"):
                tools.append(part.get("name") or "?")
    return texts, thinking, tools


def windows(text, size=NGRAM):
    tokens = words(text)
    return {
        tuple(tokens[i:i + size])
        for i in range(max(0, len(tokens) - size + 1))
    }


def overlap_ratio(first, second):
    """Share of the second turn's windows that already appeared in the first."""
    if len(words(second)) < CROSS_MIN_WORDS:
        return 0.0
    left, right = windows(first, CROSS_WINDOW), windows(second, CROSS_WINDOW)
    if not right:
        return 0.0
    return len(left & right) / len(right)


def restart_of(first, second):
    """True when a later turn opens exactly like an earlier one."""
    left, right = words(first), words(second)
    if len(left) < RESTART_WORDS or len(right) < RESTART_WORDS:
        return False
    return left[:RESTART_WORDS] == right[:RESTART_WORDS]


def scan(session_dir):
    messages = load_messages(session_dir)
    turns = []
    for entry in messages:
        message = entry.get("message") or {}
        if message.get("role") != "assistant":
            continue
        texts, thinking, tools = parts_of(message)
        turns.append(
            {
                "text": "\n".join(t for t in texts if t),
                "thinking": "\n".join(t for t in thinking if t),
                "tools": tools,
                "stopReason": message.get("stopReason"),
                "model": message.get("model"),
                "usage": message.get("usage") or {},
                "id": entry.get("message_id"),
            }
        )

    assistant_texts = [t["text"] for t in turns if t["text"]]
    thinking_texts = [t["thinking"] for t in turns if t["thinking"]]
    final = assistant_texts[-1] if assistant_texts else ""

    cross = []
    restarts = []
    for i, first in enumerate(assistant_texts):
        for j, second in enumerate(assistant_texts):
            if j <= i:
                continue
            ratio = overlap_ratio(first, second)
            if ratio >= CROSS_RATIO:
                cross.append({"turns": [i, j], "overlap": round(ratio, 3)})
            if restart_of(first, second):
                restarts.append({"turns": [i, j]})

    final_metrics = measure(final) if final else None
    all_metrics = measure("\n".join(assistant_texts)) if assistant_texts else None
    think_metrics = measure("\n".join(thinking_texts)) if thinking_texts else None

    stops = {}
    for turn in turns:
        stops[turn["stopReason"]] = stops.get(turn["stopReason"], 0) + 1

    inputs = [t["usage"].get("input") for t in turns if t["usage"].get("input")]
    outputs = [t["usage"].get("output") for t in turns if t["usage"].get("output")]

    verdicts = {m["verdict"] for m in (final_metrics, all_metrics, think_metrics) if m}
    if "REPEAT" in verdicts or cross or restarts:
        overall = "REPEAT"
    elif "SUSPECT" in verdicts:
        overall = "SUSPECT"
    else:
        overall = "OK"

    return {
        "session_dir": str(session_dir),
        "turns": len(turns),
        "models": sorted({t["model"] for t in turns if t["model"]}),
        "stop_reasons": stops,
        "input_tokens_first_last": [inputs[0], inputs[-1]] if inputs else [],
        "input_tokens_max": max(inputs) if inputs else 0,
        "output_tokens_total": sum(outputs),
        "final": final_metrics,
        "all_assistant_text": all_metrics,
        "thinking": think_metrics,
        "cross_turn_overlap": cross,
        "restarts": restarts,
        "verdict": overall,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("session_dir", nargs="?", help="session directory to scan")
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    parser.add_argument(
        "--sessions-root",
        default=str(Path.home() / ".minimax/v2/sessions"),
        help="root to search when no session_dir is given",
    )
    parser.add_argument("--since", type=int, default=0, help="only dirs newer than N minutes")
    args = parser.parse_args()

    targets = []
    if args.session_dir:
        targets = [Path(args.session_dir)]
    else:
        root = Path(args.sessions_root)
        cutoff = None
        if args.since:
            import time

            cutoff = time.time() - args.since * 60
        for path in root.glob("*/*/*/*"):
            if not path.is_dir() or not (path / "messages.jsonl").exists():
                continue
            if cutoff is not None and path.stat().st_mtime < cutoff:
                continue
            targets.append(path)

    reports = [scan(path) for path in targets]
    reports = [r for r in reports if r["turns"]]
    reports.sort(key=lambda r: r["turns"], reverse=True)

    if args.json:
        print(json.dumps(reports, ensure_ascii=False, indent=2))
        return 0

    for report in reports:
        print(
            f"{report['verdict']:8} {report['turns']:3} turns  "
            f"{','.join(report['models']) or '?':40} {Path(report['session_dir']).name}"
        )
        if report["final"]:
            f = report["final"]
            print(
                f"         final: ngram={f['ngram_repeat_ratio']} dup={f['dup_line_ratio']} "
                f"words={f['words']} longest_repeat={f['longest_repeat_words']}"
            )
        if report["thinking"]:
            t = report["thinking"]
            print(
                f"         think: ngram={t['ngram_repeat_ratio']} dup={t['dup_line_ratio']} "
                f"words={t['words']}"
            )
        if report["cross_turn_overlap"]:
            print(f"         cross-turn overlap: {report['cross_turn_overlap']}")
        if report["restarts"]:
            print(f"         restarts: {report['restarts']}")
        print(
            f"         stops: {report['stop_reasons']} "
            f"in_tokens {report['input_tokens_first_last']} max={report['input_tokens_max']}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
