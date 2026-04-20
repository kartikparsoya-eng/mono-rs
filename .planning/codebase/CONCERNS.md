# Concerns

## Performance Bottlenecks (Primary Motivation for Rust Rewrite)

### 1. IVM Pipeline Processing — CPU-bound, single-threaded
- **File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts`
- **Hot path:** `#advance()` method (lines 621-715)
- JS generators in the IVM pipeline cannot be parallelized
- Advancement timeout (lines 752-784) aborts IVM if too slow, blocking WAL switching in WAL2 mode

### 2. Serialization/Deserialization Overhead
- **File:** `packages/zqlite/src/table-source.ts`
- `toSQLiteRow()` uses `Object.fromEntries` per row — allocates new objects for every row
- Significant GC pressure at scale

### 3. CVR Store PostgreSQL Round-trips
- **File:** `packages/zero-cache/src/services/view-syncer/cvr-store.ts` (1329 lines)
- **File:** `packages/zero-cache/src/services/view-syncer/row-record-cache.ts` (445 lines)
- `RowRecordCache` exists solely to paper over PG latency
- Arbitrary page size (TODO at line 178 of row-record-cache.ts)

### 4. SQLite Snapshot Management
- **File:** `packages/zero-cache/src/services/view-syncer/snapshotter.ts` (590 lines)
- `Diff[Symbol.iterator]` does 1-3 SQLite queries per change entry, each crossing FFI boundary
- Lines 362-370: **320x performance regression** from NULL handling in multi-key OR lookups
- BEGIN CONCURRENT for snapshot isolation adds overhead

### 5. GC Pressure
- **File:** `packages/zero-cache/src/services/view-syncer/view-syncer.ts`
- 15+ `performance.now()` calls and `slowHydrateThreshold` suggest known latency issues
- Object allocation patterns in hot paths cause GC pauses at scale

## SQLite FFI Overhead
- Current: JS → N-API → C++ (better-sqlite3) → SQLite
- Each `prepare()`, `run()`, `get()`, `all()`, `iterate()` crosses FFI boundary
- Statement cache in `packages/zqlite/src/internal/statement-cache.ts` mitigates but doesn't eliminate
- 60+ files depend on `db.ts` — any change here has wide blast radius

## TODOs and Known Issues

### zqlite
- `table-source.ts:421` — TODO: Compute this once! (repeated computation)
- `table-source.test.ts:434` — TODO: Fix Row type!!!
- `table-source.test.ts:853` — TODO: Add constraint test with compound keys

### view-syncer
- `snapshotter.ts:245` — TODO: Determine if worth changing definition to count
- `snapshotter.ts:505` — TODO: Consider doing this for deep-equal values
- `snapshotter.ts:528` — TODO: Can we get rid of these RowValue casts?
- `row-record-cache.ts:178` — TODO: Arbitrary page size
- `cvr-store.ts:158` — TODO: Make catchup wait configurable
- `cvr.ts:70` — TODO: Use Immutable<CVR> when AST is immutable

## Architecture Risks for Rust Rewrite

### Wide dependency on `db.ts`
- 60+ files import from `packages/zqlite/src/db.ts`
- Rust replacement must exactly match `Database` and `Statement` APIs
- Any behavioral difference will cascade through the entire system

### IVM Operator Interface Coupling
- `Input`, `Output`, `Storage` interfaces from `packages/zql/src/ivm/operator.ts`
- TypeScript operators and Rust operators must interop seamlessly
- Mixed pipelines (some TS operators, some Rust) during incremental migration

### Test Infrastructure Dependencies
- Tests use `:memory:` SQLite databases — Rust module must support this
- `DbFile` test utility manages temp file lifecycle
- PG test containers for integration tests — unaffected by Rust rewrite

## Security
- JWT validation via `jose` (auth layer, stays in TS)
- No SQL injection risk in parameterized queries (both TS and planned Rust use prepared statements)
- Secret management: connection strings via env vars

## Skip List (Cold Paths — Not Worth Rewriting)
- `packages/zqlite/src/explain-queries.ts` — debug/analysis only
- `packages/zqlite/src/sqlite-cost-model.ts` — query optimization hints
- `packages/zqlite/src/sqlite-stat-fanout.ts` — statistics
- `packages/zero-cache/src/db/lite-tables.ts` — schema inspection
- `packages/zero-cache/src/db/migration-lite.ts` — migration (cold path)
- `packages/zero-cache/src/services/replicator/schema/replica-schema.ts` — schema management
- `packages/zero-cache/src/services/litestream/backup-monitor.ts` — backup monitoring
