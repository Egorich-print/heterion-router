#!/usr/bin/env python3
"""Auto-select an external model for a task and call it through Heterion Router.

Two independent axes:

* **Capability** — what the task NEEDS (`vision`, `code`, `longctx`, ...).
  A capability chain only contains models that were MEASURED to satisfy it.
* **Task class** — what KIND of work it is (`deep`, `code`, `quick`, ...).
  A class chain encodes the reasoning effort and the cost/latency trade-off.

Both chains are walked in order until one call succeeds, so a task never dies
because one provider is rate-limited, out of plan, or temporarily broken.

The router also implements per-model failover (combos: `auto-subagent`,
`auto-fast`, `auto-free`), so the first candidate of many chains is itself a
chain: one call already tries several upstreams.

Every candidate carries the evidence that put it in the chain. The chains are
built from live probes, never from a catalogue — an earlier chain built from the
registry listed `glm-5.3` and `kimi-k3` as fallbacks, and both answer 402 on
the current plan, so the chain would have degraded on its first step.

Usage:
    auto_call.py --task "..." [--class deep | --capability vision] [--max-tokens N]
    auto_call.py --task "..." --image /path/to.png      # vision is required
    auto_call.py --list-classes
    auto_call.py --list-capabilities
    auto_call.py --probe            # check which candidates are alive right now

Env:
    HETERION_BASE_URL  router base URL   (default http://127.0.0.1:20128/v1)
    HETERION_API_KEY   router bearer key (default: the local OpenCode-Main key)
"""

from __future__ import annotations

import argparse
import base64
import json
import mimetypes
import os
import sys
import time
import urllib.error
import urllib.request
import uuid

BASE_URL = os.environ.get("HETERION_BASE_URL", "http://127.0.0.1:20128/v1").rstrip("/")
# The router's client key is a credential: it is read from the environment
# only and never stored in the repository.
API_KEY = os.environ.get("HETERION_API_KEY", "")
if not API_KEY:
    raise SystemExit(
        "HETERION_API_KEY is unset: export the router's client key "
        "(see the router's API-key list) before running this script."
    )

# Per-model output cap, applied on top of the caller's budget.
#
# Codestral degenerated in 3 of 7 sessions on 2026-10-26: it printed one lint
# name 4090 times and ran to its 32768-token ceiling. The router's repetition
# guard cuts the loop, but in a non-streaming call the upstream has already
# billed those tokens by then — so the damage is bounded here instead.
OUTPUT_CAP: dict[str, int] = {
    "mistral/codestral-latest": 8192,
}

