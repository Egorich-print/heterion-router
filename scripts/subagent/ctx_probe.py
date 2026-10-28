#!/usr/bin/env python3
"""Measure the context window of an agentrouter model.

The platform publishes no context metadata: not `GET /v1/models`, not
`/api/pricing`, not `/api/status`. So the window can only be measured — a
prompt of a known size is sent to `/v1/responses` and the answer says whether
it fits. That matters because the client window is set from a config value
(`limit.context`), and a value that is too small costs the agent its history
long before the platform would refuse anything.

Usage:
  AGENTROUTER_API_KEY=... ./scripts/subagent/ctx_probe.py --model gpt-5.6-sol
  AGENTROUTER_API_KEY=... ./scripts/subagent/ctx_probe.py --model gpt-5.6-sol \\
      --targets 200000 500000 1000000 --max-output 32768

The key is a credential: it is read from the environment only and never
stored in the repository. It is checked when a call is actually made, not at
import time.
"""

import argparse
import json
import os
import random
import sys
import urllib.error
import urllib.request

BASE_URL = os.environ.get(
    "AGENTROUTER_BASE_URL", "https://agentrouter.org"
).rstrip("/")
API_KEY = os.environ.get("AGENTROUTER_API_KEY", "")

# Measured on live calls (2026-10-28): this filler vocabulary tokenises at
# 6.116 characters per prompt token. The probe converts a token target into
# characters and then reports the token count the platform actually counted,
# so the calibration never has to be trusted blindly.
CHARS_PER_TOKEN = 6.116

VOCAB = """time year people way day man thing woman life child world school state family
student group country problem hand part place case week company system program question
work government number night point home water room mother area money story fact month lot
right study book eye job word business issue side kind head house service friend father power
hour game line end member law car city community name president team minute idea kid body
information back parent face others level office door health person art war history party
result change morning reason research girl guy moment air teacher force education""".split()


def require_api_key() -> str:
    """The platform key, which comes from the environment only.

    Checked when a call is actually made, not at import time: other scripts
    import this module to reuse the filler and never talk to the platform.
    """
    if not API_KEY:
        sys.exit(
            "AGENTROUTER_API_KEY is not set: it is a credential and does not "
            "live in the repository."
        )
    return API_KEY


def make_prompt(target_tokens: int) -> str:
    """Filler text of roughly `target_tokens` prompt tokens."""
    chars = int(target_tokens * CHARS_PER_TOKEN)
    words = max(1, int(chars / 6.3))
    rnd = random.Random(20261028)
    body = " ".join(rnd.choice(VOCAB) for _ in range(words))
    return (
        "Read the following filler text and answer with the single word OK.\n\n"
        f"{body}\n\nAnswer with the single word OK."
    )


def probe(model: str, target_tokens: int, max_output: int) -> dict:
    """One call with a prompt of about `target_tokens` tokens."""
    text = make_prompt(target_tokens)
    body = {
        "model": model,
        "input": text,
        "max_output_tokens": max_output,
    }
    request = urllib.request.Request(
        f"{BASE_URL}/v1/responses",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {require_api_key()}",
            # The platform refuses an unrecognised client; see ADR-021.
            "originator": "codex_cli_rs",
            "user-agent": "heterion-router/0.1.0 (codex-responses client)",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=1800) as response:
            payload = json.loads(response.read().decode())
    except urllib.error.HTTPError as error:
        return {
            "target": target_tokens,
            "ok": False,
            "status": error.code,
            "error": error.read().decode("utf-8", "replace")[:500],
        }
    except Exception as error:  # noqa: BLE001
        return {
            "target": target_tokens,
            "ok": False,
            "status": None,
            "error": f"{type(error).__name__}: {error}",
        }

    usage = payload.get("usage") or {}
    return {
        "target": target_tokens,
        "ok": True,
        "status": 200,
        "prompt_tokens": usage.get("input_tokens") or usage.get("prompt_tokens"),
        "output_tokens": usage.get("output_tokens") or usage.get("completion_tokens"),
        "chars": len(text),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-sol")
    parser.add_argument(
        "--targets",
        type=int,
        nargs="+",
        default=[100_000, 200_000, 400_000, 700_000, 1_000_000],
        help="prompt-token targets to try, in order; the first failure stops the run",
    )
    parser.add_argument(
        "--max-output",
        type=int,
        default=64,
        help="max_output_tokens per call (use the real budget to test the total)",
    )
    args = parser.parse_args()

    for target in args.targets:
        result = probe(args.model, target, args.max_output)
        if result["ok"]:
            print(
                f"[{target}] ok  platform counted {result['prompt_tokens']} prompt tokens, "
                f"{result['output_tokens']} output tokens ({result['chars']} chars)",
                flush=True,
            )
        else:
            print(
                f"[{target}] failed with status {result['status']}\n    {result['error']}",
                flush=True,
            )
            break
    return 0


if __name__ == "__main__":
    sys.exit(main())
