# Milestone v5.0 Streaming — Requirements

**Goal:** Make the v4.0 rayon parallelism visible to clients via per-pipeline streaming. Fix 2 real bugs and 3 latent risks identified in `.planning/IVM-PORT-AUDIT.md`. Hard constraint: every existing method, contract, test, and consumer keeps working unchanged. The streaming path is opt-in.

**References:**

- `.planning/IVM-STREAMING-PLAN.md` — design and phasing
- `.planning/IVM-PORT-AUDIT.md` — bugs and risks

---

## v5.0 Requirements

### Streaming Primitives (Rust)

- [ ] **STREAM-01**: New `RustPipelineManager.advance_streaming(id, changesJson)` napi method emits one chunk per pipeline via an mpsc-backed AsyncIterator-shaped class. Existing `advance` / `advance_async` are unchanged.
- [ ] **STREAM-02**: New `RustPipelineManager.hydrate_streaming(id)` and `hydrate_query_streaming(id, queryId)` napi methods, symmetric to streaming advance. Existing `hydrate` / `hydrate_async` / `hydrate_query` / `hydrate_query_async` are unchanged.
- [ ] **STREAM-03**: Per-pipeline rayon tasks lock only their own `Mutex<PipelineState>`; the instance lock is held briefly only to read immutable specs. Pipelines run in parallel for both `advance_streaming` and `hydrate_streaming`.
- [ ] **STREAM-04**: Cancellation via `Arc<AtomicBool>` checked between operator pushes inside `advance_persistent_pipeline` — JS `iterator.return()` actually stops Rust work in flight.
- [ ] **STREAM-05**: Companion scalar subquery check runs once after all pipeline tasks join; reset signal arrives as a final `StreamItem::ResetSignal`. Companion row changes arrive as one final `StreamItem::Chunk`.
- [ ] **STREAM-06**: Permission table filter and minRowVersion bump applied per chunk inside the per-pipeline task (mathematically identical to today's combined-Vec pass).

### Streaming Wrappers (TS)

- [ ] **WRAP-01**: New `decodeAdvanceChunkBuf(buf): DecodedRowChange[]` per-chunk decoder in `decode-advance-buf.ts`. Existing `decodeAdvanceResultBuf` is unchanged.
- [ ] **WRAP-02**: New `pipeline-driver.ts::advanceStreaming(timer, vsId?): Promise<{version, numChanges, changes: AsyncIterable<RowChange | 'yield'>}>` — wraps `manager.advance_streaming`, applies snapshotter diff + swap as today.
- [ ] **WRAP-03**: New `pipeline-driver.ts::addQueriesStreaming(queries, timer): AsyncIterable<RowChange | 'yield'>` — wraps `manager.hydrate_streaming`, falls back to TS hydrate for queries with companions (same `rustEligible` check as today's `addQueriesAsync`).
- [ ] **WRAP-04**: TS streaming wrapper surfaces `ResetPipelinesSignal('scalar-subquery')` and `ResetPipelinesSignal('advancement-timeout')` with the same throw shape as today's buffered path; calls `stream.return_()` on timeout.

### View-Syncer Migration

- [ ] **MIGRATE-01**: `view-syncer.ts::#advancePipelines` uses `advanceStreaming` instead of `advanceAsync`. `#processChanges` accepts `AsyncIterable<RowChange | 'yield'>` and uses `for await of`.
- [ ] **MIGRATE-02**: `view-syncer.ts::#hydrateUnchangedQueries` (and any other batch hydration callsite) uses `addQueriesStreaming` instead of `addQueriesAsync`.
- [ ] **MIGRATE-03**: `pokers.pokePart` fires as fast pipelines complete (mid-batch), not only at the end. CVR commit still happens exactly once at the end after the full stream consumes; `pokers.end(finalVersion)` still fires once after CVR commit.
- [ ] **MIGRATE-04**: `pokers.cancel()` correctly fires when `ResetPipelinesSignal` is thrown mid-stream — clients drop in-flight changes (existing behavior preserved).

### Backwards Compatibility / Test Preservation

- [ ] **COMPAT-01**: All existing tests pass unchanged: `pipeline-driver.*.test.ts`, `fuzz-ivm.test.ts`, `decode-advance-buf.test.ts`, all Rust unit tests.
- [ ] **COMPAT-02**: Buffered methods (`advance`, `advanceAsync`, `hydrate`, `hydrateAsync`, `hydrateQuery`, `hydrateQueryAsync`, `addQuery`, `addQueries`, `addQueriesAsync`) unchanged in signature and behavior. The streaming path is purely additive.
- [ ] **COMPAT-03**: `encode_advance_result_buf` and `decodeAdvanceResultBuf` formats are unchanged.

### Streaming Tests (New)

- [ ] **TEST-01**: Rust unit test — per-pipeline cancellation: queue many pipelines, cancel after a few; assert no more than ~cancelled+1 chunks delivered.
- [ ] **TEST-02**: Rust unit test — reset signal mid-stream: companion scalar changes; reset delivered, channel closes, no further chunks.
- [ ] **TEST-03**: Rust unit test — panic isolation: one pipeline panics; others still complete; panic surfaces as `StreamItem::Error`.
- [ ] **TEST-04**: TS test — parity with buffered path on a fixed input (same RowChanges modulo cross-pipeline ordering).
- [ ] **TEST-05**: TS test — `iterator.return()` (e.g., `for await { break }`) calls Rust `stream.return_()` and stops work.

### Performance / Tuning

- [ ] **PERF-01**: Bounded `mpsc::sync_channel(pipeline_count)` so stragglers don't accumulate unbounded.
- [ ] **PERF-02**: Microbenchmark in `rust-ivm-bench.ts` (or new file) — N pipelines with one slow tail pipeline; assert TTFB ~min(pipeline_time), not max.
- [ ] **PERF-03**: Memory peak measurement — peak Rust heap drops from `O(total_changes)` to `O(max_pipeline_changes)` during advance/hydrate.

### Audit Bug Fixes (from `.planning/IVM-PORT-AUDIT.md`)

- [x] **AUDIT-01** (Bug #1): Fix `LIKE` case sensitivity at `packages/zqlite-rs/src/hydrate.rs:769`. Change `Predicate::Like(field, pattern, true)` to `false` for the `like` key. Add a regression test that distinguishes `LIKE 'Foo%'` from `LIKE 'foo%'`.
- [x] **AUDIT-02** (Bug #2): Fix `EXISTS` parent_field missing from `collect_split_edit_keys` in `packages/zqlite-rs/src/advance.rs`. Add `Condition::CorrelatedSubquery` arm that recurses and adds `related.correlation.parent_field` to keys. Add regression test: edit a column that is an EXISTS parent_field; assert source emits Remove+Add (not Edit) and downstream output is correct.
- [x] **AUDIT-03** (Risk #2): Promote framework-invariant `debug_assert!` to `assert!` in `join_op.rs` (parent edit / child edit must not change relationship), `exists_op.rs` (Unexpected re-entrancy), `take_op.rs` (Invalid state — duplicate primary key), `cap_op.rs` (partition key must not change on edit).
- [x] **AUDIT-04** (Risk #3): Fix `ExistsOperator` and `OrExistsOperator` Edit handling when `or_predicate` is set — evaluate `or_predicate` on both `old_node.row` and `node.row`; emit Remove if old passed but new doesn't, Add if vice versa, pass-through if both match the same way.

---

## Future Requirements (deferred to later milestones)

- Per-pipeline timing instrumentation as an optional flag in chunk metadata (extension of existing `AdvanceTimings` trailer for buffered).
- Per-row streaming via `ThreadsafeFunction` callbacks (rejected for v5.0 — chunk granularity is the right tradeoff for current data sizes).
- SharedArrayBuffer ring (rejected for v5.0 — too much complexity for current throughput).

## Out of Scope

- Replacing or deprecating any buffered method — explicit hard constraint of v5.0.
- Mid-pipeline streaming (yield rows from inside one pipeline's push) — would require restructuring `push_through_ptrs` to yield, much bigger lift, not justified for typical pipeline output sizes.
- Cross-instance streaming aggregation (each `RustPipelineManager` instance is a client group; no cross-instance fanout intended).
- Cap operator parity rewrite — Cap is dead code (not emitted by `ast_to_config`); deletion or rewrite is a separate decision.

---

## Traceability

| REQ-ID     | Phase    | Status      |
| ---------- | -------- | ----------- |
| STREAM-01  | Phase 31 | Not started |
| STREAM-02  | Phase 31 | Not started |
| STREAM-03  | Phase 31 | Not started |
| STREAM-04  | Phase 31 | Not started |
| STREAM-05  | Phase 31 | Not started |
| STREAM-06  | Phase 31 | Not started |
| WRAP-01    | Phase 31 | Not started |
| WRAP-02    | Phase 31 | Not started |
| WRAP-03    | Phase 31 | Not started |
| WRAP-04    | Phase 31 | Not started |
| MIGRATE-01 | Phase 32 | Not started |
| MIGRATE-02 | Phase 32 | Not started |
| MIGRATE-03 | Phase 32 | Not started |
| MIGRATE-04 | Phase 32 | Not started |
| COMPAT-01  | Phase 31 | Not started |
| COMPAT-02  | Phase 31 | Not started |
| COMPAT-03  | Phase 31 | Not started |
| TEST-01    | Phase 31 | Not started |
| TEST-02    | Phase 31 | Not started |
| TEST-03    | Phase 31 | Not started |
| TEST-04    | Phase 31 | Not started |
| TEST-05    | Phase 31 | Not started |
| PERF-01    | Phase 33 | Not started |
| PERF-02    | Phase 33 | Not started |
| PERF-03    | Phase 33 | Not started |
| AUDIT-01   | Phase 30 | Not started |
| AUDIT-02   | Phase 30 | Not started |
| AUDIT-03   | Phase 30 | Not started |
| AUDIT-04   | Phase 30 | Not started |