# --------------------------------------------------------------------------
# Capability chains: what the task NEEDS.
#
# Each candidate is (router_model_id, reasoning_effort, evidence).
# --------------------------------------------------------------------------
CAPABILITIES: dict[str, dict] = {
    "vision": {
        "summary": "must READ an image, screenshot, diagram or photo",
        "candidates": [
            ("ollama-cloud/gemma4:31b", "medium",
             "measured: read the test image and named the red circle, blue square, "
             "green triangle and the texts IN / VISION-OK-42 / HETERION SWARM"),
            ("gemini/gemini-3.1-flash-lite", "medium",
             "measured 2026-10-26 after the router stopped dropping images: same "
             "test image, all three shapes with their positions and all three "
             "texts quoted; 2.6s; the free tier gives it 500 requests a day"),
            ("gemini/gemini-3.5-flash-lite", "medium",
             "measured 2026-10-26: same image, shapes and texts correct; 23s"),
            ("gemini/gemini-3-flash-preview", "medium",
             "measured 2026-10-26: same image, shapes and positions correct; 4.9s; "
             "1M context window"),
            ("gemini/gemini-3.6-flash", "medium",
             "measured 2026-10-26: same image, 4.8s, 1M context window"),
            ("gemini/gemma-4-26b-a4b-it", "medium",
             "measured 2026-10-26: reads the image (three shapes in a row), but "
             "101s per call, so it is a fallback rather than a first choice"),
            ("auto-subagent", "medium",
             "text-only fallback chain: the task degrades to a description, not a "
             "blind description of a picture"),
        ],
    },
    "code": {
        "summary": "produce or change code",
        "candidates": [
            ("mistral/codestral-latest", None,
             "the code-specialised model on the router; measured alive, and it is "
             "what the Mistral provider is for; its output is capped at 8192 "
             "tokens, because it degenerated in 3 of 7 sessions on 2026-10-26"),
            ("ollama-cloud/beam", "high",
             "1M context, so a whole repository fits in one request"),
            ("auto-subagent", "high",
             "priority chain with failover across the free-tier models"),
        ],
    },
    "longctx": {
        "summary": "needs a whole repository or a long transcript in context",
        "candidates": [
            ("ollama-cloud/beam", "max",
             "measured: 634,392 prompt tokens in a single request, answered "
             "correctly in 17s"),
            ("auto-subagent", "max",
             "1M declared context; falls over to the 262k models if beam stops"),
            ("gemini/gemini-3.1-flash-lite", "medium",
             "measured 2026-10-26: 1M context, 2.6s per call and a free-tier "
             "quota of 500 requests a day"),
            ("gemini/gemini-3-flash-preview", "medium",
             "measured 2026-10-26: 1M context and it reads images too, so a "
             "picture and a repository fit in one call"),
            ("auto-free", "medium",
             "keyless public gateways; smallest context but never empty"),
        ],
    },
    "reasoning": {
        "summary": "the answer must be reasoned, not looked up",
        "candidates": [
            ("ollama-cloud/beam", "max",
             "reasoning levels up to max; Ollama's own default for beam is max"),
            ("auto-subagent", "max",
             "same chain, with failover"),
            ("ollama-cloud/nemotron-3-ultra", "medium",
             "reasoning model, 262k context"),
            ("auto-free", "medium",
             "last resort: correctness over depth"),
        ],
    },
    "fast": {
        "summary": "cheap and quick; the answer is small",
        "candidates": [
            ("auto-fast", "medium",
             "priority chain: nemotron-3-super, then gemini-3.1-flash-lite, then "
             "the cheap public gateways"),
            ("ollama-cloud/nemotron-3-super", "medium",
             "measured ~1s per call"),
            ("gemini/gemini-3.1-flash-lite", "medium",
             "measured 2026-10-26: 2.6s per call, 1M context, and the free tier "
             "allows 500 calls a day — the quota headroom the 3 Flash family "
             "does not have"),
            ("auto-free", "medium",
             "keyless, so it survives a credential problem"),
        ],
    },
    "search": {
        "summary": "the answer must come from the web, with sources",
        "candidates": [
            ("exa/exa-search", None,
             "the router's Exa search surface (ADR-016/019): the last user message "
             "is the query, the answer comes back as a cited block, and the API "
             "keys live in the router — not here; search options ride in the "
             "request under the \"exa\" key"),
        ],
    },
    "beam-replacement": {
        "summary": "what to use when beam stops answering",
        "candidates": [
            ("ollama-cloud/nemotron-3-ultra", "medium",
             "262k context, reasoning model, on the same free plan as beam"),
            ("ollama-cloud/gemma4:31b", "medium",
             "262k, reasoning model AND it reads images — the closest all-round "
             "replacement for beam"),
            ("ollama-cloud/nemotron-3-super", "medium", "262k, reasoning model"),
            ("ollama-cloud/gpt-oss:120b", "medium", "131k, reasoning model"),
            ("gemini/gemini-3.1-flash-lite", "medium",
             "measured 2026-10-26: 1M context, reads images, 2.6s per call and "
             "500 calls a day on the free tier"),
            ("gemini/gemini-3.6-flash", "medium",
             "measured 2026-10-26: 1M context, reads images, 4.8s per call; the "
             "5 RPM tier of the flash family, which also gets 503-overloaded, so "
             "it sits behind the flash-lite entry"),
            ("auto-free", "medium", "keyless public gateways, last resort"),
        ],
    },
}

