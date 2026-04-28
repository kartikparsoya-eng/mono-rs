# Integrations

## Databases

### SQLite (Primary — Client Replicas)

- **Library:** `@rocicorp/zero-sqlite3` (^1.0.17) — fork of better-sqlite3
- **Usage:** Each client gets a per-client SQLite database as a materialized view
- **Key files:**
  - `packages/zqlite/src/db.ts` — `Database` wrapper with `prepare()`, `exec()`, `run()`, `transaction()`
  - `packages/zqlite/src/internal/statement-cache.ts` — prepared statement caching
  - `packages/zqlite/src/table-source.ts` — IVM data source backed by SQLite
  - `packages/zqlite/src/database-storage.ts` — IVM operator state storage in SQLite
- **Modes:** WAL mode, WAL2 mode, BEGIN CONCURRENT for snapshot isolation
- **FFI:** JS ↔ C++ binding via N-API (current bottleneck target for Rust rewrite)

### PostgreSQL (Upstream — Source of Truth)

- **Library:** `postgres` (3.4.7, aka postgres.js)
- **Usage:** Upstream data source, logical replication, CVR storage
- **Key files:**
  - `packages/zero-cache/src/services/change-source/pg/` — PG logical replication consumer
  - `packages/zero-cache/src/services/view-syncer/cvr-store.ts` — CVR persistence in PG
  - `packages/zero-pg/` — PG utility functions
- **Testing:** `@testcontainers/postgresql` for PG 15/16/17/18 test matrices

## HTTP/WebSocket Server

- **Framework:** Fastify v5 with WebSocket plugin
- **Key files:**
  - `packages/zero-cache/src/services/http-service.ts`
  - WebSocket used for client sync protocol
- **CORS:** `@fastify/cors` for cross-origin support

## Observability

- **OpenTelemetry** full stack: traces, metrics, logs
- **Exporters:** OTLP HTTP for metrics
- **Custom metrics:** `packages/zero-cache/src/services/statz.ts`
- **Datadog integration:** `packages/datadog/`

## Auth

- **JWT validation:** `jose` library
- **Basic auth:** For admin/internal endpoints
- **Key files:** View syncer auth maintenance in `packages/zero-cache/src/services/view-syncer/`

## Cloud Events

- **Library:** `cloudevents` (^10.0.0)
- **Usage:** Event notification/webhook system

## Litestream

- **Integration:** `packages/zero-cache/src/services/litestream/`
- **Purpose:** SQLite backup/replication to object storage
