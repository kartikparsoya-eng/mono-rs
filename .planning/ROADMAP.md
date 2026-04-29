# Roadmap — v5.0 Streaming

> **Mandatory verification gate.** Every phase MUST pass these checks before commit:
>
> 1. `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.*.test.ts` — all existing tests pass unchanged
> 2. `npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — fuzz with 1k iterations
> 3. `cargo test` in `packages/zqlite-rs/` and `packages/zero-ivm-rs/`
> 4. **Hard constraint:** no signature change to existing buffered methods (`advance`, `advanceAsync`, `hydrate*`, `addQuery*`); no change to `encode_advance_result_buf` or `decodeAdvanceResultBuf` formats.

---

## Milestone Goal

Make v4.0's rayon parallelism investment visible to clients as reduced time-to-first-byte. Today, parallelism only reduces total server-side wall time — clients still wait for the slowest pipeline because everything is materialized into a single Buffer before returning to JS. Per-pipeline streaming changes that. Also fix two real bugs and three latent risks identified in `.planning/IVM-PORT-AUDIT.md`.

**Phases derive from:** `.planning/IVM-STREAMING-PLAN.md` (3-phase A/B-merged + C + D split) and `.planning/IVM-PORT-AUDIT.md` (4 audit fixes shipped first as a known-correct baseline).

---

## Phases

- [ ] **Phase 30: Audit Fixes** — Ship the 4 IVM port audit fixes on a known-correct baseline before building streaming on top.
- [ ] **Phase 31: Rust Streaming Primitives + TS Wrappers** — Additive Rust napi streaming methods + TS PipelineDriver wrappers + decoder; no consumer migrated yet.
- [ ] **Phase 32: View-Syncer Streaming Migration** — Production consumer flips to streaming; pokes fire as fast pipelines complete.
- [ ] **Phase 33: Performance Tuning + Benchmarks** — Bounded channel, TTFB benchmark, memory peak measurement.

---

## Phase Details

### Phase 30: Audit Fixes

**Goal:** Ship the 4 fixes from the IVM port audit so the streaming work in Phase 31 builds on a known-correct operator baseline. These are independent of streaming, low-risk, and unblock the rest of the milestone.

**Depends on:** —

**Requirements:** AUDIT-01, AUDIT-02, AUDIT-03, AUDIT-04

**Success Criteria** (what must be TRUE):

1. `LIKE 'Foo%'` matches only `'Foo...'`-prefixed rows in Rust IVM (case-sensitive); `LIKE 'foo%'` does not match `'Foo...'`. A regression test in `packages/zqlite-rs/` distinguishes the two.
2. Editing a column that is an `EXISTS` parent_field causes the source to emit `Remove + Add` (not `Edit`); a regression test confirms downstream output matches TS for both directions of the membership transition.
3. Framework-invariant violations in Join (parent/child edit changes relationship), Exists (re-entrancy), Take (duplicate primary key), and Cap (partition key change on edit) panic loudly via `assert!` in release builds — verified by a Rust unit test that triggers each invariant and confirms the panic message.
4. `ExistsOperator` / `OrExistsOperator` Edit handling with `or_predicate` set emits `Remove` when old row passed via or_predicate but new row no longer matches and child count is 0; emits `Add` in the inverse case; passes through when both sides match the same way. Verified by a unit test with all 4 transitions.
5. Full vitest suite (`pipeline-driver.*.test.ts` + `fuzz-ivm.test.ts` 1k iterations) and `cargo test` in both Rust crates pass after fixes are applied.

**Plans:** 2/4 plans executed

Plans:

- [x] 30-01-PLAN.md — AUDIT-01: LIKE case sensitivity fix in parse_predicate_json (Wave 1)
- [x] 30-02-PLAN.md — AUDIT-02: EXISTS parent_field added to collect_split_edit_keys (Wave 1)
- [ ] 30-03-PLAN.md — AUDIT-04: Exists/OrExists Edit-with-or_predicate 4-transition fix (Wave 2, depends on 30-02)
- [ ] 30-04-PLAN.md — AUDIT-03: Promote 5 framework-invariant debug_assert sites to assert (Wave 2)

---

### Phase 31: Rust Streaming Primitives + TS Wrappers

**Goal:** Add the full additive streaming surface — Rust `advance_streaming` / `hydrate_streaming` napi methods returning AsyncIterator-shaped chunks, plus TS PipelineDriver wrappers and a per-chunk decoder. Nothing in production code consumes these yet; existing buffered methods are byte-for-byte unchanged.

**Depends on:** Phase 30

**Requirements:** STREAM-01, STREAM-02, STREAM-03, STREAM-04, STREAM-05, STREAM-06, WRAP-01, WRAP-02, WRAP-03, WRAP-04, COMPAT-01, COMPAT-02, COMPAT-03, TEST-01, TEST-02, TEST-03, TEST-04, TEST-05

**Note on scope:** Phases A and B from `.planning/IVM-STREAMING-PLAN.md` are intentionally combined into one phase. The split is only meaningful if a TS consumer migrated between them — which doesn't happen until Phase 32. Combining keeps the additive-but-unconsumed streaming surface in one reviewable unit and lets TEST-04 (TS-vs-Rust parity test) ship alongside the code it covers.

**Success Criteria** (what must be TRUE):

1. `RustPipelineManager.advance_streaming(id, changesJson)` and `hydrate_streaming(id)` / `hydrate_query_streaming(id, queryId)` return an `AdvanceStream`/`HydrateStream` napi class whose `next()` resolves to per-pipeline chunks in completion order; `return_()` flips an `Arc<AtomicBool>` cancel flag that running rayon tasks observe between operator pushes and exit early. Verified by `TEST-01` (cancellation drops at most cancelled+1 chunks), `TEST-02` (companion scalar reset closes channel cleanly), `TEST-03` (panicking pipeline surfaces as `StreamItem::Error` while siblings complete).
2. `pipeline-driver.ts::advanceStreaming(timer, vsId?)` and `addQueriesStreaming(queries, timer)` exist alongside (not replacing) `advanceAsync` / `addQueriesAsync`, return `AsyncIterable<RowChange | 'yield'>`, surface `ResetPipelinesSignal('scalar-subquery')` / `ResetPipelinesSignal('advancement-timeout')` with the same throw shape as today's buffered path, and call `stream.return_()` on timeout. Companion-bearing queries fall back to the existing TS hydrate path via the same `rustEligible` check.
3. `decodeAdvanceChunkBuf(buf): DecodedRowChange[]` decodes the per-chunk format (header `[u32 count][u8 0]` + per-row tagged encoding); `decodeAdvanceResultBuf` is byte-for-byte unchanged. A TS test confirms parity: same input → buffered `advanceAsync` and `advanceStreaming` produce identical `RowChange` sets modulo cross-pipeline ordering (`TEST-04`).
4. `TEST-05` confirms `for await { break }` over `advanceStreaming.changes` calls Rust `stream.return_()` and Rust pipeline tasks stop work within one operator-push boundary.
5. Backwards compatibility: full vitest suite (`pipeline-driver.*.test.ts`, `fuzz-ivm.test.ts` 1k iterations, `decode-advance-buf.test.ts`) and `cargo test` in both Rust crates pass unchanged. Buffered method signatures (`advance`, `advanceAsync`, `hydrate*`, `addQuery*`, `addQueries*`) and the `encode_advance_result_buf` / `decodeAdvanceResultBuf` binary format are byte-for-byte identical to v4.0 (verified by diff against the v4.0 tag).

**Plans:** TBD

---

### Phase 32: View-Syncer Streaming Migration

**Goal:** Flip the production consumer (`view-syncer.ts`) from the buffered API to the streaming API so `pokers.pokePart` fires as fast pipelines complete instead of only at the end of the batch. CVR commit semantics, `pokers.end`, and `pokers.cancel` on reset are preserved exactly.

**Depends on:** Phase 31

**Requirements:** MIGRATE-01, MIGRATE-02, MIGRATE-03, MIGRATE-04

**Success Criteria** (what must be TRUE):

1. `view-syncer.ts::#advancePipelines` calls `advanceStreaming` (not `advanceAsync`); `#processChanges` consumes `AsyncIterable<RowChange | 'yield'>` via `for await of`. The signature change is internal to `view-syncer.ts` and does not leak to any other module.
2. `view-syncer.ts::#hydrateUnchangedQueries` (and any other batch-hydration callsite identified in Phase 32 planning) calls `addQueriesStreaming` instead of `addQueriesAsync`.
3. `pokers.pokePart` is observed firing mid-batch in a test scenario with N pipelines of varying completion times — verified by an instrumented test that records `pokePart` call timestamps relative to per-pipeline completion, asserting the first `pokePart` occurs before the slowest pipeline finishes.
4. CVR commit happens exactly once at the end of `#advancePipelines` after the full stream is consumed; `pokers.end(finalVersion)` fires exactly once after CVR commit; `pokers.cancel()` fires correctly when `ResetPipelinesSignal` is thrown mid-stream (verified by an integration test that injects a companion-scalar change mid-batch and asserts no in-flight changes are committed client-side).
5. Full view-syncer test suite passes; `pipeline-driver.*.test.ts`, `fuzz-ivm.test.ts` (1k iterations), and `cargo test` continue to pass unchanged from Phase 31.

