# ADR-003: Dual Storage — SQLite Primary with JSON Migration Path

## Status: Accepted

## Context

OmniRoute originally used LowDB (JSON file) for all persistence. As the project grew, JSON-based storage became a bottleneck for concurrent access, querying, and data integrity.

**Alternatives considered:**

- **LowDB only** — Simple but no concurrent access, no ACID, no querying
- **SQLite only** — Fast, ACID-compliant, but breaks existing deployments
- **PostgreSQL** — Production-grade but requires external dependency
- **Dual storage with migration** — SQLite primary + automatic JSON migration

## Decision

We migrated to **SQLite as the primary store** with an automatic one-time migration from `db.json`:

1. On startup, if `db.json` exists and SQLite is empty, auto-migrate all data
2. All new reads/writes go through SQLite
3. The `db.json` file is preserved but no longer written to

Settings remain in a hybrid model where LowDB handles simple key-value configuration for backward compatibility.

## Consequences

**Positive:**

- ACID transactions for provider connections, API keys, and usage data
- Proper SQL queries for analytics and log filtering
- Concurrent read/write safety via WAL mode
- Zero-downtime migration from JSON — users upgrade transparently

**Negative:**

- Two storage engines to maintain (SQLite + LowDB for settings)
- Migration code must handle edge cases and partial data
- SQLite binary dependency needed in deployment environments
