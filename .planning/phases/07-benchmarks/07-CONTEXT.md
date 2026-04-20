# Phase 7: Benchmarks & Validation - Context

**Gathered:** 2026-04-20
**Status:** Ready for planning

<domain>
## Phase Boundary

Extend the existing benchmark suite with a realistic view-syncer diff simulation, add regression validation (assert gates + timed integration test), and document all results in a markdown report. GC pause reduction is documented theoretically (buffer protocol reduces JS object allocation) but not measured directly.

</domain>

<decisions>
## Implementation Decisions

### Workload Profiles
- **D-22:** Add a view-syncer diff simulation benchmark: changesSinceBuf -> getRowsMultiBuf -> decode loop. This measures the actual snapshotter hot path end-to-end as a composite operation, not isolated micro-ops.
- No concurrent reader or mixed read/write benchmarks needed — focus on the proven hot path.

### Regression Validation
- **D-23:** Two-pronged regression validation:
  1. Extend `rust-vs-ts.cjs --assert` with gates on all key operations (existing pattern)
  2. Add a timed comparison run of `snapshotter.test.ts` (Rust-backed vs better-sqlite3) to catch unexpected end-to-end slowdowns
- Both must pass for Phase 7 to be complete.

### GC Pause Measurement
- **D-24:** Skip direct GC measurement. Document the theory: allBuf returns a single Buffer instead of N JS objects, reducing GC pressure proportionally to result set size. The 1.7-2x speedup on bulk reads partially reflects this.

### Results Documentation
- **D-25:** Markdown report in `.planning/phases/07-benchmarks/` with:
  - Operation table (operation, Rust time, TS time, speedup ratio, notes)
  - View-syncer diff simulation results
  - Regression test outcomes
  - Summary paragraph with conclusions

### Claude's Discretion
- Exact benchmark parameters (row counts, iteration counts, warmup)
- How to structure the timed snapshotter comparison (wrapper script vs vitest reporter)
- Whether to add new assert gates beyond existing allBuf thresholds

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Existing Benchmark
- `packages/zqlite-rs/bench/rust-vs-ts.cjs` — 670-line comprehensive benchmark with 6 sections and `--assert` mode

### Snapshotter (integration target)
- `packages/zero-cache/src/services/view-syncer/snapshotter.ts` — Rust-integrated read methods
- `packages/zero-cache/src/services/view-syncer/snapshotter.test.ts` — 10 tests, timing comparison target

### Rust Crate
- `packages/zqlite-rs/src/database.rs` — getRow, getRowsBuf, changesSinceBuf, getRowsMultiBuf, queryAll
- `packages/zqlite/src/db.ts` — decodeBuf, allBuf wrappers

### Prior Phase Context
- `.planning/phases/05-high-impact-services/05-CONTEXT.md` — D-15 through D-18
- `.planning/phases/06-bulk-loader/06-CONTEXT.md` — D-19 through D-21, benchmark methodology

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `rust-vs-ts.cjs` bench() function: warmup, iteration timing, p50/p95/p99 stats
- decodeBuf() already inline in benchmark for buffer protocol testing
- `--assert` flag infrastructure for CI regression gates

### Established Patterns
- Benchmark creates temp SQLite DB, seeds with N rows, compares both backends
- Uses process.hrtime() for microsecond precision
- Reports median and percentile stats

### Integration Points
- Snapshotter test file can be run with timing via vitest's `--reporter` or a wrapper
- rust-vs-ts.cjs already loads both backends side-by-side

</code_context>

<specifics>
## Specific Ideas

- View-syncer diff simulation should mirror the actual Diff iterator pattern: get changes since version X, then fetch full rows for each changed key
- Timed snapshotter comparison: run the test suite twice (once with Rust, once with better-sqlite3 fallback) and compare wall-clock times

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 07-benchmarks*
*Context gathered: 2026-04-20*
