# Phase 3: IVM Data Layer - Context

**Gathered:** 2026-04-20
**Status:** Ready for planning

<domain>
## Phase Boundary

Add batch row conversion functions to `packages/zqlite-rs/` Rust crate that TableSource's hot paths call instead of per-row JS iteration. DatabaseStorage stays in TypeScript. `fromSQLiteTypes`/`toSQLiteTypes` stay in TypeScript (deferred to Phase 5). Test suites must pass unchanged.

</domain>

<decisions>
## Implementation Decisions

### TableSource Rust Boundary

- **D-09:** Hot-path only. TableSource class stays in TypeScript. Only the inner row-iteration loops move to Rust. Generators, overlay, connect/push, Source interface — all stay TS. TS calls Rust for batch row conversion. Tests pass easily since the class structure is unchanged.

### Batch Row Conversion API

- **D-10:** Dual-mode API. Rust exposes two functions:
  1. `queryAll(sql, params, columnTypes)` → JS array of objects. One FFI call, Rust does all row iteration + type conversion. For queries with LIMIT or known-small results.
  2. `queryBatched(sql, params, columnTypes, batchSize)` → yields JS arrays of N rows. TS `#fetch` uses this with `batchSize=500`. One FFI call per 500 rows instead of per row. Streaming semantics preserved, no full-result buffering. Batch size tunable.
- Column type metadata passed to Rust so it can do `fromSQLiteTypes` conversion (boolean→int, bigint→number, json→parse) inside Rust.

### DatabaseStorage Scope

- **D-11:** DatabaseStorage stays in TypeScript. It's only 187 lines and already benefits from Phase 1's Rust Database underneath. Same reasoning as D-08 (StatementRunner). No Rust rewrite needed.

### Type Converter Location

- **D-12:** `fromSQLiteTypes`/`toSQLiteTypes` stay as TS exports for now. They're used by snapshotter, change-processor, and other modules not yet in scope. Moving them now creates FFI calls in code that's about to be rewritten in Phase 5. Defer to Phase 5 when all consumers are in scope — do it once, cleanly.

### Write Path

- **D-13:** `#writeChange` (INSERT/DELETE/UPDATE) stays in TS for Phase 3. The write path is not the bottleneck — it's single-row operations already handled efficiently by Phase 1's Rust Database. The read/fetch path is where batch wins matter.

### Claude's Discretion

- Internal Rust function signatures and parameter encoding
- Batch size default (500 suggested, tunable)
- How column type metadata is passed (JSON schema object vs enum array)
- Rust unit test structure for the new batch functions

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Source Files (modification targets)

- `packages/zqlite/src/table-source.ts` — TableSource class, `#fetch`, `#mapFromSQLiteTypes` (685 lines)
- `packages/zqlite/src/query-builder.ts` — `buildSelectQuery`, `toSQLiteType`, `fromSQLiteTypes` helpers (283 lines)
- `packages/zqlite/src/database-storage.ts` — stays TS, no changes (187 lines)

### Rust Crate (extend)

- `packages/zqlite-rs/src/database.rs` — Rust Database class to add queryAll/queryBatched methods
- `packages/zqlite-rs/src/types.rs` — type conversion logic to extend with column-type-aware conversion

### Test Files (correctness gates)

- `packages/zqlite/src/table-source.test.ts` — 1030 lines, MUST pass unchanged
- `packages/zqlite/src/database-storage.test.ts` — 212 lines, no changes expected

### IVM Interface Files (understand contracts)

- `packages/zql/src/ivm/operator.ts` — Storage interface, FetchRequest, Start types
- `packages/zql/src/ivm/memory-source.ts` — generateWithOverlay, Connection, Overlay types
- `packages/zql/src/ivm/source.ts` — Source, SourceChange, SourceInput interfaces
- `packages/zql/src/ivm/stream.ts` — Stream type (generator-based)
- `packages/zql/src/ivm/data.ts` — Node type, makeComparator

### Phase 1 Context

- `.planning/phases/01-rust-foundation/01-CONTEXT.md` — Prior decisions D-01 through D-08

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `packages/zqlite-rs/src/database.rs` — Rust Database already has `prepare`, `exec`, `run` methods. `queryAll`/`queryBatched` build on this.
- `packages/zqlite-rs/src/types.rs` — `SqliteValue`, `row_to_js_object`, `js_array_params_to_sqlite` already exist from Phase 1. Extend with column-type-aware conversion.
- `packages/zqlite/src/internal/sql.ts` — `compile()`, `format()` produce SQL strings + bind values. Rust receives these as input.

### Established Patterns

- **StatementCache:** TS cache wraps Rust Database.prepare(). `queryAll`/`queryBatched` bypass statement cache since they manage their own prepared statements internally in Rust.
- **safeIntegers(true):** TableSource enables this before iteration. Rust must handle bigint values in the same way.
- **Generator yield pattern:** `#fetch` yields `Node | 'yield'`. The 'yield' sentinel comes from `shouldYield()`. This stays in TS — Rust returns row batches, TS wraps them in the generator pattern.

### Integration Points

- `table-source.ts` line 291: `cachedStatement.statement.iterate<Row>()` — this is what gets replaced by Rust batch call
- `table-source.ts` line 364: `#mapFromSQLiteTypes()` — this generator gets replaced by consuming Rust batch results
- `table-source.ts` line 510: `getRow()` uses `cache.use()` then `fromSQLiteTypes` — could use `queryAll` for single-row fetch

</code_context>

<specifics>
## Specific Ideas

- Batch size of 500 rows per FFI call is the starting point. Tunable via parameter, not hardcoded.
- `queryBatched` returns an iterator-like object in JS that TS can `for...of` over, each iteration returning a batch array.
- Column type metadata format: `{columnName: "boolean" | "number" | "string" | "null" | "json"}` — Rust parses this once per query.
- The `timeSampled` wrapper in `#mapFromSQLiteTypes` can wrap the batch call instead of individual row reads.

</specifics>

<deferred>
## Deferred Ideas

- **fromSQLiteTypes/toSQLiteTypes in Rust** — Deferred to Phase 5 when snapshotter and change-processor are being converted. Do it once with all consumers in scope.
- **Write path (#writeChange) in Rust** — Not a bottleneck. Could batch writes later if profiling shows need.
- **Full TableSource in Rust** — Would require Rust generators compatible with JS Stream type. Too complex for the gain. Revisit if Phase 3 benchmarks show TS generator overhead is significant.

</deferred>

---

_Phase: 03-ivm-data-layer_
_Context gathered: 2026-04-20_