**Plans:** TBD

---

### Phase 33: Performance Tuning + Benchmarks

**Goal:** Confirm the architectural promise of streaming — TTFB drops from `max(pipeline_time)` to `min(pipeline_time)` — and bound the channel so straggler pipelines don't accumulate unbounded chunks. Measure peak Rust heap to confirm streaming reduces it from `O(total_changes)` to `O(max_pipeline_changes)`.

**Depends on:** Phase 32

**Requirements:** PERF-01, PERF-02, PERF-03

**Success Criteria** (what must be TRUE):

1. The Rust streaming channel is `mpsc::sync_channel(pipeline_count)` (bounded), so each pipeline can buffer at most one chunk before the receiver pulls; verified by a Rust unit test that fills the channel and confirms producer pipelines block until JS pulls.
2. A microbenchmark (extending `rust-ivm-bench.ts` or new file) configures N pipelines with one slow tail pipeline and asserts that time-to-first-chunk is within ~1.5x of `min(pipeline_time)` (not `max`). Result is recorded in the benchmark output for trend tracking.
3. Peak Rust heap during a representative advance/hydrate batch is measured (via `jemalloc-stats` or equivalent) and is `O(max_pipeline_changes)` rather than `O(total_changes)` — verified by running the same workload through buffered `advanceAsync` and streaming `advanceStreaming` and showing the streaming peak is at least ~Nx smaller for an N-pipeline workload with balanced output.
4. Full vitest + `cargo test` suite continues to pass; benchmark numbers are committed to `.planning/milestones/v5.0-bench-results.md` for posterity.

