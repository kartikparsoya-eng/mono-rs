# Phase 14: Integration + Edge Case Testing — Research

## Summary

The codebase has strong foundations for this phase: an existing benchmark script (`parallel-fanout-bench.ts`) that can be adapted, a 30+ test suite in `pipeline-driver.test.ts` that validates Rust/TS correctness, and a well-defined `rust_advance()` napi interface with JSON-based data exchange. The main work is formalizing benchmarks with pipeline count sweeps, adding edge case tests for data types and error paths, writing documentation, and gating the v2.0 tag on 4x throughput + byte-identical correctness.

## 1. Existing Benchmark Infrastructure

**`parallel-fanout-bench.ts`** (314 lines, untracked) compares sequential PipelineDriver (50 pipelines) vs Worker Threads parallel approach. Key characteristics:

- **Schema:** `issues` (id, closed) + `comments` (id, issueID, upvotes) — 2 tables
- **Workload:** 500 seeded rows, 100 inserts across both tables
- **Query mix:** 4 templates — JOIN, Filter, LIMIT, plain scan — cycled across pipelines
- **Measurement:** `performance.now()` around `pipelines.advance()`, 5 iterations, reports avg speedup
- **DB setup:** WAL2 journal mode, `initReplicationState`, `populateFromExistingTables`, `fakeReplicator.processTransaction`

**Reusable:** DB setup helper (`setupDb`), query templates, `ReplicationMessages` pattern, `NO_TIME_ADVANCEMENT_TIMER` constant. **Gaps:** No pipeline count sweep (fixed at 50), no Rust vs TS A/B toggle (uses Worker Threads not Rust advance), no diff size variation, no CI-friendly output format.

## 2. rust_advance() Interface

**napi signature** (advance.rs:359-367):
```rust
#[napi]
pub fn rust_advance(
    db_path: String,
    prev_version: String,
    curr_version: String,
    syncable_tables_json: String,    // JSON: HashMap<String, TableAndZqlSpec>
    all_table_names_json: String,    // JSON: string[]
    permissions_table: String,       // typically ""
    pipeline_configs_json: String,   // JSON: PipelineConfig[]
) -> napi::Result<String>  // returns JSON: AdvanceResult
```

**Return type** (`AdvanceResult`): `{ changes: RowChange[], error?: string, error_type?: string }`

**RowChange fields:** `queryID`, `table`, `row_key` (JSON), `row` (JSON | null), `type` ("add"|"edit"|"remove")

**How TS calls it** (pipeline-driver.ts:843-937): `#rustAdvance()` calls `snapshotter.advanceWithoutDiff()` to get prev/curr versions, serializes `#tableSpecs` and `#pipelineConfigs` to JSON, calls `rustAdvanceFn()`, parses result, handles `version_mismatch` with retry, `reset`/`truncate` with `ResetPipelinesSignal`.

**For benchmarks:** Can call `rustAdvanceFn` directly after setting up a DB with known data and changelog entries, or go through `PipelineDriver.advance()` which routes to Rust automatically when `USE_RUST_ADVANCE` is true and all pipelines are eligible.

## 3. Database Schema for Benchmarks

**`_zero.changeLog2` schema** (from diff.rs:80-84):
```sql
CREATE TABLE "_zero.changeLog2" (
    stateVersion TEXT,
    "table" TEXT,
    rowKey TEXT,     -- JSON string
    op TEXT,         -- 's' (set), 'd' (delete), 'r' (reset), 't' (truncate)
    pos INTEGER      -- ordering within a version
);
```

**Populating synthetic data:** The `fakeReplicator.processTransaction(version, ...messages)` utility handles inserting into both the data tables and `_zero.changeLog2`. Used in both tests and existing bench. For direct Rust benchmarks, could populate directly via SQL.

**WAL mode:** DB must use `journal_mode = wal2` (set in both test and bench setup). Rust opens two read-only connections with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` and uses `BEGIN DEFERRED` for snapshot isolation.

**Supporting tables:** `_zero.replicationState` (version tracking), `_zero.tableMetadata` (minRowVersion), `_zero.column_metadata` (backfill state).

## 4. Correctness Validation Pattern

**pipeline-driver.test.ts pattern** (2332 lines, 30+ tests):
- Setup: `PipelineDriver` + `DbFile` + `fakeReplicator` in `beforeEach`
- Hydrate: `pipelines.addQuery(hash, queryID, ast, timer)` -> collect `RowChange[]`
- Advance: `replicator.processTransaction(version, ...messages)` then `[...pipelines.advance(timer).changes]`
- Assert: `toMatchInlineSnapshot()` on full RowChange arrays (type, queryID, table, rowKey, row)

**Byte-identical comparison (D-69):** Run same workload through both paths:
1. Set `ZERO_DISABLE_RUST_IVM=1` -> collect TS output
2. Unset -> collect Rust output
3. Deep-compare serialized RowChange arrays

**Key insight:** The `#convertRustChanges()` method (pipeline-driver.ts:939-963) maps Rust's string-based change types to TS `ChangeType` enum and constructs identical `RowChange` objects. Comparison should serialize both outputs to JSON and diff.

## 5. Fallback/Error Paths

**What triggers TS fallback:**
- `ZERO_DISABLE_RUST_IVM=1` env var -> all Rust paths disabled (line 69)
- `rustAdvanceFn` undefined (native module not built) -> `USE_RUST_ADVANCE = false` (line 72)
- Pipeline not eligible: has `related` (joins), `limit`, companions, or correlated subqueries -> `#extractPipelineConfig` returns null (lines 810-813)
- Not all pipelines eligible: `#reevaluateRustAdvance()` requires `pipelineConfigs.size === pipelines.size` (line 836)
- Rust advance throws non-ResetPipelinesSignal error -> catches, logs warning, sets `#useRustAdvance = false` (lines 687-691)

