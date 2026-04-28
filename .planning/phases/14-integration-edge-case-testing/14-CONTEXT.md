# Phase 14: Integration + Edge Case Testing - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

End-to-end validation of the Rust IVM pipeline: benchmark suite proving performance, edge case coverage ensuring correctness, architecture documentation, and v2.0 tagging. This phase does NOT add new Rust operators or modify existing ones — it validates what Phases 10-13 built.

</domain>

<decisions>
## Implementation Decisions

### Benchmark Suite

- **D-66:** Two benchmark types: microbench (synthetic, fixed pipelines/diff sizes for CI regression) + realistic workload replay (multi-table, mixed query types for headline numbers)
- **D-67:** Pipeline count sweep: 1, 2, 4, 8, 16, 32, 64 pipelines to show Rayon scaling curve
- **D-68:** Two-number performance bar. Hard minimum: 4x throughput on 50-pipeline workload with 8 cores — below this, v2.0 does not ship. Success target: 6x — matches theoretical Amdahl prediction.
- **D-69:** Non-negotiable correctness gate: RowChange output must be byte-identical to TS advance() on all benchmark workloads. Fast and wrong does not ship.

### Edge Case Coverage

- **D-70:** All four edge case categories required: diff size extremes, pipeline topology changes, error/fallback paths, data type edge cases
- **D-71:** Testing method: both Rust unit tests (cargo test in zqlite-rs) for internal logic + TS integration harness comparing Rust vs TS advance() output end-to-end
- **D-72:** Diff size extremes: 1000+ changes, empty diffs, single-row diffs, all-no-op diffs
- **D-73:** Pipeline topology: mixed eligible/ineligible, hot-swap Rust/TS mid-session, add/remove during advance
- **D-74:** Error paths: Rust panic recovery, SQLite busy/locked, version mismatch retry, graceful TS fallback
- **D-75:** Data types: NULL, empty string, unicode, very long values, blob/real/integer/text

### Documentation & Tagging

- **D-76:** Produce architecture overview (Rust/TS integration, data flow, operator boundary, fallback mechanisms) and ops/migration guide (enable/disable, env vars, build prereqs, troubleshooting)
- **D-77:** Performance results NOT required as separate doc — captured in benchmark output
- **D-78:** Tag `mono-rs/v2.0` after benchmarks pass 4x bar + all edge case tests green. Docs can follow the tag.

### Claude's Discretion

- Benchmark framework choice (vitest bench, custom harness, or standalone script)
- Specific diff sizes in the sweep beyond the stated extremes
- Documentation format (markdown in repo root vs .planning/)

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Rust Crates

- `packages/zqlite-rs/src/advance.rs` — Rayon fan-out coordinator (the code being benchmarked)
- `packages/zqlite-rs/src/diff.rs` — Diff reader (upstream of advance)
- `packages/zero-ivm-rs/src/filter.rs` — Filter predicate evaluator used in pipeline processing

### Integration Layer

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — TS integration with Rust advance, fallback logic, pipeline config extraction

### Prior Phase Context

- `.planning/phases/13-rayon-parallelism-for-fan-out/13-CONTEXT.md` — Rayon design decisions
- `.planning/phases/13-rayon-parallelism-for-fan-out/13-01-PLAN.md` — Diff reader spec
- `.planning/phases/13-rayon-parallelism-for-fan-out/13-02-PLAN.md` — Advance coordinator spec

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `parallel-fanout-bench.ts` (untracked) — existing benchmark script from Phase 13 development, can be formalized
- `pipeline-driver.test.ts` — 30-test suite already validates correctness of Rust path integration
- `zqlite-rs/src/advance.rs` tests — 9 unit tests covering basic advance scenarios

### Established Patterns

- Env var `ZERO_DISABLE_RUST_IVM=1` disables all Rust paths — benchmarks can toggle this for A/B comparison
- `USE_RUST_ADVANCE` flag in pipeline-driver.ts controls advance-level Rust usage
- `#reevaluateRustAdvance()` determines eligibility per pipeline topology

### Integration Points

- Benchmark harness needs a real SQLite database with `_zero.changeLog2` populated
- TS integration harness connects at `pipeline-driver.ts` level — calls advance() and compares output
- Tag creation via `git tag mono-rs/v2.0` after all gates pass

</code_context>

<specifics>
## Specific Ideas

- 4x minimum derived from Amdahl's Law analysis: sequential floor ~4ms, parallel portion ~500ms -> ~80ms on 8 cores = ~6x theoretical max
- If below 4x, investigate: WAL mode not enabling concurrent reads, unexpected serialization points, Rayon thread pool contention
- Correctness comparison: serialize RowChange output from both paths, diff byte-by-byte

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

_Phase: 14-integration-edge-case-testing_
_Context gathered: 2026-04-21_