**Plans:** TBD

---

## Progress

| Phase                                       | Plans Complete | Status      | Completed |
| ------------------------------------------- | -------------- | ----------- | --------- |
| 30. Audit Fixes                             | 2/4            | In Progress |           |
| 31. Rust Streaming Primitives + TS Wrappers | 0/0            | Not started | —         |
| 32. View-Syncer Streaming Migration         | 0/0            | Not started | —         |
| 33. Performance Tuning + Benchmarks         | 0/0            | Not started | —         |

---

## Coverage

- v5.0 requirements: 29
- Mapped: 29 (100%)
- Orphans: 0

| Phase    | Requirement count | Requirement IDs                                                                                                                                                                    |
| -------- | ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Phase 30 | 4                 | AUDIT-01, AUDIT-02, AUDIT-03, AUDIT-04                                                                                                                                             |
| Phase 31 | 18                | STREAM-01, STREAM-02, STREAM-03, STREAM-04, STREAM-05, STREAM-06, WRAP-01, WRAP-02, WRAP-03, WRAP-04, COMPAT-01, COMPAT-02, COMPAT-03, TEST-01, TEST-02, TEST-03, TEST-04, TEST-05 |
| Phase 32 | 4                 | MIGRATE-01, MIGRATE-02, MIGRATE-03, MIGRATE-04                                                                                                                                     |
| Phase 33 | 3                 | PERF-01, PERF-02, PERF-03                                                                                                                                                          |
