# Requirements: zero-cache Rust Rewrite

**Defined:** 2026-04-20
**Core Value:** All SQLite I/O and row-level computation in Rust for 5-10x throughput

## v1 Requirements

### Foundation

- [ ] **FOUND-01**: Rust napi-rs module with Database class matching `db.ts` API (prepare, exec, run, transaction, pragma, Disposable)
- [ ] **FOUND-02**: Rust Statement class matching Statement API (run, get, all, iterate) with exact type coercion behavior
- [ ] **FOUND-03**: Rust prepared statement LRU cache matching `statement-cache.ts` behavior
- [ ] **FOUND-04**: Rust StatementRunner matching `statements.ts` API (parameterized queries, batch operations)

### IVM Data Layer

- [ ] **IVM-01**: Rust TableSource implementing Input interface (fetch, cleanup, setOutput, getSchema) with SQL generation from AST
- [ ] **IVM-02**: Rust DatabaseStorage implementing Storage interface (get, set, del, scan) backed by SQLite

### Schema Modules

- [ ] **SCHEMA-01**: Rust change-log matching `change-log.ts` read/write operations
- [ ] **SCHEMA-02**: Rust column-metadata matching `column-metadata.ts` read/write operations
- [ ] **SCHEMA-03**: Rust table-metadata matching `table-metadata.ts` read/write operations
- [ ] **SCHEMA-04**: Rust replication-state matching `replication-state.ts` read/write operations

### High-Impact Services

- [ ] **SVC-01**: Rust Snapshotter with Diff iterator matching `snapshotter.ts` (BEGIN CONCURRENT, snapshot comparison, NULL-safe multi-key lookups)
- [ ] **SVC-02**: Rust ChangeProcessor matching `change-processor.ts` (CDC write path, PG change → SQLite operations)
- [ ] **SVC-03**: Rust initial sync bulk loader matching `initial-sync.ts` (batch INSERT, table creation)

### Testing & Benchmarks

- [ ] **TEST-01**: All existing vitest tests pass unchanged after swapping TS→Rust imports
- [ ] **TEST-02**: Rust `#[cfg(test)]` unit tests for edge cases, memory safety, thread safety
- [ ] **BENCH-01**: Comparative benchmarks showing measurable improvement over TS baseline

## v2 Requirements

### Phase 2 — IVM Pipeline

- **PIPE-01**: Rust pipeline-driver `#advance()` replacing hottest code path
- **PIPE-02**: Rust IVM operators (join, take, filter, sort) using zero-cost iterators

### Phase 3 — CVR Store

- **CVR-01**: Rust CVR store replacing `cvr-store.ts` (embedded store or Rust PG client)
- **CVR-02**: Rust row-record-cache replacing `row-record-cache.ts` (RocksDB/sled)

## Out of Scope

| Feature | Reason |
|---------|--------|
| Client-side code (zero-client, zero-react) | Not performance-critical, different runtime |
| WebSocket/HTTP server (Fastify) | Stays TS, not a bottleneck |
| Auth/JWT handling | Stays TS, not a bottleneck |
| Change streamer | Stays TS, fan-out is not CPU-bound |
| Protocol handling | Stays TS, serialization is minor |
| Cold-path utilities (lite-tables, migrations, backup) | Diminishing returns |

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| FOUND-01 | Phase 1 | Pending |
| FOUND-02 | Phase 1 | Pending |
| FOUND-03 | Phase 1 | Pending |
| FOUND-04 | Phase 2 | Pending |
| IVM-01 | Phase 3 | Pending |
| IVM-02 | Phase 3 | Pending |
| SCHEMA-01 | Phase 4 | Pending |
| SCHEMA-02 | Phase 4 | Pending |
| SCHEMA-03 | Phase 4 | Pending |
| SCHEMA-04 | Phase 4 | Pending |
| SVC-01 | Phase 5 | Pending |
| SVC-02 | Phase 5 | Pending |
| SVC-03 | Phase 6 | Pending |
| TEST-01 | Phase 1-6 | Pending |
| TEST-02 | Phase 1-6 | Pending |
| BENCH-01 | Phase 7 | Pending |

**Coverage:**
- v1 requirements: 16 total
- Mapped to phases: 16
- Unmapped: 0

---
*Requirements defined: 2026-04-20*
*Last updated: 2026-04-20 after initial definition*
