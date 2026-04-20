# Roadmap: zero-cache Rust Rewrite

## Overview

Incremental rewrite of zero-cache's performance-critical SQLite layer from TypeScript to Rust via napi-rs. Follows the dependency chain: Foundation (db.ts) -> Data Layer (TableSource, Storage) -> Schema Modules -> High-Impact Services (Snapshotter, ChangeProcessor) -> Bulk Loader -> Benchmarks. Each phase is independently deployable and validated by existing vitest suites.

## Phases

- [ ] **Phase 1: Rust Foundation** - napi-rs module with Database + Statement + statement cache
- [ ] **Phase 2: StatementRunner** - Parameterized query runner for zero-cache
- [ ] **Phase 3: IVM Data Layer** - TableSource + DatabaseStorage in Rust
- [ ] **Phase 4: Schema Modules** - change-log, column-metadata, table-metadata, replication-state
- [ ] **Phase 5: High-Impact Services** - Snapshotter + ChangeProcessor in Rust
- [ ] **Phase 6: Bulk Loader** - initial-sync in Rust
- [ ] **Phase 7: Benchmarks & Validation** - Comparative A/B benchmarks

## Phase Details

### Phase 1: Rust Foundation
**Goal**: Create napi-rs Rust module that exactly replaces `db.ts` + `statement-cache.ts` with zero behavioral changes
**Depends on**: Nothing (first phase)
**Requirements**: FOUND-01, FOUND-02, FOUND-03, TEST-01, TEST-02
**Success Criteria** (what must be TRUE):
  1. `packages/zqlite/src/db.test.ts` passes with Rust module replacing TS `Database`/`Statement`
  2. Rust Database supports `:memory:`, file-backed, WAL mode, WAL2 mode, pragma, exec, run, transaction
  3. Rust Statement supports run, get, all, iterate with exact type coercion matching better-sqlite3
  4. Rust `#[cfg(test)]` unit tests cover NULL handling, type coercion, error cases
  5. Statement cache implements LRU eviction matching `statement-cache.ts` behavior
**Plans**: 3 plans

Plans:
- [x] 01-01: Scaffold napi-rs Rust crate (Cargo.toml, build config, npm package wrapper)
- [x] 01-02: Implement Database + Statement classes with rusqlite
- [x] 01-03: Wire up to existing tests, fix behavioral differences

### Phase 2: StatementRunner
**Goal**: Replace `statements.ts` with Rust StatementRunner, unlocking zero-cache module rewrites
**Depends on**: Phase 1
**Requirements**: FOUND-04, TEST-01
**Success Criteria** (what must be TRUE):
  1. `packages/zero-cache/src/db/statements.test.ts` passes with Rust StatementRunner
  2. Parameterized queries and batch operations work identically to TS version
**Plans**: 1 plan

Plans:
- [ ] 02-01: Implement StatementRunner in Rust, swap imports, validate tests

### Phase 3: IVM Data Layer
**Goal**: Replace TableSource and DatabaseStorage with Rust implementations, eliminating per-row Object.fromEntries overhead
**Depends on**: Phase 1
**Requirements**: IVM-01, IVM-02, TEST-01, TEST-02
**Success Criteria** (what must be TRUE):
  1. `packages/zqlite/src/table-source.test.ts` passes with Rust TableSource
  2. `packages/zqlite/src/database-storage.test.ts` passes with Rust DatabaseStorage
  3. TableSource.fetch() generates correct SQL from AST and returns rows as JS objects without Object.fromEntries
  4. DatabaseStorage implements get/set/del/scan backed by SQLite
**Plans**: 2 plans

Plans:
- [ ] 03-01: Implement Rust TableSource (Input interface, SQL generation, row serialization)
- [ ] 03-02: Implement Rust DatabaseStorage (Storage interface, key-value on SQLite)

### Phase 4: Schema Modules
**Goal**: Replace 4 schema modules (change-log, column-metadata, table-metadata, replication-state) with Rust
**Depends on**: Phase 1
**Requirements**: SCHEMA-01, SCHEMA-02, SCHEMA-03, SCHEMA-04, TEST-01
**Success Criteria** (what must be TRUE):
  1. All schema module tests pass with Rust replacements
  2. Read/write operations produce identical SQLite state to TS versions
**Plans**: 1 plan

Plans:
- [ ] 04-01: Implement all 4 schema modules in Rust (parallel-safe, small modules)

### Phase 5: High-Impact Services
**Goal**: Replace Snapshotter and ChangeProcessor — the two highest-impact rewrite targets
**Depends on**: Phase 1, Phase 3, Phase 4
**Requirements**: SVC-01, SVC-02, TEST-01, TEST-02
**Success Criteria** (what must be TRUE):
  1. `snapshotter.test.ts` (705 lines) passes with Rust Snapshotter
  2. `change-processor.test.ts` passes with Rust ChangeProcessor
  3. Snapshotter Diff iterator keeps all SQLite queries in Rust (no per-change FFI crossing)
  4. NULL-safe multi-key OR lookups work without 320x regression
  5. BEGIN CONCURRENT snapshot isolation works correctly
**Plans**: 2 plans

Plans:
- [ ] 05-01: Implement Rust Snapshotter (Diff iterator, BEGIN CONCURRENT, snapshot comparison)
- [ ] 05-02: Implement Rust ChangeProcessor (CDC write path, PG change -> SQLite ops)

### Phase 6: Bulk Loader
**Goal**: Replace initial-sync with Rust bulk loader for fast initial data population
**Depends on**: Phase 1, Phase 4
**Requirements**: SVC-03, TEST-01
**Success Criteria** (what must be TRUE):
  1. Initial sync tests pass with Rust bulk loader
  2. Batch INSERT operations are significantly faster than TS version
**Plans**: 1 plan

Plans:
- [ ] 06-01: Implement Rust initial sync bulk loader

### Phase 7: Benchmarks & Validation
**Goal**: Prove measurable performance improvement with comparative A/B benchmarks
**Depends on**: Phase 1, Phase 3, Phase 5
**Requirements**: BENCH-01
**Success Criteria** (what must be TRUE):
  1. Benchmark suite extends `packages/zero-cache/bench/` with Rust vs TS comparisons
  2. Measurable throughput improvement (target: 5-10x for SQLite-heavy paths)
  3. GC pause reduction demonstrated under load
**Plans**: 1 plan

Plans:
- [ ] 07-01: Create comparative benchmark suite and validate performance gains

## Progress

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Rust Foundation | 0/3 | Not started | - |
| 2. StatementRunner | 0/1 | Not started | - |
| 3. IVM Data Layer | 0/2 | Not started | - |
| 4. Schema Modules | 0/1 | Not started | - |
| 5. High-Impact Services | 0/2 | Not started | - |
| 6. Bulk Loader | 0/1 | Not started | - |
| 7. Benchmarks & Validation | 0/1 | Not started | - |
