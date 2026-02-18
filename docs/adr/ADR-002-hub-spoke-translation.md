# ADR-002: Hub-and-Spoke Translation with OpenAI as Intermediate Format

## Status: Accepted

## Context

OmniRoute routes requests across 20+ providers, each with its own API format (OpenAI, Anthropic Messages, Google Gemini, AWS Bedrock, etc.). Direct provider-to-provider translation would require O(n²) translators.

**Alternatives considered:**

- **Direct translation** — Each pair needs a dedicated translator (n² complexity)
- **Common intermediate format** — Translate to/from a canonical format (2n complexity)
- **Protocol buffers** — Strong typing but heavy overhead for a proxy

## Decision

We use the **OpenAI Chat Completions format** as the canonical intermediate representation. All incoming requests are normalized to OpenAI format, processed, then translated to the target provider's format.

```
Client → [any format] → OpenAI canonical → [target format] → Provider
Provider → [response] → OpenAI canonical → [original format] → Client
```

## Consequences

**Positive:**

- Only 2 translators per provider (inbound + outbound) instead of n² pairs
- OpenAI format is the de facto standard — most clients already use it
- Adding a new provider requires only implementing one translator pair
- Streaming (SSE) works consistently through the canonical format

**Negative:**

- Some provider-specific features may be lost in translation
- The double translation adds latency (typically < 5ms)
- OpenAI format changes require updating the canonical representation
