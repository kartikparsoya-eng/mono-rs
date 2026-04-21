# Phase 21: Rust TableSource with Multi-Connection Pool - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Port the TS `TableSource` class (packages/zqlite/src/table-source.ts, ~771 lines) to Rust with a connection pool that enables parallel SQLite reads. Each `fetch()` uses a read-only connection from the pool, all pinned to the same WAL snapshot. Includes the overlay system for self-join correctness during push propagation, filter/sort pushdown into SQL, and the `genPush()` fan-out to connected pipelines.

This phase does NOT include parallel multi-pipeline hydration (Phase 22), within-pipeline child parallelism (Phase 23), or full operator tree advance (Phase 24). The connection pool is built here; parallelism consumers come later.

</domain>

<decisions>
## Implementation Decisions

### Connection Pool Architecture

- **D-01:** Custom lightweight pool using `Vec<rusqlite::Connection>` behind `Arc<Mutex<Vec<Connection>>>`. Threads take/return connections. No r2d2 dependency — overkill for fixed-snapshot read-only connections.
- **D-02:** WAL snapshot pinning via `BEGIN DEFERRED` (read transaction) on each connection at pool creation time. All connections see the same data. Pool `set_snapshot()` closes old transactions and opens new ones at the new WAL position.
- **D-03:** Pool lives in `packages/zqlite-rs/` (not zero-ivm-rs) since it touches SQLite directly. RustTableSource also lives in zqlite-rs.

### Overlay System Design

- **D-04:** HashMap-based overlay matching TS semantics exactly. `HashMap<Vec<Value>, OverlayEntry>` keyed by primary key values. Epoch tracking for `lastPushedEpoch` per connection to only show overlay entries from the current push cycle.
- **D-05:** `generateWithOverlay` and `generateWithOverlayUnordered` ported to Rust as methods on RustTableSource. Overlay is set/cleared during `genPush()` exactly as in TS.

### Source-Operator Integration

- **D-06:** Separate `Source` trait (not extending `Operator`). TS has distinct Source/Input/Output interfaces — mirror this. Source has `connect()` returning a `SourceInput` struct with a `fetch()` method. The Phase 20 pipeline builder's `Source` config variant becomes a reference to a RustTableSource instance.
- **D-07:** `SourceInput.fetch()` takes the same `FetchRequest` type from Phase 20. Returns `Vec<Node>` (same as Operator.fetch). The pipeline builder wires source's fetch as the leaf of the operator tree.

### SQL Query Building

- **D-08:** Port `buildSelectQuery()` from `packages/zqlite/src/query-builder.ts` to Rust. Handles constraint -> WHERE clause, ordering -> ORDER BY, start bounds -> bound conditions, filter pushdown. ~200 lines of SQL string construction, straightforward to port.
- **D-09:** Reuse existing Rust `queryAll` (from zqlite-rs db module) for executing the built SQL. The `queryAll` path already handles type conversion and binary buffer protocol.

### Push Path (genPush)

- **D-10:** Port `genPushAndWriteWithSplitEdit()` from memory-source.ts to Rust. This handles: check if row exists -> determine ADD/REMOVE/EDIT -> set overlay -> fan out to all connected pipelines -> write change to SQLite.
- **D-11:** Write-after-push ordering preserved: changes are written to SQLite AFTER being pushed through the operator tree, ensuring self-join correctness (TS comment: "we can't reveal a value to an output before it has been pushed to that output").

### Claude's Discretion

- Internal data structures for connection pool (channel vs mutex vs lock-free)
- SQL parameter binding strategy (prepared statements vs string interpolation)
- Statement caching approach in Rust (LRU cache vs HashMap)
- Error handling strategy (Result types, error conversion)

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### TS TableSource (source of truth for behavior)

- `packages/zqlite/src/table-source.ts` — Main TableSource class (771 lines): connect(), fetch(), genPush(), writeChange(), overlay system
- `packages/zql/src/ivm/memory-source.ts` — generateWithOverlay, generateWithOverlayUnordered, genPushAndWriteWithSplitEdit, Connection/Overlay types
- `packages/zql/src/ivm/source.ts` — Source, SourceChange, SourceInput interfaces
- `packages/zql/src/ivm/operator.ts` — FetchRequest, Input, Output interfaces
- `packages/zqlite/src/query-builder.ts` — buildSelectQuery(), SQL construction from constraints/ordering/start

### Existing Rust Code (integration points)

- `packages/zero-ivm-rs/src/operator.rs` — Operator trait (fetch/push) from Phase 20
- `packages/zero-ivm-rs/src/pipeline.rs` — Pipeline builder, OperatorConfig with Source variant
- `packages/zero-ivm-rs/src/types.rs` — Node, Change, FetchRequest, Row, Constraint, SortSpec types
- `packages/zqlite-rs/src/lib.rs` — Existing Database/Statement napi bindings, queryAll
- `packages/zqlite-rs/src/advance.rs` — rust_fan_out(), existing push path (filter-only)

### Correctness Infrastructure

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — Lines 1090-1110: TableSource creation, setDB() snapshot management
- `packages/zero-cache/src/services/view-syncer/dual-executor.ts` — Dual-exec comparator for Rust vs TS validation

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `zqlite-rs/src/lib.rs` Database wrapper: already has `queryAll` with type conversion and binary buffer protocol — reuse for fetch SQL execution
- `zero-ivm-rs/src/types.rs` core types: Row, Node, Change, FetchRequest, Constraint, SortSpec — shared with operators
- `zero-ivm-rs/src/pipeline.rs` OperatorConfig::Source variant: already has table_name, columns, primary_key, sort — extend for connection pool config
- `zero-ivm-rs/src/storage.rs` RustStorage HashMap: could inform overlay implementation pattern

### Established Patterns

- napi classes for TS interop: `#[napi]` structs with methods (Database, RustPipeline, etc.)
- Separate crates: zero-ivm-rs (pure computation, no SQLite) vs zqlite-rs (SQLite-touching)
- serde_json::Value for row data interchange between TS and Rust
- Binary buffer protocol for bulk row transfer (queryAll returns Buffer)

### Integration Points

- Pipeline builder needs to accept a RustTableSource reference as the leaf operator
- `pipeline-driver.ts` creates TableSource instances — needs napi constructor for RustTableSource
- `setDB()` called when snapshot changes — maps to pool snapshot update
- `genPush()` called from pipeline-driver advance path — needs napi method

</code_context>

<specifics>
## Specific Ideas

- Connection pool should pre-open N connections (configurable, default 4) at construction
- WAL snapshot pinning: `PRAGMA wal_checkpoint(PASSIVE)` not needed — just BEGIN DEFERRED on each connection at same point
- The TS `shouldYield` callback is not needed in Rust — yielding is a Node.js event loop concern; Rust runs on Rayon threads where blocking is fine
- Statement caching per-connection (each connection has its own StatementCache equivalent)

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

_Phase: 21-rust-tablesource-with-multi-connection-pool_
_Context gathered: 2026-04-21_
