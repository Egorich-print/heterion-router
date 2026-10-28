#!/usr/bin/env python3
"""Replay a recorded agent conversation through the router and measure it.

The repetition loop was found in a transcript, not in a live call: codestral
answered an agentic task with one 110 KB block that repeated the same lint
code until the token cap. To tell whose fault that is, the exact conversation
is rebuilt from `messages.jsonl` and sent to the router again — once streamed,
once not. If both replays loop, the model degenerates; if only one does, the
router's stream path is the culprit.

Usage:
  ./scripts/subagent/replay_session.py <session-dir> [--model MODEL]
                                       [--max-tokens N] [--stream] [--no-stream]
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from repeat_check import measure  # noqa: E402

BASE_URL = os.environ.get("HETERION_BASE_URL", "http://127.0.0.1:20128/v1").rstrip("/")
# The router's client key is a credential: it is read from the environment
# only and never stored in the repository.
API_KEY = os.environ.get("HETERION_API_KEY", "")
if not API_KEY:
    raise SystemExit(
        "HETERION_API_KEY is unset: export the router's client key "
        "(see the router's API-key list) before running this script."
    )


def load_messages(session_dir):
    path = Path(session_dir) / "messages.jsonl"
    out = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.strip():
            continue
        try:
            out.append(json.loads(line)["message"])
        except (json.JSONDecodeError, KeyError):
            continue
    return out


def to_openai_messages(messages):
    """Rebuild the conversation in OpenAI chat format."""
    out = []
    for message in messages:
        role = message.get("role")
        content = message.get("content")
        if role == "user":
            text = "\n".join(
                part.get("text") or ""
                for part in (content if isinstance(content, list) else [])
                if part.get("type") == "text"
            )
            out.append({"role": "user", "content": text})
        elif role == "assistant":
            text_parts, calls = [], []
            for part in content if isinstance(content, list) else []:
                if part.get("type") == "text":
                    text_parts.append(part.get("text") or "")
                elif part.get("type") == "toolCall":
                    calls.append(
                        {
                            "id": part.get("id"),
                            "type": "function",
                            "function": {
                                "name": part.get("name"),
                                "arguments": json.dumps(part.get("arguments") or {}),
                            },
                        }
                    )
            entry = {"role": "assistant", "content": "\n".join(text_parts) or None}
            if calls:
                entry["tool_calls"] = calls
            out.append(entry)
        elif role == "toolResult":
            text = "\n".join(
                part.get("text") or ""
                for part in (content if isinstance(content, list) else [])
                if part.get("type") == "text"
            )
            out.append(
                {
                    "role": "tool",
                    "tool_call_id": message.get("toolCallId"),
                    "content": text,
                }
            )
    return out


def load_tools(session_dir):
    """The tool schema the runtime sent, if the call record still holds it."""
    path = Path(session_dir) / "llm-call.json"
    if not path.exists():
        return None
    try:
        config = json.loads(path.read_text(encoding="utf-8", errors="replace"))
    except json.JSONDecodeError:
        return None
    tools = config.get("tools")
    if not tools:
        return None
    return [{"type": "function", "function": tool} for tool in tools]


def post(model, messages, *, stream, max_tokens, timeout=600, tools=None):
    body = {
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
    }
    if tools:
        body["tools"] = tools
    if stream:
        body["stream"] = True
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
            raw = response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return "", f"HTTP {error.code}: {error.read().decode('utf-8', 'replace')[:300]}"
    except Exception as error:  # noqa: BLE001
        return "", f"{type(error).__name__}: {error}"
    if not stream:
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError:
            return raw, None
        choice = (payload.get("choices") or [{}])[0]
        message = choice.get("message") or {}
        return message.get("content") or "", None
    parts = []
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        chunk = line[len("data:"):].strip()
        if not chunk or chunk == "[DONE]":
            continue
        try:
            event = json.loads(chunk)
        except json.JSONDecodeError:
            continue
        for choice in event.get("choices") or []:
            piece = (choice.get("delta") or {}).get("content")
            if piece:
                parts.append(piece)
    return "".join(parts), None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("session_dir")
    parser.add_argument("--model", default="mistral/codestral-latest")
    parser.add_argument("--max-tokens", type=int, default=32768)
    parser.add_argument("--stream", action="store_true")
    parser.add_argument("--no-stream", action="store_true")
    parser.add_argument("--head", type=int, default=400, help="chars of output to show")
    args = parser.parse_args()

    messages = to_openai_messages(load_messages(args.session_dir))
    tools = load_tools(args.session_dir)
    modes = []
    if args.stream:
        modes.append(True)
    if args.no_stream:
        modes.append(False)
    if not modes:
        modes = [True, False]

    print(f"session: {args.session_dir}")
    print(f"model:   {args.model}  messages: {len(messages)}  tools: {len(tools or [])}")
    for stream in modes:
        text, error = post(
            args.model,
            messages,
            stream=stream,
            max_tokens=args.max_tokens,
            tools=tools,
        )
        label = "stream" if stream else "plain "
        if error:
            print(f"  {label}: ERROR {error}")
            continue
        result = measure(text)
        print(
            f"  {label}: {result['verdict']:8} ngram={result['ngram_repeat_ratio']} "
            f"dup={result['dup_line_ratio']} words={result['words']} "
            f"longest_repeat={result['longest_repeat_words']} chars={result['chars']}"
        )
        if result["verdict"] != "OK":
            print(f"     sample: {result['longest_repeat_sample'][:200]}")
        print(f"     head: {text[: args.head]!r}")
        if result["words"] > 40:
            print(f"     tail: {text[-200:]!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