# --------------------------------------------------------------------------
# Task-class chains: what KIND of work it is.
# --------------------------------------------------------------------------
TASK_CLASSES: dict[str, dict] = {
    "deep": {
        "summary": "architecture, design, hard debugging, multi-file reasoning",
        "candidates": [
            ("ollama-cloud/beam", "max", "1M context, reasoning max"),
            ("auto-subagent", "max", "same chain with failover"),
            ("ollama-cloud/nemotron-3-ultra", "medium", "262k reasoning model"),
            ("auto-free", "medium", "never empty"),
        ],
    },
    "code": {
        "summary": "bounded implementation change inside an existing codebase",
        "candidates": [
            ("mistral/codestral-latest", None,
             "code specialist; output capped at 8192 tokens — it degenerated in "
             "3 of 7 sessions on 2026-10-26"),
            ("ollama-cloud/beam", "high", "1M context for the surrounding code"),
            ("auto-subagent", "high", "priority chain with failover"),
            ("auto-fast", "medium", "cheap fallback"),
            ("auto-free", "medium", "last resort"),
        ],
    },
    "longctx": {
        "summary": "needs a whole repository or a long transcript in context",
        "candidates": [
            ("ollama-cloud/beam", "max", "measured at 634k prompt tokens"),
            ("auto-subagent", "max", "1M declared, with failover"),
            ("ollama-cloud/gemma4:31b", "medium", "262k, also reads images"),
            ("auto-free", "medium", "last resort"),
        ],
    },
    "search": {
        "summary": "find current facts on the web; the answer must be sourced",
        "candidates": [
            ("exa/exa-search", None,
             "search surface, not a chat model: the query is the task itself and "
             "the answer is a cited block"),
        ],
    },
    "quick": {
        "summary": "small lookup, a fact, a one-file read",
        "candidates": [
            ("auto-fast", "medium", "cheap chain, ~1s"),
            ("ollama-cloud/nemotron-3-super", "medium", "measured ~1s"),
            ("auto-free", "medium", "keyless fallback"),
        ],
    },
    "bulk": {
        "summary": "mechanical, repetitive, many similar items",
        "candidates": [
            ("auto-fast", "low", "cheapest chain"),
            ("ollama-cloud/gpt-oss:20b", "low", "small model"),
            ("auto-free", "low", "last resort"),
        ],
    },
    "verify": {
        "summary": "independent check of a deliverable against criteria",
        "candidates": [
            ("ollama-cloud/nemotron-3-ultra", "medium", "reasoning model, not the author"),
            ("ollama-cloud/beam", "high", "1M context for the evidence"),
            ("auto-free", "medium", "last resort"),
        ],
    },
}

# Keyword -> class. First match wins, so the order below is the priority order.
CLASS_KEYWORDS: list[tuple[str, tuple[str, ...]]] = [
    # Web intent wins before the repo-lookup class: "find the latest release"
    # wants the web, "find the file" wants the repo.
    ("search", ("search the web", "search online", "web search", "look up online",
                "google ", "wikipedia", "latest release", "current version",
                "news about", "official announcement", "on the web",
                "released on", "published on")),
    ("longctx", ("whole repo", "entire repo", "whole codebase", "full codebase",
                 "across the repo", "all files", "sweep the", "migrate the repo",
                 "long transcript", "full history", "every file")),
    ("deep", ("architect", "design", "why does", "root cause", "debug", "race condition",
              "deadlock", "trade-off", "tradeoff", "plan a", "strategy", "prove",
              "invariant", "concurrency", "memory leak", "refactor the whole",
              "step by step", "reason carefully", "prove it")),
    ("verify", ("verify", "review", "audit", "check whether", "validate", "critique",
                "adversarial", "does it really", "sanity check")),
    ("bulk", ("rename", "rename all", "for each", "each file", "batch", "bulk",
              "every occurrence", "scaffold", "list of", "all the files")),
    ("code", ("implement", "add a", "write the", "fix the", "refactor", "migrate",
              "port ", "patch", "extend", "integrate", "wire up")),
    ("quick", ("what is", "which", "find", "locate", "look up", "grep", "show me",
               "read the", "path to", "name of")),
]


# Phrases that mean "the answer is in this repository", not on the web.
REPO_MARKERS = ("in the repo", "in this repo", "in our code", "in the codebase",
                "in cargo.toml", "in the file", "in the source", "in the code",
                "in the workspace", "in the project", "in this project")


