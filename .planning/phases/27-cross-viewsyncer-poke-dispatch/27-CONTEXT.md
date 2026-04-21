# Phase 27: Cross-ViewSyncer Poke Dispatch - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

When a Postgres change arrives, process ALL affected ViewSyncer instances in parallel in a single Rust call, instead of each ViewSyncer processing sequentially on the Node event loop. This replaces the current per-ViewSyncer `#advancePipelines` loop with a batched `rust_dispatch_poke()` call that fans out across Rayon threads.

</domain>

<decisions>
## Implementation Decisions

### NAPI Function Signature

- **D-01:** New `rust_dispatch_poke(change, viewsyncer_pipelines: Vec<(vs_id, pipelines)>) -> Vec<(vs_id, Vec<RowChange>)>` napi function in `zqlite-rs`
- **D-02:** Input is a batch of ViewSyncer pipeline configs keyed by VS ID; output maps results back per VS ID
- **D-03:** Reuse existing binary serialization format from Phase 25 (`decodeAdvanceResultBuf`) for result encoding

### Integration Point

- **D-04:** The `version-ready` handler in `view-syncer.ts` `run()` loop currently calls `#advancePipelines` per ViewSyncer. Phase 27 introduces a coordinator that batches all pending ViewSyncer advances into a single Rust call
- **D-05:** Each ViewSyncer still owns its own CVR update and poke delivery — only the IVM computation is batched in Rust
- **D-06:** Feature flag `ZERO_DISABLE_RUST_DISPATCH` to fall back to per-VS sequential advance (same pattern as `ZERO_DISABLE_RUST_HYDRATION`)

### timeSliceQueue Elimination

- **D-07:** The global `timeSliceQueue` Lock (line 2435 of view-syncer.ts) becomes unnecessary for the advance path when Rust dispatch is active — CPU work moves off the main thread
- **D-08:** Keep `timeSliceQueue` for the hydration path (not yet moved to batch Rust dispatch)
- **D-09:** When `ZERO_DISABLE_RUST_DISPATCH` is set, `timeSliceQueue` remains active for the advance path

### Parallelism Strategy

- **D-10:** All ViewSyncer pipeline evaluations run on Rayon thread pool — same pool used by Phase 22/24 hydration and advance
- **D-11:** Each VS gets its own SQLite read connection from the pool (WAL snapshot pinning from Phase 21)
- **D-12:** Results are binary-serialized per-VS and returned as a flat buffer; TS side partitions by VS ID

### Error Handling

- **D-13:** If any single VS pipeline panics or errors in Rust, return an error variant for that VS only — don't fail the entire batch
- **D-14:** TS side handles per-VS errors the same way as current `ResetPipelinesSignal` — reset that VS's pipelines

### Claude's Discretion

- Exact batching/coordination mechanism in TS (how pending VS advances are collected)
- Connection pool sizing for concurrent VS dispatch
- Whether to use a dedicated napi thread or Rayon's existing pool

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Existing Advance Path

- `packages/zero-cache/src/services/view-syncer/view-syncer.ts` — `run()` loop (line 442), `#advancePipelines`, `timeSliceQueue` (line 2435)
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — `#rustAdvance()` (line 988), `#advance()` (line 803), pipeline config extraction

### Rust Infrastructure

- `packages/zqlite-rs/index.d.ts` — existing napi type declarations (`rustAdvance`, `rustHydrate`)
- `packages/zqlite-rs/src/lib.rs` — napi function implementations
- `packages/zero-ivm-rs/src/lib.rs` — Operator trait, Rayon fan-out, connection pool

### Binary Serialization

- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` — result decoder from Phase 25

### Prior Phase Context

- `.planning/phases/26-pipeline-driver-ts-integration/26-01-PLAN.md` — Rust hydration integration pattern
- `.planning/phases/26-pipeline-driver-ts-integration/26-02-PLAN.md` — Rust advance integration pattern

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `rustAdvanceFn` in pipeline-driver.ts: per-pipeline Rust advance already works — Phase 27 lifts this to cross-VS level
- `decodeAdvanceResultBuf`: binary decoder for advance results — reuse for dispatch results
- `RustStorage`, `RustTakeStorage`: Rust-backed storage for pipeline state
- `timeSliceQueue` / `yieldProcess`: cooperative yielding mechanism to replace

### Established Patterns

- Feature flag pattern: `ZERO_DISABLE_RUST_HYDRATION` env var (Phase 26) — replicate as `ZERO_DISABLE_RUST_DISPATCH`
- Pipeline config extraction: `#extractPipelineConfig()` builds JSON config per pipeline — reuse for batch config
- `ResetPipelinesSignal`: error-as-signal pattern for pipeline resets

### Integration Points

- `view-syncer.ts` `run()` loop — the `version-ready` handler that calls `#advancePipelines`
- `pipeline-driver.ts` `processTransaction()` — called by `#advancePipelines`, returns changes generator
- CVR update + poke delivery — happens after advance results, remains in TS

</code_context>

<specifics>
## Specific Ideas

No specific requirements — open to standard approaches. Follow the same integration pattern established in Phase 26 (feature flag, fallback to TS, binary serialization).

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope.

</deferred>

---

_Phase: 27-cross-viewsyncer-poke-dispatch_
_Context gathered: 2026-04-21_
