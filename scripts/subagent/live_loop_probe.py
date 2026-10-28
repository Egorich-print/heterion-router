#!/usr/bin/env python3
"""Try to make a model degenerate, and watch the router's repetition guard.

The loops found on 2026-10-26 happened inside long agent turns, not in a
single question — but the trigger is ordinary: a model asked to enumerate
many similar items tends to stop enumerating and start repeating. This probe
sends such a request through the router and reports what came back:

  * `stop`   — the model answered normally;
  * `length` — the answer was cut: either the model hit the token cap, or
    the repetition guard cut the loop (the gateway log says which:
    `WARN repetition guard model=…`).

Usage:
  ./scripts/subagent/live_loop_probe.py
  ./scripts/subagent/live_loop_probe.py mistral/codestral-latest
"""

import json
import os
import sys
import time
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

# A prompt that asks for a long enumeration of similar names: the shape that
# made Codestral repeat itself on 2026-10-26.
PROMPT = (
    "Выпиши подряд, без нумерации и без остановки, полные названия проверок "
    "clippy: unused_variables, unused_mut, unused_imports, dead_code, "
    "needless_return, needless_borrow, len_without_is_empty, manual_map, "
    "single_char_pattern, needless_range_loop, to_string_in_format_args, "
    "useless_format, redundant_clone, map_clone, unwrap_used, expect_used, "
    "panic, todo, unimplemented, dbg_macro, print_stdout, print_stderr, "
    "slow_vector_initialization, vec_init_then_push, or_fun_call, "
    "needless_bool, needless_return, needless_borrow, manual_map, "
    "single_char_pattern, needless_range_loop, to_string_in_format_args, "
    "useless_format, redundant_clone, map_clone, unwrap_used, expect_used, "
    "panic, todo, unimplemented, dbg_macro, print_stdout, print_stderr, "
    "slow_vector_initialization, vec_init_then_push, or_fun_call, "
    "needless_bool, needless_return, needless_borrow, manual_map, "
    "single_char_pattern, needless_range_loop, to_string_in_format_args, "
    "useless_format, redundant_clone, map_clone, unwrap_used, expect_used, "
    "panic, todo, unimplemented, dbg_macro, print_stdout, print_stderr, "
    "slow_vector_initialization, vec_init_then_push, or_fun_call, "
    "needless_bool, needless_return, needless_borrow, manual_map, "
    "single_char_pattern, needless_range_loop, to_string_in_format_args, "
    "useless_format, redundant_clone, map_clone, unwrap_used, expect_used, "
    "panic, todo, unimplemented, dbg_macro, print_stdout, print_stderr, "
    "slow_vector_initialization, vec_init_then_push, or_fun_call, "
    "needless_bool, needless_return, needless_borrow, manual_map, "
    "single_char_pattern, needless_range_loop, to_string_in_format_args, "
    "useless_format, redundant_clone, map_clone, unwrap_used, expect_used, "
    "panic, todo, unimplemented, dbg_macro, print_stdout, print_stderr, "
    "slow_vector_initialization, vec_init_then_push, or_fun_call, "
    "needless_bool"
)

MODELS = [
    "mistral/codestral-latest",
    "ollama-cloud/gpt-oss:20b",
    "ollama-cloud/nemotron-3-ultra",
]


def repeat_ratio(text, size=8):
    words = text.split()
    if len(words) < size:
        return 0.0
    seen = set()
    repeats = 0
    for index in range(len(words) - size + 1):
        window = tuple(words[index:index + size])
        if window in seen:
            repeats += 1
        else:
            seen.add(window)
    return repeats / (len(words) - size + 1)


def ask(model):
    body = {
        "model": model,
        # The nonce keeps every run a fresh call: the router caches answers,
        # and a cached one would hide a degeneration that happens now.
        "messages": [
            {"role": "user", "content": f"{PROMPT}\n\n(run {int(time.time())})"}
        ],
        "max_tokens": 8000,
    }
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
        with urllib.request.urlopen(request, timeout=600) as response:
            payload = json.loads(response.read())
    except urllib.error.HTTPError as error:
        return None, f"HTTP {error.code}: {error.read().decode('utf-8', 'replace')[:120]}"
    except Exception as error:  # noqa: BLE001
        return None, f"{type(error).__name__}: {error}"
    choice = (payload.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    return (choice.get("finish_reason"), message.get("content") or ""), None


def main():
    models = sys.argv[1:] or MODELS
    # The prompt itself names the same lint list four times, so an answer that
    # faithfully reproduces it scores high on the repeat ratio without the
    # model having degenerated. The prompt's own score is the baseline: a
    # verdict only means a loop when the answer is far above it.
    baseline = repeat_ratio(PROMPT)
    print(f"prompt baseline: ngram={baseline:.3f}")
    for model in models:
        result, error = ask(model)
        if error:
            print(f"{model}: ERROR {error}")
            continue
        finish, text = result
        ratio = repeat_ratio(text)
        verdict = "LOOP" if ratio >= 0.5 and ratio > baseline + 0.2 else "ok"
        print(
            f"{model}: finish={finish} chars={len(text)} ngram={ratio:.3f} {verdict}"
        )
        if verdict == "LOOP":
            print(f"   head: {text[:160]!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