def classify(task: str) -> str:
    """Classify a task into one of TASK_CLASSES. Default: `deep`."""
    lowered = task.lower()
    repo_local = any(marker in lowered for marker in REPO_MARKERS)
    for name, keywords in CLASS_KEYWORDS:
        if name == "search" and repo_local:
            # "the current version in cargo.toml" is a repository question, not
            # a web question: the search class must not swallow it.
            continue
        if any(keyword in lowered for keyword in keywords):
            return name
    return "deep"


def image_part(path: str) -> dict:
    """An OpenAI vision content part holding the image at `path`."""
    mime = mimetypes.guess_type(path)[0] or "image/png"
    with open(path, "rb") as handle:
        encoded = base64.b64encode(handle.read()).decode()
    return {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{encoded}"}}


def call(model: str, prompt: str, *, system: str | None, max_tokens: int,
         effort: str | None, stream: bool, timeout: int,
         image: str | None = None) -> dict:
    """One chat-completions call. Returns a result dict; never raises."""
    if image:
        content: object = [{"type": "text", "text": prompt}, image_part(image)]
        user_message = {"role": "user", "content": content}
    else:
        user_message = {"role": "user", "content": prompt}
    body: dict = {
        "model": model,
        "messages": ([{"role": "system", "content": system}] if system else [])
        + [user_message],
        "max_tokens": max_tokens,
    }
    if effort:
        body["reasoning_effort"] = effort
    if stream:
        body["stream"] = True

    request = urllib.request.Request(
        f"{BASE_URL}/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {API_KEY}"},
        method="POST",
    )
    started = time.time()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode()
    except urllib.error.HTTPError as error:
        body_text = error.read().decode(errors="replace")[:300]
        return {"ok": False, "model": model, "status": error.code,
                "error": body_text, "elapsed_s": round(time.time() - started, 2)}
    except Exception as error:  # noqa: BLE001 - a network failure is a candidate skip
        return {"ok": False, "model": model, "status": 0,
                "error": f"{type(error).__name__}: {error}",
                "elapsed_s": round(time.time() - started, 2)}

    try:
        payload = json.loads(raw)
    except json.JSONDecodeError:
        return {"ok": False, "model": model, "status": 0,
                "error": f"non-JSON response: {raw[:200]}",
                "elapsed_s": round(time.time() - started, 2)}

    if "error" in payload:
        return {"ok": False, "model": model,
                "status": payload["error"].get("status", 0),
                "error": json.dumps(payload["error"])[:300],
                "elapsed_s": round(time.time() - started, 2)}

    choice = payload["choices"][0]
    usage = payload.get("usage", {}) or {}
    return {
        "ok": True,
        "model": model,
        "served_model": payload.get("model"),
        "content": (choice.get("message", {}) or {}).get("content", "") or "",
        "finish_reason": choice.get("finish_reason"),
        "tool_calls": (choice.get("message", {}) or {}).get("tool_calls"),
        "usage": usage,
        "elapsed_s": round(time.time() - started, 2),
    }


def run(chain: list, task: str, *, label: str, system: str | None, max_tokens: int,
        stream: bool, timeout: int, image: str | None = None) -> dict:
    """Walk a candidate chain until one call succeeds."""
    attempts: list[dict] = []
    for entry in chain:
        model, effort = entry[0], entry[1]
        tokens = min(max_tokens, OUTPUT_CAP.get(model, max_tokens))
        result = call(model, task, system=system, max_tokens=tokens,
                      effort=effort, stream=stream, timeout=timeout, image=image)
        attempts.append({"model": model, "effort": effort, "ok": result["ok"],
                         "status": result.get("status"),
                         "max_tokens": tokens,
                         "elapsed_s": result.get("elapsed_s"),
                         "error": result.get("error")})
        if result["ok"]:
            result["attempts"] = attempts
            result["selection"] = label
            result["effort"] = effort
            return result
    return {"ok": False, "selection": label, "attempts": attempts,
            "error": "every candidate failed"}


