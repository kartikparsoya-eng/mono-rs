# Stack

## Languages & Runtime

- **TypeScript** (~6.0.2) — primary language across all packages
- **Node.js** — server runtime for zero-cache
- **Target:** Rust via napi-rs for performance-critical paths (planned)

## Package Manager & Build

- **npm** workspaces — monorepo structure (`apps/*`, `packages/*`, `prod`, `tools/*`)
- **Turbo** (turborepo) — build orchestration (`npx turbo run build`)
- **tsx** — TypeScript execution for scripts and dev
- **oxlint** — linting
- **oxfmt** — formatting

## Core Packages

| Package             | Purpose                                                          |
| ------------------- | ---------------------------------------------------------------- |
| `zero-cache`        | Server-side sync engine (replicator, view-syncer, change-source) |
| `zqlite`            | SQLite source provider for ZQL (server-side)                     |
| `zql`               | IVM (Incremental View Maintenance) query library                 |
| `zero-protocol`     | Wire protocol types (AST, Row, PrimaryKey)                       |
| `zero-client`       | Client-side sync library                                         |
| `zero-react`        | React bindings                                                   |
| `zero-solid`        | SolidJS bindings                                                 |
| `zero-react-native` | React Native bindings                                            |
| `zero-schema`       | Schema definition                                                |
| `zero-types`        | Shared type definitions                                          |
| `zero-pg`           | PostgreSQL utilities                                             |
| `zero-server`       | Server framework                                                 |
| `zero-events`       | Event system                                                     |
| `shared`            | Shared utilities                                                 |
| `otel`              | OpenTelemetry instrumentation                                    |

## Key Dependencies

### SQLite

- `@rocicorp/zero-sqlite3` (^1.0.17) — better-sqlite3 fork, used by `zqlite` and `zero-cache`

### PostgreSQL

- `postgres` (3.4.7) — PostgreSQL client (postgres.js)
- `@databases/sql` (^3.3.0) — SQL query builder
- `@drdgvhbh/postgres-error-codes` — PG error code constants

### HTTP/WebSocket

- `fastify` (^5.0.0) — HTTP server
- `@fastify/websocket` (^11.0.0) — WebSocket support
- `@fastify/cors` (^10.0.0) — CORS handling
- `ws` (^8.18.1) — WebSocket client

### Observability

- `@opentelemetry/api` (^1.9.0) — tracing/metrics
- `@opentelemetry/sdk-node` — Node.js SDK
- `@opentelemetry/auto-instrumentations-node` — auto-instrumentation
- `@opentelemetry/exporter-metrics-otlp-http` — metrics export

### Auth

- `jose` (^5.9.3) — JWT handling
- `basic-auth` (^2.0.1) — HTTP basic auth

### Testing

- `vitest` (4.1.3) — test framework
- `@testcontainers/postgresql` (^10.9.0) — PostgreSQL test containers
- `mockttp` (^4.2.1) — HTTP mocking
- `nock` (^14.0.4) — HTTP mocking
- `fast-check` (^3.18.0) — property-based testing

### Utilities

- `nanoid` — ID generation
- `eventemitter3` — event emitters
- `@rocicorp/lock` — async locking
- `@rocicorp/resolver` — promise utilities
- `compare-utf8` — UTF-8 comparison
- `json-custom-numbers` — JSON parsing with numeric precision

## Configuration

- `tsconfig.json` per package
- `vitest.config.ts` per package (with PG version-specific configs in zero-cache)
- `turbo.json` for build pipeline
- Root `package.json` defines workspaces