**Error types from Rust:**
- `version_mismatch` -> retry once with fresh snapshot (lines 885-913)
- `reset` / `truncate` -> throw `ResetPipelinesSignal` (lines 915-920)
- Generic error -> throw `Error` (lines 922-924), caught by fallback handler

**Panic recovery:** Rust panics in napi-rs become JS exceptions. The catch block at line 687 handles this -> falls back to TS. No explicit panic hook testing exists yet.

**SQLite busy/locked:** Rust opens read-only connections with `SQLITE_OPEN_NO_MUTEX`. If busy, rusqlite returns an error which propagates through `DiffError::Unknown` -> Rust returns `error_type: "unknown"` -> TS throws generic error -> fallback.

## 6. Data Type Serialization

**Row type:** `HashMap<String, serde_json::Value>` in Rust (diff.rs:6), `Row` (Record<string, JSONValue>) in TS.

**Value mapping** (diff.rs:414-432, `sqlite_value_to_json`):
| SQLite Type | JSON Type |
|---|---|
| NULL | null |
| Integer | number |
| Real | number (f64) |
| Text | string |
| Blob | string (base64) |

**Type conversions** (diff.rs:232-255, `from_sqlite_types`):
- `boolean` columns: integer 0/1 -> JSON bool
- `json` columns: string -> parsed JSON object

**Edge cases to test (D-75):**
- NULL values across all column types
- Empty string vs NULL
- Unicode strings (emoji, CJK, combining characters)
- Very long strings (> 64KB)
- `MAX_SAFE_INTEGER` boundary -- test already exists (pipeline-driver.test.ts:1884-1905) showing overflow throws
- Blob/real/integer type coercion
- Boolean 0/1 <-> true/false conversion fidelity

**Predicate Value enum** (advance.rs:64-70): `Null | Bool(bool) | Number(f64) | String(String)` -- no blob support in predicates. `from_json` maps arrays/objects to `Null` (line 79), which could silently drop complex values.

## 7. Performance Measurement Approach

**Rust vs TS isolation:** Toggle `ZERO_DISABLE_RUST_IVM=1` env var for A/B comparison. Both paths go through `PipelineDriver.advance()` so setup is identical.

**Pipeline count sweep (D-67):** [1, 2, 4, 8, 16, 32, 64] pipelines. For each count:
1. Create PipelineDriver, add N queries (cycling through templates)
2. `processTransaction` with fixed diff
3. Time `advance()` with Rust enabled and disabled
4. Report speedup ratio

**Diff size sweep:** Vary `NUM_INSERTS` (1, 10, 100, 500, 1000+) while holding pipeline count constant.

**Rayon scaling:** Rust uses `par_iter()` over pipelines (advance.rs:467-470). Speedup comes from parallel pipeline processing, not parallel diff reading. With 1 pipeline, expect ~1x; with 64, expect near-linear scaling up to core count.

**Timing approach:** `performance.now()` around `advance()` call. For microbench, run multiple iterations and report median. For CI regression, set threshold assertions.

## 8. Recommendations for Planning

**Plan structure -- 3 plans recommended:**

1. **Plan 14-01: Benchmark Suite** -- Formalize `parallel-fanout-bench.ts` into a proper benchmark with:
   - Pipeline count sweep [1, 2, 4, 8, 16, 32, 64]
   - Diff size sweep [1, 10, 100, 500, 1000]
   - Rust vs TS A/B via env var toggle
   - CI-friendly output (JSON results, pass/fail on 4x threshold)
   - Realistic workload variant with multi-table mixed queries

2. **Plan 14-02: Edge Case Tests** -- Four categories per D-70:
   - **Diff size extremes** (D-72): Rust unit tests in advance.rs + TS integration via pipeline-driver pattern
   - **Pipeline topology** (D-73): Mixed eligible/ineligible, hot-swap via env var, add/remove during advance
   - **Error paths** (D-74): Force version mismatch, simulate SQLite busy, trigger Rust panic, verify graceful fallback
   - **Data types** (D-75): NULL, empty string, unicode, long values, blob, real, integer, boolean boundaries
   - Use existing pipeline-driver.test.ts patterns; add new test file `pipeline-driver.edge-cases.test.ts`

3. **Plan 14-03: Documentation + v2.0 Tag** -- Per D-76:
   - Architecture overview: Rust/TS integration diagram, operator boundary, data flow, fallback mechanisms
   - Ops/migration guide: enable/disable env vars, build prereqs (`cargo`, napi-rs), troubleshooting
   - Tag `mono-rs/v2.0` after gates pass

**Key dependencies:**
- Benchmarks need a built `zqlite-rs` native module -- ensure `cargo build` in CI
- Edge case tests for Rust unit tests need `cargo test` in zqlite-rs
- TS integration tests run via `vitest` with existing infrastructure

**Risk areas:**
- Rust advance currently only supports filter-only pipelines (no joins/limit/exists in `#extractPipelineConfig`). The 4x bar may be hard to hit if benchmark workloads include joins. Recommend testing filter-only pipelines for the throughput gate, and mixed pipelines for correctness.
- The predicate AST in advance.rs reimplements filter logic separately from zero-ivm-rs (`cdylib can't cross-link` comment at line 62). Predicate format mismatch between TS AST `Condition` and Rust `Predicate::from_json` is a correctness risk -- the TS `#extractPipelineConfig` passes `ast.where` (which uses `{type: 'simple', left: {type: 'column', name}, op: '=', right: {type: 'literal', value}}`) but Rust expects `{op: 'eq', field, value}`. This format mismatch means **filter predicates may not actually evaluate correctly in Rust advance**. Edge case tests must verify this.

## RESEARCH COMPLETE