def probe() -> int:
    """Check which candidates of every chain answer right now."""
    seen: list[str] = []
    for spec in list(CAPABILITIES.values()) + list(TASK_CLASSES.values()):
        for entry in spec["candidates"]:
            if entry[0] not in seen:
                seen.append(entry[0])
    # A nonce keeps this out of the router's response cache: a cached hit
    # would answer in ~0 ms and prove nothing about the upstream.
    nonce = uuid.uuid4().hex[:8]
    failures = 0
    for model in seen:
        result = call(model, f"Reply with exactly: PROBE_OK_{nonce}", system=None,
                      max_tokens=2000, effort=None, stream=False, timeout=90)
        if result["ok"]:
            print(f"  OK    {model:34} {result['elapsed_s']:>6}s "
                  f"served={result.get('served_model')}")
        else:
            failures += 1
            print(f"  FAIL  {model:34} status={result.get('status')} "
                  f"{str(result.get('error'))[:70]}")
        # The free tier counts per account per minute, and the router rotates
        # five Google accounts for Gemini: a burst of probes burns through the
        # minute budget of all of them at once and every model starts
        # answering 429. Pacing the probe keeps the measurement about the
        # models rather than about the quota.
        time.sleep(4)
    return 1 if failures else 0


def print_chain(title: str, registry: dict) -> None:
    for name, spec in registry.items():
        print(f"{name:18} {spec['summary']}")
        for model, effort, evidence in spec["candidates"]:
            print(f"{'':18} -> {model}[{effort}]")
            print(f"{'':20} {evidence}")
        print()


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--task", help="what the model has to do")
    parser.add_argument("--class", dest="task_class", choices=sorted(TASK_CLASSES),
                        help="force a task class instead of classifying")
    parser.add_argument("--capability", choices=sorted(CAPABILITIES),
                        help="select by required capability instead of task class")
    parser.add_argument("--image", help="image path; implies --capability vision")
    parser.add_argument("--system", help="system prompt")
    parser.add_argument("--max-tokens", type=int, default=16000)
    parser.add_argument("--stream", action="store_true")
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    parser.add_argument("--list-classes", action="store_true")
    parser.add_argument("--list-capabilities", action="store_true")
    parser.add_argument("--probe", action="store_true",
                        help="check which candidate models are alive")
    args = parser.parse_args()

    if args.list_classes:
        print_chain("TASK CLASSES", TASK_CLASSES)
        return 0
    if args.list_capabilities:
        print_chain("CAPABILITIES", CAPABILITIES)
        return 0
    if args.probe:
        return probe()
    if not args.task:
        parser.error("--task is required")

    if args.image:
        capability = args.capability or "vision"
        if capability not in CAPABILITIES:
            parser.error(f"unknown capability {capability}")
        chain = CAPABILITIES[capability]["candidates"]
        label = f"capability:{capability}"
    elif args.capability:
        chain = CAPABILITIES[args.capability]["candidates"]
        label = f"capability:{args.capability}"
    else:
        task_class = args.task_class or classify(args.task)
        chain = TASK_CLASSES[task_class]["candidates"]
        label = f"class:{task_class}"

    result = run(chain, args.task, label=label, system=args.system,
                 max_tokens=args.max_tokens, stream=args.stream,
                 timeout=args.timeout, image=args.image)

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if result.get("ok") else 1

    if not result.get("ok"):
        print(f"FAILED {label}", file=sys.stderr)
        for attempt in result.get("attempts", []):
            print(f"  {attempt['model']:34} status={attempt['status']} "
                  f"{str(attempt.get('error'))[:100]}", file=sys.stderr)
        return 1

    print(f"[{result['selection']} requested={result['model']} "
          f"served={result.get('served_model')} effort={result.get('effort')} "
          f"{result['elapsed_s']}s]")
    usage = result.get("usage") or {}
    if usage:
        print(f"[usage in={usage.get('prompt_tokens')} out={usage.get('completion_tokens')} "
              f"total={usage.get('total_tokens')}]")
    skipped = [a for a in result.get("attempts", []) if not a["ok"]]
    if skipped:
        print("[skipped " + ", ".join(f"{a['model']}({a['status']})" for a in skipped) + "]")
    print(result.get("content", ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
