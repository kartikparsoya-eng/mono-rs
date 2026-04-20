# Architecture

## Pattern
Zero is a **local-first sync engine** using Incremental View Maintenance (IVM). The architecture follows a pipeline pattern:

```
PostgreSQL (source of truth)
  → Logical Replication
    → Change Source (CDC consumer)
      → Change Streamer (fan-out)
        → Replicator (applies changes to per-client SQLite)
          → View Syncer (IVM pipeline per client)
            → Client (WebSocket push)
```

## Core Layers

### Layer 0: Data Access (`zqlite`)
- `packages/zqlite/src/db.ts` — `Database` class wrapping `@rocicorp/zero-sqlite3`
  - `prepare(sql)` → `Statement`
  - `exec(sql)` — raw SQL execution
  - `run(sql, ...params)` — parameterized execute
  - `transaction(fn)` — synchronous transactions
- `packages/zqlite/src/internal/statement-cache.ts` — LRU cache for prepared statements
- `Statement` class: `run()`, `get()`, `all()`, `iterate()`

### Layer 1: IVM Data Sources (`zqlite`)
- `packages/zqlite/src/table-source.ts` — `TableSource` implements `Input` interface
  - `fetch(req)` — generates SQL from AST, returns rows
  - `toSQLiteRow()` — converts raw SQLite rows to JS objects (bottleneck: `Object.fromEntries` per row)
  - Implements `getSchema()`, `setOutput()`, `cleanup()`
- `packages/zqlite/src/database-storage.ts` — `DatabaseStorage` implements `Storage` interface
  - `get(key)`, `set(key, value)`, `del(key)`, `scan(options)`

### Layer 2: IVM Pipeline (`zql`)
- `packages/zql/src/ivm/` — IVM operator library
  - Operators: `join.ts`, `filter.ts`, `sort.ts`, `take.ts`, `exists.ts`, `cap.ts`
  - Core interfaces in `operator.ts`: `Input`, `Output`, `Storage`, `FetchRequest`
  - `change.ts`, `change-type.ts` — change propagation types
  - Operators are composed into pipelines, connected via `setOutput()`

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
  - `#advance()` (lines 621-715) — **hottest code path**, processes changes through operators
  - Advancement timeout (lines 752-784) — aborts if IVM too slow (blocks WAL switching)
- `snapshotter.ts` (590 lines) — manages SQLite snapshots, `Diff[Symbol.iterator]`
  - Lines 362-370: 320x perf regression from NULL handling
  - BEGIN CONCURRENT for snapshot isolation
- `cvr-store.ts` (1329 lines) — Client View Record persistence in PostgreSQL
- `row-record-cache.ts` (445 lines) — write-back cache to reduce PG round-trips
- `client-handler.ts` — WebSocket client connection handler

## Data Flow

1. **PG → SQLite:** Change source receives WAL events → replicator writes to per-client SQLite DBs
2. **SQLite → IVM:** View syncer reads from SQLite via `TableSource` → feeds IVM pipeline
3. **IVM → Client:** Pipeline produces diffs → serialized via `zero-protocol` → pushed over WebSocket

## Key Interfaces
- `Input` (`operator.ts`): `fetch(req)`, `cleanup(req)`, `setOutput(output)`, `getSchema()`
- `Output` (`operator.ts`): `push(change)`
- `Storage` (`operator.ts`): `get(key)`, `set(key, value)`, `del(key)`, `scan(options)`
- `FetchRequest`: `constraint`, `start`, `reverse`

## Entry Points
- `packages/zero-cache/src/server/runner/main.ts` — server entry
- `packages/zero-cache/src/services/runner.ts` — service orchestrator
- `packages/zero-cache/src/services/life-cycle.ts` — lifecycle management
