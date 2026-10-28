#!/usr/bin/env python3
"""Does a model repeat itself when its answer is cut off and continued?

A client that truncates an answer at `max_tokens` and asks the model to
continue hands the model back its own partial answer. If the model restarts
instead of continuing, the visible transcript repeats the opening — which is
exactly what a user sees as "it repeats right after it starts".

The experiment: ask for a long answer with a small token cap, then send the
partial answer back with "continue", and measure how much of the second part
overlaps the first. A model that continues cleanly overlaps only at the
seam; a model that restarts overlaps from the very beginning.
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
if not API_KEY:
    raise SystemExit(
        "HETERION_API_KEY is unset: export the router's client key "
        "(see the router's API-key list) before running this script."
    )

TASK = (
    "Write a detailed technical explanation, at least 900 words, of how a "
    "Rust async runtime schedules tasks on a multi-threaded executor: the "
    "work-stealing queue, how a parked worker is woken, cooperative yielding, "
    "and how timers are kept. Use numbered points."
)
CONTINUE_PROMPT = "Continue exactly where you left off, from the next sentence."


def ask(model, messages, max_tokens, timeout=240):
    body = {
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
    }
    request = urllib.request.Request(
        f"{BASE_URL}/chat/completions",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as error:
        return "", f"HTTP {error.code}: {error.read().decode('utf-8', 'replace')[:200]}"
    except Exception as error:  # noqa: BLE001
        return "", f"{type(error).__name__}: {error}"
    choices = payload.get("choices") or []
    if not choices:
        return "", "no choices"
    return (choices[0].get("message") or {}).get("content") or "", None


def overlap(first, second, size=8):
    """Share of the second part's word windows that also occur in the first."""
    left = first.split()
    right = second.split()
    if len(right) < size:
        return 0.0, ""
    seen = set()
    for index in range(len(left) - size + 1):
        seen.add(tuple(left[index:index + size]))
    repeats = 0
    first_hit = None
    for index in range(len(right) - size + 1):
        window = tuple(right[index:index + size])
        if window in seen:
            repeats += 1
            if first_hit is None:
                first_hit = index
    return repeats / (len(right) - size + 1), first_hit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("models", nargs="+")
    parser.add_argument("--max-tokens", type=int, default=200)
    args = parser.parse_args()

    for model in args.models:
        first, error = ask(model, [{"role": "user", "content": TASK}], args.max_tokens)
        if error:
            print(f"{model}: first leg failed: {error}", flush=True)
            continue
        if not first.strip():
            print(f"{model}: first leg empty", flush=True)
            continue
        second, error = ask(
            model,
            [
                {"role": "user", "content": TASK},
                {"role": "assistant", "content": first},
                {"role": "user", "content": CONTINUE_PROMPT},
            ],
            args.max_tokens,
        )
        if error:
            print(f"{model}: second leg failed: {error}", flush=True)
            continue
        ratio, first_hit = overlap(first, second)
        verdict = "RESTARTS" if ratio >= 0.20 else "continues"
        print(
            f"{model}\n  leg1={len(first.split())}w leg2={len(second.split())}w "
            f"overlap={ratio:.3f} first_hit={first_hit} -> {verdict}",
            flush=True,
        )
        if ratio >= 0.20:
            print("  leg2 head:", second[:220].replace("\n", " "), flush=True)
            print("  leg1 head:", first[:220].replace("\n", " "), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
