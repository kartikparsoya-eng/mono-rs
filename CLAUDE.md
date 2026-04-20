@AGENTS.md

<!-- GSD:project-start source:PROJECT.md -->
## Project

**zero-cache Rust Rewrite (mono-rs)**

A performance-focused partial rewrite of the Zero sync engine's server-side components (zero-cache, zqlite) from TypeScript to Rust via napi-rs. The goal is to eliminate JavaScript runtime bottlenecks — GC pressure, single-threaded execution, and JS↔C++ SQLite FFI overhead — while keeping the existing TypeScript codebase for non-performance-critical paths (WebSocket/HTTP, auth, protocol handling, change streamer).

**Core Value:** All SQLite I/O and row-level computation must happen in Rust, delivering 5-10x throughput improvement and eliminating GC pauses at scale.

### Constraints

- **Tech stack**: Rust via napi-rs (must produce Node.js native module)
- **API compatibility**: Rust module must exactly match existing TS Database/Statement API
- **Incremental**: Each step must be independently deployable and testable
- **Testing**: Existing vitest suites are the primary correctness gate — must pass unchanged
- **Dependencies**: Critical path is `db.ts → table-source.ts → snapshotter.ts → pipeline-driver.ts`
<!-- GSD:project-end -->

<!-- GSD:stack-start source:codebase/STACK.md -->
## Technology Stack

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
| Package | Purpose |
|---------|---------|
| `zero-cache` | Server-side sync engine (replicator, view-syncer, change-source) |
| `zqlite` | SQLite source provider for ZQL (server-side) |
| `zql` | IVM (Incremental View Maintenance) query library |
| `zero-protocol` | Wire protocol types (AST, Row, PrimaryKey) |
| `zero-client` | Client-side sync library |
| `zero-react` | React bindings |
| `zero-solid` | SolidJS bindings |
| `zero-react-native` | React Native bindings |
| `zero-schema` | Schema definition |
| `zero-types` | Shared type definitions |
| `zero-pg` | PostgreSQL utilities |
| `zero-server` | Server framework |
| `zero-events` | Event system |
| `shared` | Shared utilities |
| `otel` | OpenTelemetry instrumentation |
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
<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->
## Conventions

## Code Style
- **Language:** TypeScript with strict settings (`~6.0.2`)
- **Module system:** ESM (`"type": "module"` in all packages)
- **Imports:** Explicit `.ts` extensions in import paths
- **Formatting:** `oxfmt` (not prettier)
- **Linting:** `oxlint` (not eslint)
## Naming
- **Files:** `kebab-case.ts` (e.g., `table-source.ts`, `change-processor.ts`)
- **Classes:** PascalCase (e.g., `Database`, `TableSource`, `Snapshotter`)
- **Interfaces:** PascalCase, no `I` prefix (e.g., `Input`, `Output`, `Storage`)
- **Private fields:** `#field` (native private fields, not `_field`)
- **Constants:** UPPER_SNAKE_CASE (e.g., `AUTO_VACUUM_INCREMENTAL`, `MB`)
- **Functions:** camelCase
- **Test files:** `feature.test.ts` (co-located with source)
- **PG-only tests:** `feature.pg.test.ts`
## Patterns
### Disposable Pattern
### Private Fields
### LogContext
### OpenTelemetry Tracing
### Error Classes
### Iterator/Generator Pattern
### SQL Tagged Templates
## Error Handling
- Custom error classes per domain
- `try/catch` with `cause` chaining: `throw new Error(msg, {cause})`
- SQLite errors: `SqliteError` from `@rocicorp/zero-sqlite3`
- Logging via `LogContext` (warn for slow queries, error for failures)
<!-- GSD:conventions-end -->

## Row Fetching API Choice

- `.allBuf()` + `decodeBuf()` — hot paths, bulk reads, table-source, snapshotter (>100 rows)
- `.all()` — small result sets, admin queries, tests, inspect-handler
- `Database.queryAll()` — typed hot path (boolean/json conversion in Rust), used by table-source `#queryAllTyped`

Rule: if the call site is in a loop or processes >100 rows, use `.allBuf()` or `queryAll()`.

Note: `queryAll()` already uses interned keys + resolved types in Rust. For table-source specifically,
`queryAll()` is preferred because it handles schema type conversion (boolean, json) in a single FFI call.
`.allBuf()` is raw (no type conversion) — use it where you'd otherwise call `.all()` on large result sets.

<!-- GSD:architecture-start source:ARCHITECTURE.md -->
## Architecture

## Pattern
```
```
## Core Layers
### Layer 0: Data Access (`zqlite`)
- `packages/zqlite/src/db.ts` — `Database` class wrapping `@rocicorp/zero-sqlite3`
- `packages/zqlite/src/internal/statement-cache.ts` — LRU cache for prepared statements
- `Statement` class: `run()`, `get()`, `all()`, `iterate()`
### Layer 1: IVM Data Sources (`zqlite`)
- `packages/zqlite/src/table-source.ts` — `TableSource` implements `Input` interface
- `packages/zqlite/src/database-storage.ts` — `DatabaseStorage` implements `Storage` interface
### Layer 2: IVM Pipeline (`zql`)
- `packages/zql/src/ivm/` — IVM operator library
### Layer 3: Services (`zero-cache`)
#### Change Source
- `packages/zero-cache/src/services/change-source/pg/` — PostgreSQL logical replication
- `packages/zero-cache/src/services/change-source/pg/initial-sync.ts` — bulk initial data load
#### Replicator
- `packages/zero-cache/src/services/replicator/` — applies PG changes to SQLite
- `change-processor.ts` (934 lines) — CDC writer, transforms PG changes to SQLite ops
- Schema metadata: `schema/change-log.ts`, `column-metadata.ts`, `table-metadata.ts`, `replication-state.ts`
#### View Syncer
- `packages/zero-cache/src/services/view-syncer/` — per-client IVM
- `pipeline-driver.ts` (900+ lines) — orchestrates IVM pipeline
- `snapshotter.ts` (590 lines) — manages SQLite snapshots, `Diff[Symbol.iterator]`
- `cvr-store.ts` (1329 lines) — Client View Record persistence in PostgreSQL
- `row-record-cache.ts` (445 lines) — write-back cache to reduce PG round-trips
- `client-handler.ts` — WebSocket client connection handler
## Data Flow
## Key Interfaces
- `Input` (`operator.ts`): `fetch(req)`, `cleanup(req)`, `setOutput(output)`, `getSchema()`
- `Output` (`operator.ts`): `push(change)`
- `Storage` (`operator.ts`): `get(key)`, `set(key, value)`, `del(key)`, `scan(options)`
- `FetchRequest`: `constraint`, `start`, `reverse`
## Entry Points
- `packages/zero-cache/src/server/runner/main.ts` — server entry
- `packages/zero-cache/src/services/runner.ts` — service orchestrator
- `packages/zero-cache/src/services/life-cycle.ts` — lifecycle management
<!-- GSD:architecture-end -->

<!-- GSD:skills-start source:skills/ -->
## Project Skills

No project skills found. Add skills to any of: `.claude/skills/`, `.agents/skills/`, `.cursor/skills/`, or `.github/skills/` with a `SKILL.md` index file.
<!-- GSD:skills-end -->

<!-- GSD:workflow-start source:GSD defaults -->
## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:
- `/gsd-quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd-debug` for investigation and bug fixing
- `/gsd-execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->

<!-- GSD:profile-start -->
## Developer Profile

> Profile not yet configured. Run `/gsd-profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
