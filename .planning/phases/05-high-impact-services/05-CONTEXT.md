# Phase 5: High-Impact Services - Context

**Gathered:** 2026-04-20
**Status:** Ready for planning

<domain>
## Phase Boundary

Optimize Snapshotter's read-path hot loop by exposing typed Rust methods (getRow, getRows, changesSince) that eliminate per-row FFI overhead and inline type conversion. ChangeProcessor stays in TypeScript (D-15). The Diff iterator logic stays in TS but calls optimized Rust methods instead of generic Statement.get/all/iterate.

</domain>

<decisions>
## Implementation Decisions

### ChangeProcessor Scope
- **D-15:** ChangeProcessor stays entirely in TypeScript. D-13 (write path stays TS) applies cleanly — the module is 95% writes with single-row ops already efficient via Phase 1's Rust Database. processBackfill is a cold catch-up path, not steady-state. If backfill loop performance surfaces later, address with queryBatched pattern.

### Snapshotter Diff Boundary
- **D-16:** Individual optimized Rust methods, not a monolithic diff function. Rust exposes `getRow`, `getRows`, `changesSince` as methods that accept column type metadata and return converted results. The TS Diff iterator (`[Symbol.iterator]`) stays in TypeScript and calls these methods. Simpler, lower risk, still eliminates per-row JS object construction overhead.

### Type Conversion (fromSQLiteTypes)
- **D-17:** Type conversion happens inline in Rust read methods. `getRow`/`getRows`/`changesSince` accept column type metadata (same pattern as `queryAll` from Phase 3) and return already-converted rows. The standalone TS `fromSQLiteTypes` export remains available for other consumers (D-12 still applies to the export), but Snapshotter's hot path no longer calls it.

### Protocol Selection (allBuf vs direct)
- **D-18:** Mixed protocol based on result size characteristics:
  - `getRow`: Direct napi object return — always 0-1 rows, allBuf overhead not justified
  - `getRows`: allBuf protocol — multi-key OR means potentially unbounded results, consistency with queryAll pattern
  - `changesSince`: allBuf batches of 500 — thousands of entries, same queryBatched pattern from Phase 3

### Claude's Discretion
- Rust method signatures (how table spec / column types are passed)
- Whether getRow/getRows are new Database methods or Snapshot-specific helpers
- changesSince batch iteration API design (callback vs iterator vs generator)
- How safeIntegers threshold is communicated to Rust methods

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Snapshotter Source (modification target)
- `packages/zero-cache/src/services/view-syncer/snapshotter.ts` — Snapshot class, Diff iterator, getRow/getRows/changesSince (590 lines)

### Test Files (correctness gates)
- `packages/zero-cache/src/services/view-syncer/snapshotter.test.ts` — MUST pass unchanged
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — Uses Snapshotter, must pass

### Rust Crate (extend)
- `packages/zqlite-rs/src/database.rs` — Add getRow/getRows methods or new module
- `packages/zqlite-rs/src/statement.rs` — Existing statement execution patterns
- `packages/zqlite-rs/src/types.rs` — Type conversion logic (queryAll pattern to reuse)

### Prior Phase Context
- `.planning/phases/03-ivm-data-layer/03-CONTEXT.md` — D-09 through D-13, queryAll/queryBatched patterns
- `packages/zqlite/src/db.ts` — allBuf/decodeBuf implementations to reuse

### Integration Points
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — Calls snapshotter.advance(), processes Diff
- `packages/zero-cache/src/db/statements.ts` — StatementRunner wraps Database (D-08 stays TS)
- `packages/zqlite/src/table-source.ts` — fromSQLiteTypes export, queryAll pattern reference

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `Database.queryAll()` in `database.rs` — Column type metadata + interned keys + type conversion. Same pattern for getRow/getRows.
- `allBuf` binary protocol in `statement.rs` — Reuse for getRows and changesSince batches.
- `decodeBuf()` in `db.ts` — TS decoder for binary buffer protocol, already tested.

### Established Patterns
- **Column type metadata:** Passed as object `{col: "boolean"|"json"|...}` to Rust, parsed once per query. Reuse from queryAll.
- **safeIntegers:** BigInt only for values >= MAX_SAFE_INTEGER. Already implemented in Rust.
- **Statement cache:** Snapshotter uses `StatementRunner.statementCache` for prepared statements. New Rust methods may bypass this or integrate.
- **BEGIN CONCURRENT:** Snapshotter opens concurrent transactions for snapshot isolation. Rust methods operate within these existing transactions.

### Integration Points
- `Snapshot.getRow()` (line 339) — TS builds SQL dynamically from table spec columns/keys. Rust method needs same inputs.
- `Snapshot.getRows()` (line 357) — NULL filtering for multi-key OR. Must be preserved in Rust or TS caller.
- `Snapshot.changesSince()` (line 326) — Returns iterator + cleanup function. Rust batched version needs similar lifecycle.
- `Diff[Symbol.iterator]` (line 421) — Calls getRow/getRows, applies fromSQLiteTypes, yields Changes. Stays TS, calls Rust methods.

</code_context>

<specifics>
## Specific Ideas

- getRow returns `Row | undefined` — direct napi object, same as Statement.get() but with type conversion inline
- getRows returns decoded allBuf array — TS calls decodeBuf() on the buffer
- changesSince returns a batched iterator: Rust method called repeatedly with "continue from position X" semantics, returning 500-entry allBuf buffers
- NULL filtering for getRows (the 320x regression fix) stays in the TS caller — it's a logical filter on which keys to query, not a SQLite concern
- The `v.parse(value, schema)` validation on change log entries could move to Rust for the changesSince path

</specifics>

<deferred>
## Deferred Ideas

- **Full diff in Rust** — If individual methods prove insufficient after benchmarking, could move entire Diff iterator to Rust in a later pass.
- **ChangeProcessor backfill optimization** — If profiling shows backfill is slow, add queryBatched pattern for getLatestRowOp loop.
- **fromSQLiteTypes TS export removal** — Once all consumers use Rust methods with inline conversion, the TS export can be deprecated.

</deferred>

---

*Phase: 05-high-impact-services*
*Context gathered: 2026-04-20*
