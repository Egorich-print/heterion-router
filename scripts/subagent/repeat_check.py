#!/usr/bin/env python3
"""Detect repetition in model output — a tracker, not a guess.

A model that loops says the same thing again and again: the same line, the
same eight words, the same paragraph. This script measures that instead of
eyeballing it, so "it repeats" becomes a number with a threshold, and so the
same check can run over a model sweep, a single answer, or a file.

Modes:
  --model MODEL --task TEXT   one call through the router, then measured
  --stream                    the same call as a stream (a router-side loop
                               shows up only in streaming)
  --sweep                     every candidate model, one task each
  --file PATH                 measure text already on disk
  --stdin                     measure text on stdin

Usage:
  ./scripts/subagent/repeat_check.py --model ollama-cloud/beam --task "explain X"
  ./scripts/subagent/repeat_check.py --sweep --task "..."
  ./scripts/subagent/repeat_check.py --file answer.md
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

BASE_URL = os.environ.get("HETERION_BASE_URL", "http://127.0.0.1:20128/v1").rstrip("/")
# The router's client key is a credential: it is read from the environment
# only and never stored in the repository.
API_KEY = os.environ.get("HETERION_API_KEY", "")


def require_api_key() -> str:
    """The router's client key, which comes from the environment only.

    Checked when a call is actually made, not at import time: `session_scan.py`
    imports this module to reuse `measure()` and never talks to the router.
    """
    if not API_KEY:
        raise SystemExit(
            "HETERION_API_KEY is unset: export the router's client key "
            "(see the router's API-key list) before running this script."
        )
    return API_KEY

# A window of words wide enough that a genuine paraphrase never repeats.
NGRAM = 8
# A line repeated once in three is already a loop, not an echo.
DUP_LINE_RATIO = 0.30
# More than a fifth of the eight-word windows being repeats is a loop.
NGRAM_RATIO = 0.20
# Ten percent is worth a look but is not proof.
WARN_RATIO = 0.10

SWEEP_MODELS = [
    "ollama-cloud/beam",
    "ollama-cloud/gemma4:31b",
    "ollama-cloud/nemotron-3-ultra",
    "ollama-cloud/nemotron-3-super",
    "ollama-cloud/gpt-oss:120b",
    "ollama-cloud/gpt-oss:20b",
    "mistral/codestral-latest",
    "gemini/gemini-3.1-flash-lite",
    "gemini/gemini-3-flash-preview",
    "gemini/gemma-4-26b-a4b-it",
    "openrouter/grok-4.6",
    "groq/openai/gpt-oss-20b",
    "pollinations/openai-fast",
    "kilocode/openrouter/free",
]


def words(text):
    return text.split()


def ngram_repeat_ratio(text, size=NGRAM):
    """Share of word windows that have already been seen."""
    tokens = words(text)
    if len(tokens) < size:
        return 0.0
    seen = set()
    repeats = 0
    for index in range(len(tokens) - size + 1):
        window = tuple(tokens[index:index + size])
        if window in seen:
            repeats += 1
        else:
            seen.add(window)
    return repeats / (len(tokens) - size + 1)


def duplicate_line_ratio(text):
    """Share of non-empty lines that repeat an earlier line."""
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    if not lines:
        return 0.0
    seen = set()
    repeats = 0
    for line in lines:
        if line in seen:
            repeats += 1
        else:
            seen.add(line)
    return repeats / len(lines)


def longest_repeated_block(text, cap=40):
    """The longest word block that occurs twice, and the block itself."""
    tokens = words(text)
    upper = min(cap, len(tokens) // 2)
    for size in range(upper, 2, -1):
        seen = {}
        for index in range(len(tokens) - size + 1):
            window = tuple(tokens[index:index + size])
            if window in seen:
                return size, " ".join(window)
            seen[window] = index
    return 0, ""


def measure(text):
    """Every repetition metric for one answer."""
    ratio = ngram_repeat_ratio(text)
    lines = duplicate_line_ratio(text)
    size, block = longest_repeated_block(text)
    if lines >= DUP_LINE_RATIO or ratio >= NGRAM_RATIO:
        verdict = "REPEAT"
    elif ratio >= WARN_RATIO or lines >= WARN_RATIO:
        verdict = "SUSPECT"
    else:
        verdict = "OK"
    return {
        "chars": len(text),
        "words": len(words(text)),
        "ngram_repeat_ratio": round(ratio, 3),
        "dup_line_ratio": round(lines, 3),
        "longest_repeat_words": size,
        "longest_repeat_sample": block[:160],
        "verdict": verdict,
    }


def call(model, prompt, *, stream=False, max_tokens=1200, timeout=180):
    """One chat completion through the router; returns (text, reasoning, error).

    The reasoning text is measured separately: a model can loop in its
    thinking block while the visible answer stays clean, and the two failures
    look completely different to whoever reads the transcript.
    """
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
    }
    if stream:
        body["stream"] = True
    request = urllib.request.Request(
        f"{BASE_URL}/chat/completions",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {require_api_key()}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return "", "", f"HTTP {error.code}: {error.read().decode('utf-8', 'replace')[:200]}"
    except Exception as error:  # noqa: BLE001
        return "", "", f"{type(error).__name__}: {error}"
    if stream:
        content, reasoning = _text_from_sse(raw)
        return content, reasoning, None
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError:
        return raw, "", None
    choices = payload.get("choices") or []
    if not choices:
        return "", "", f"no choices: {raw[:200]}"
    message = choices[0].get("message") or {}
    reasoning = message.get("reasoning_content") or message.get("reasoning") or ""
    if not isinstance(reasoning, str):
        reasoning = json.dumps(reasoning, ensure_ascii=False)
    return (message.get("content") or ""), reasoning, None


def _text_from_sse(raw):
    """Collect the content and reasoning deltas of an SSE stream."""
    parts = []
    reasoning_parts = []
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        payload = line[len("data:"):].strip()
        if not payload or payload == "[DONE]":
            continue
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            continue
        for choice in event.get("choices") or []:
            delta = choice.get("delta") or {}
            content = delta.get("content")
            if content:
                parts.append(content)
            reasoning = delta.get("reasoning_content")
            if reasoning:
                reasoning_parts.append(reasoning)
    return "".join(parts), "".join(reasoning_parts)


def sweep(task, *, stream):
    """One task across every candidate model; measured, not eyeballed."""
    rows = []
    for model in SWEEP_MODELS:
        text, reasoning, error = call(model, task, stream=stream)
        if error:
            rows.append({"model": model, "error": error, "verdict": "ERROR"})
            print(f"  ERR   {model:34} {error[:80]}", flush=True)
            continue
        result = measure(text)
        result["model"] = model
        if reasoning.strip():
            reason_result = measure(reasoning)
            result["reasoning"] = {
                "verdict": reason_result["verdict"],
                "ngram_repeat_ratio": reason_result["ngram_repeat_ratio"],
                "dup_line_ratio": reason_result["dup_line_ratio"],
                "longest_repeat_words": reason_result["longest_repeat_words"],
                "longest_repeat_sample": reason_result["longest_repeat_sample"],
                "words": reason_result["words"],
            }
        rows.append(result)
        verdict = result["verdict"]
        reason = result.get("reasoning")
        if reason and reason["verdict"] != "OK":
            verdict = f"{verdict}+R"
        print(
            f"  {verdict:<8} {model:34} "
            f"ngram={result['ngram_repeat_ratio']:<6} "
            f"lines={result['dup_line_ratio']:<6} "
            f"repeat={result['longest_repeat_words']}w "
            f"words={result['words']}"
            + (f"  reasoning={reason['verdict']}"
               f"(ngram={reason['ngram_repeat_ratio']})" if reason else ""),
            flush=True,
        )
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", help="one model to call through the router")
    parser.add_argument("--task", default="", help="the prompt to send")
    parser.add_argument("--stream", action="store_true",
                        help="stream the answer (a router-side loop shows here)")
    parser.add_argument("--sweep", action="store_true",
                        help="call every candidate model with the same task")
    parser.add_argument("--file", help="measure a text file")
    parser.add_argument("--stdin", action="store_true", help="measure stdin")
    parser.add_argument("--max-tokens", type=int, default=1200)
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args()

    if args.file:
        text = open(args.file, encoding="utf-8", errors="replace").read()
        result = measure(text)
        result["source"] = args.file
        print(json.dumps(result, ensure_ascii=False, indent=2) if args.json
              else _format(result))
        return 0
    if args.stdin:
        result = measure(sys.stdin.read())
        result["source"] = "stdin"
        print(json.dumps(result, ensure_ascii=False, indent=2) if args.json
              else _format(result))
        return 0
    if args.sweep:
        if not args.task:
            parser.error("--sweep needs --task")
        rows = sweep(args.task, stream=args.stream)
        if args.json:
            print(json.dumps(rows, ensure_ascii=False, indent=2))
        repeats = [r for r in rows if r.get("verdict") == "REPEAT"]
        print(f"\n{len(repeats)} of {len(rows)} models repeat")
        return 0
    if not args.model:
        parser.error("--model, --file, --stdin or --sweep is required")
    if not args.task:
        parser.error("--task is required with --model")
    text, reasoning, error = call(args.model, args.task, stream=args.stream,
                                  max_tokens=args.max_tokens)
    if error:
        print(f"call failed: {error}", file=sys.stderr)
        return 1
    result = measure(text)
    result["model"] = args.model
    result["stream"] = args.stream
    if reasoning.strip():
        result["reasoning"] = measure(reasoning)
    print(json.dumps(result, ensure_ascii=False, indent=2) if args.json
          else _format(result))
    return 0


def _format(result):
    lines = [
        f"verdict={result['verdict']} words={result['words']} "
        f"ngram_repeat={result['ngram_repeat_ratio']} "
        f"dup_lines={result['dup_line_ratio']} "
        f"longest_repeat={result['longest_repeat_words']}w",
        f"sample: {result['longest_repeat_sample'][:200]}",
    ]
    reason = result.get("reasoning")
    if reason:
        lines.append(
            f"reasoning: verdict={reason['verdict']} words={reason['words']} "
            f"ngram_repeat={reason['ngram_repeat_ratio']} "
            f"dup_lines={reason['dup_line_ratio']} "
            f"longest_repeat={reason['longest_repeat_words']}w"
        )
        lines.append(f"reasoning sample: {reason['longest_repeat_sample'][:200]}")
    return "\n".join(lines)


if __name__ == "__main__":
    sys.exit(main())
