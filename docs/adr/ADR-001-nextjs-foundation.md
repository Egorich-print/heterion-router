# ADR-001: Next.js as the Foundation for an AI Gateway

## Status: Accepted

## Context

OmniRoute is an AI routing gateway that translates, forwards, and manages requests across 20+ LLM providers. We needed a framework that could serve both the API proxy layer and a management dashboard from a single codebase.

**Alternatives considered:**

- **Express.js only** — Simpler proxy, but requires separate frontend tooling
- **Fastify** — Fast, but no built-in SSR/dashboard support
- **Next.js** — Unified full-stack framework with API routes, SSR, and static pages

## Decision

We chose Next.js because:

1. **Single deployment** — API routes (`/api/*`) and dashboard UI in one process
2. **Middleware layer** — Native request interception for auth guards and request tracing
3. **File-based routing** — Easy to map provider endpoints to handlers
4. **Built-in TypeScript** — Type safety across the entire codebase

## Consequences

**Positive:**

- One `npm run build` produces both API and UI
- Middleware provides centralized auth and request tracing
- Dashboard gets automatic code splitting and optimization

**Negative:**

- Next.js middleware has limitations (no heavy imports, edge runtime constraints)
- Serverless deployment model doesn't align with persistent WebSocket/SSE connections
- Build times are longer than Express-only setups
- The SSE proxy layer (`open-sse/`) operates outside Next.js conventions
