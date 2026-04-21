# Phase 26: Pipeline-Driver TS Integration - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Modify pipeline-driver.ts and view-syncer.ts to use the Rust hydration and advance paths built in Phases 20-25. This is a deletion-heavy phase — removing JS pipeline logic that Rust now owns. TS becomes pure orchestration/I/O glue.

Requirements: INT-01, INT-02, INT-03

</domain>

<decisions>
## Implementation Decisions

### Hydration Integration (INT-01)

- **D-01:** Replace `buildPipeline()` + `input.fetch()` in `addQuery()` with a single Rust hydration call. The current flow (pipeline-driver.ts:497-691) builds a TS IVM pipeline, hydrates via `input.fetch({})`, and wraps in a Streamer. Phase 26 replaces this with `rust_hydrate()` which builds the operator tree and fetches entirely in Rust.
- **D-02:** Scalar subquery resolution (`#resolveScalarSubqueries()`) should remain in TS initially — it's orchestration logic that determines companion pipeline setup. Rust hydrate receives the resolved AST.
- **D-03:** The result from Rust hydration must produce `Iterable<RowChange | 'yield'>` to maintain compatibility with `view-syncer.ts#processChanges()` (line 2077). Rust returns pre-computed RowChanges; TS wraps them into the expected iterable.

### Advance Integration (INT-02)

- **D-04:** Expand `#rustAdvance()` to handle ALL query types — not just filter-only configs. Phase 24 built the full operator tree advance in Rust. The current gating in `#extractPipelineConfig()` (line 867) that rejects joins/limits/companions must be removed or expanded.
- **D-05:** Keep the existing two-step approach: TS snapshotter produces the diff (correct two-snapshot isolation), then Rust does parallel fan-out over full operator trees. This preserves the diff correctness guarantee.
- **D-06:** `#dualExecAdvance()` must be updated to compare the new full-tree Rust advance against TS. This is critical for correctness validation.

### Feature Flag (INT-03)

- **D-07:** Add `ZERO_DISABLE_RUST_HYDRATION` env var (default: enabled). When set to `'1'`, falls back to the existing TS `buildPipeline()` + `input.fetch()` path. This parallels the existing `ZERO_DISABLE_RUST_IVM` pattern.
- **D-08:** The existing `ZERO_DISABLE_RUST_IVM` flag should disable BOTH hydration and advance Rust paths (umbrella flag). `ZERO_DISABLE_RUST_HYDRATION` is a more granular override for hydration-only fallback.

### Serialization Format

- **D-09:** Keep JSON serialization for the advance fan-out path initially (matching existing `rustFanOut()` interface). Binary optimization can follow in Phase 28 benchmarking.
- **D-10:** For hydration results, use the binary buffer protocol from Phase 25 (`decode-advance-buf.ts` style) since hydration returns potentially large row sets where JSON overhead matters.

### TableSource Fate

- **D-11:** Keep TableSource as a thin wrapper — it still owns `setDB()` for snapshot switching, overlay management for push consistency, and the connection management to IVM operator graph edges. Rust replaces the SQLite read path but TS TableSource manages the lifecycle.

### Claude's Discretion

- Generator-to-array conversion strategy for bridging Rust results into the existing `Iterable<RowChange | 'yield'>` contract
- Error handling and fallback behavior when Rust hydration/advance fails at runtime
- How to structure the `RustPipelineConfig` expansion to include join/take/exists metadata

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Core Integration Targets

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — Main file being modified. Key methods: `addQuery()` (L497), `#advance()` (L771), `#rustAdvance()` (L913), `#extractPipelineConfig()` (L867)
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts` — Consumer of pipeline-driver output. Key: `#addAndRemoveQueries()` (L1843), `#processChanges()` (L2077), `#advancePipelines()` (L2172)

### Rust NAPI Layer

- `packages/zqlite-rs/index.d.ts` — Current Rust exports: `rustAdvance()`, `rustFanOut()`, Database/Statement classes
- `packages/zqlite-rs/src/connection_pool.rs` — Connection pool for parallel reads
- `packages/zero-ivm-rs/index.d.ts` — Rust IVM operator exports (RustStorage, RustFilterPredicate, etc.)

### Supporting Files

- `packages/zqlite/src/table-source.ts` — TableSource implementation being partially replaced
- `packages/zero-cache/src/services/view-syncer/dual-executor.ts` — Dual-exec comparator for correctness validation
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — Primary test suite (28 pass, 2 pre-existing failures)
- `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — Property-based fuzz tests

### Existing Patterns

- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` — Binary buffer decoder (Phase 25 serialization)
- `packages/zero-cache/src/services/view-syncer/rust-exists.ts` — Example of Rust operator TS wrapper pattern
- `packages/zero-cache/src/services/view-syncer/rust-join.ts` — Example of Rust operator TS wrapper pattern

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `rustFanOut()` napi function — already handles parallel fan-out, needs expansion for full operator tree configs
- `RustPipelineConfig` type (pipeline-driver.ts:80-94) — JSON shape for Rust fan-out, needs extension
- `decode-advance-buf.ts` — Binary decoder for Rust→TS row transfer
- `dual-executor.ts` — Comparator for TS vs Rust correctness validation
- `RustStorage`, `RustTakeState`, `RustFilterPredicate` — Rust-backed IVM operator storage already in use

### Established Patterns

- Dynamic `require('zqlite-rs')` at module load (pipeline-driver.ts:68-78) — graceful fallback when native module unavailable
- Feature flag hierarchy: `ZERO_DISABLE_RUST_IVM` → `USE_RUST_IVM` → `USE_RUST_JOIN/EXISTS/ADVANCE` (pipeline-driver.ts:96-100)
- Generator-based change streaming: `*addQuery()` and `*#advance()` yield `RowChange | 'yield'` for time-slicing
- `#reevaluateRustAdvance()` — re-checks all pipelines when a new query is added/removed

### Integration Points

- `addQuery()` return type: `Iterable<RowChange | 'yield'>` consumed by view-syncer.ts `#processChanges()`
- `advance()` return type: `{version, numChanges, changes: Iterable<RowChange | 'yield'>}` consumed by view-syncer.ts `#advancePipelines()`
- `timeSliceQueue` (view-syncer.ts:2435) — global Lock for cooperative yielding, may become unnecessary for Rust-backed paths
- `#snapshotter.advance()` — produces SnapshotDiff, must remain as Rust uses TS-produced diffs

</code_context>

<specifics>
## Specific Ideas

No specific requirements — open to standard approaches. The phase goal is clear: replace TS pipeline build/hydrate/advance with Rust calls while maintaining the existing test suite as the correctness gate.

</specifics>

<deferred>
## Deferred Ideas

- Binary serialization for advance fan-out (optimize in Phase 28 benchmarking)
- Removing `timeSliceQueue` entirely (Phase 27 — Cross-ViewSyncer dispatch)
- Streaming decode for hydration results (Phase 28 if needed)

</deferred>

---

_Phase: 26-pipeline-driver-ts-integration_
_Context gathered: 2026-04-21_
