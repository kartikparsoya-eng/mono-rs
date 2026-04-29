# IVM Streaming Plan

Goal: stream per-pipeline `RowChange` chunks from Rust IVM to JS so clients
see fast pipelines' rows immediately while slow pipelines are still
computing. Today's parallelism (rayon `par_iter` for hydrate) only reduces
total wall time; with streaming, the parallelism becomes visible to clients
as reduced time-to-first-byte.

**Hard constraint: every existing method, contract, test, and consumer keeps
working unchanged.** The streaming path is added as new methods, and
consumers migrate one at a time. If we have to revert, the streaming code
can be deleted with zero regressions to the buffered path.

---

## 1. Surface area to preserve

These APIs and their semantics must not change:

### Rust napi (`zqlite-rs`)

- `RustPipelineManager.advance(id, changesJson) → Buffer`
- `RustPipelineManager.advanceAsync(id, changesJson) → Promise<Buffer>`
- `RustPipelineManager.hydrate(id) → Buffer`
- `RustPipelineManager.hydrateAsync(id) → Promise<Buffer>`
- `RustPipelineManager.hydrateQuery(id, queryId) → Buffer`
- `RustPipelineManager.hydrateQueryAsync(id, queryId) → Promise<Buffer>`
- All `addQuery / removeQuery / setTableSpecs / setPermissionTables / setQueryCompanions / swapSnapshot / pipelineCount` calls

### TS PipelineDriver

- `advance(timer): { version, numChanges, changes: Iterable<RowChange | 'yield'> }`
- `advanceAsync(timer): Promise<{ version, numChanges, changes: Iterable<RowChange | 'yield'> }>`
- `addQuery(transformationHash, queryID, ast, timer): Iterable<RowChange | 'yield'>`
- `addQueries(queries, timer): Iterable<RowChange | 'yield'>`
- `addQueriesAsync(queries, timer): Promise<Iterable<RowChange | 'yield'>>`

### Wire / protocol

- `encode_advance_result_buf` binary format and `decodeAdvanceResultBuf` decoder.
- `RowChange`, `ResetPipelinesSignal('scalar-subquery' | 'advancement-timeout' | …)`.
- Poke protocol (`startPoke / pokePart / pokers.end / pokers.cancel`).
- CVR commit at end of `#advancePipelines`.

Only **additions** are allowed.

---

## 2. New streaming surface

### Rust napi additions

A new napi-class `AdvanceStream` (and twin `HydrateStream`) acts as a JS
async iterator backed by an mpsc channel. The manager spawns one rayon task
per pipeline; each task runs the existing `advance_persistent_pipeline`
unchanged, encodes its `Vec<RowChange>` into a chunk Buffer, and sends it
through the channel. JS pulls chunks via `next()`.

```rust
#[napi]
pub struct AdvanceStream {
    rx: Arc<Mutex<mpsc::Receiver<StreamItem>>>,
    cancel: Arc<AtomicBool>,
    /// Final reset_signal / error appears as a separate StreamItem variant,
    /// not as channel error, so JS sees a clean Done after.
    handle: Option<JoinHandle<()>>,  // background coordinator
}

enum StreamItem {
    Chunk(Vec<u8>),                     // encoded per-pipeline RowChanges
    ResetSignal(String),                // companion scalar changed
    Error(String, /*type=*/String),
}

#[napi]
impl AdvanceStream {
    /// Returns Promise<{ done: bool, value?: ChunkValue }>
    /// where ChunkValue = { kind: 'chunk' | 'reset' | 'error', ... }
    #[napi]
    pub fn next(&self) -> AsyncTask<NextTask>;

    /// Cancels the stream — sets cancel flag, drains receiver.
    /// Maps to JS iterator.return().
    #[napi]
    pub fn return_(&self);
}
```

New `RustPipelineManager` methods (purely additive):

- `advance_streaming(id, changes_json) → AdvanceStream`
- `hydrate_streaming(id) → HydrateStream` _(parallel hydrate is already
  rayon-based; streaming exposes per-pipeline completion order)_

Existing `advance` / `advance_async` / `hydrate` / `hydrate_async` are
**not modified**. They keep using `encode_advance_result_buf` over a fully
collected `Vec<RowChange>`.

### TS additions

`pipeline-driver.ts` gets new methods that return `AsyncIterable`:

- `advanceStreaming(timer): Promise<{ version, numChanges, changes: AsyncIterable<RowChange | 'yield'> }>`
- `addQueriesStreaming(queries, timer): AsyncIterable<RowChange | 'yield'>`

These call the new Rust streaming methods. The existing methods stay
exactly as they are and continue to use the old buffered Rust calls.

`decode-advance-buf.ts` gets a per-chunk decoder
`decodeAdvanceChunkBuf(buf): DecodedRowChange[]`. The existing
`decodeAdvanceResultBuf` is **unchanged** (still decodes the full-buffer
format with header + flags + reset_signal trailer). The chunk decoder is a
simpler subset — just per-row decoding without the `flags` byte or
`reset_signal` trailer (those are signalled out-of-band via
`StreamItem::ResetSignal`).

---

## 3. Where work happens, and the order it matters

The current `advance_instance` does three things in sequence:

1. **IVM fan-out**: push changes through each pipeline's operator tree.
2. **Permission filter + minRowVersion bump**: applied to the combined
   `Vec<RowChange>`.
3. **Companion scalar subquery check**: queries SQLite for current scalar
   values; if any changed, returns `reset_signal` (causes JS to throw
   `ResetPipelinesSignal('scalar-subquery')` and roll back the advance).

For streaming we keep the same logical order but localize step 2 and
sequence step 3 last:

- Step 1 becomes per-pipeline rayon tasks. Each task's output goes through
  step 2 inline (permission filter + minRowVersion bump are stateless
  per-row operations; same logic moves into the task with no behavior
  change). The task encodes its filtered chunk and sends to the channel.
- Step 3 remains a single global pass after all pipeline tasks join. If a
  reset is detected, a `StreamItem::ResetSignal` is sent and the channel
  closes. Companion **row** changes (the ones Rust emits today when a
  companion table row matches) are sent as one final chunk in their own
  `StreamItem::Chunk`.

Why "reset signal can land after partial chunks have streamed":

- JS poke protocol allows `pokers.cancel()` mid-stream — view-syncer
  already handles this on `ResetPipelinesSignal` (view-syncer.ts:2207).
- Clients see in-progress poke parts get discarded. Behavior identical to
  today's buffered path where the whole batch is rolled back when reset
  fires; the difference is only that streaming may have already sent some
  parts to clients before cancellation. Net effect on the client is the
  same: no commit, no apply.

---

## 4. Concurrency & lock model

Today: `advance_instance` holds the per-instance `Mutex<PipelineInstance>`
guard for the whole call, then iterates pipelines sequentially with
`pipelines.iter().flat_map(...)`.

With streaming: we still take the instance Mutex briefly to read the
`Vec<Mutex<PipelineState>>` and the immutable specs (`syncable_tables`,
`permission_tables`, `companions`), then drop the instance lock. Each
rayon task locks **only** its own `Mutex<PipelineState>`. This is already
the correct shape — `PipelineState` is `Send`, the connection pool is
`Send + Sync` (Arc/Mutex internally). No new unsafe, no shared mutable
state across tasks.

**Important: stream cancellation must wait for in-flight rayon tasks to
notice the cancel flag.** Tasks check the flag between operator pushes
inside `advance_persistent_pipeline`. If a task has already taken its
pipeline lock and is mid-push, cancel waits. Mutex hold time is still
bounded by one pipeline's push, which is what TS's
`#shouldAdvanceYieldMaybeAbortAdvance` is sized for.

---

## 5. Cancellation & timeout (matching today's semantics)

Today: TS `#wrapWithTimeout` checks `elapsed > totalHydrationTimeMs` (or
`> hydrationTime/2 && pos <= numChanges/2`) and throws
`ResetPipelinesSignal('advancement-timeout')`. The throw aborts the JS
generator; Rust has already finished all its work, so the timeout only
prevents _processing_ the changes, not generating them.

With streaming the timer becomes meaningful for the first time:

- TS streaming wrapper checks elapsed between chunks.
- On timeout, calls `stream.return_()` (sets Rust `cancel` flag).
- Rust tasks check `cancel` between operator pushes; running tasks finish
  their current pipeline and exit; queued tasks early-exit.
- JS throws `ResetPipelinesSignal('advancement-timeout')`.

Net behavior matches today's contract: same signal, same reason. The
optimization is that Rust _also_ stops doing work, which is strictly an
improvement.

---

## 6. Order semantics

Today's buffered output emits pipeline rows in
`pipelines.iter().flat_map(...)` order — i.e., the order pipelines were
added (`add_query` push order). Tests that assert specific ordering rely
on this.

Streaming emits chunks in _completion_ order (fast pipelines first), not
add order. This is why streaming is a **new API** and not a replacement:

- Existing tests stay on `advance` / `advanceAsync` / buffered hydrate.
  Their ordering invariant is unchanged.
- View-syncer (production consumer) doesn't care about cross-pipeline
  ordering — `processChanges` and the CVR updater handle changes
  per-query-id. Migrating it to streaming is safe.
- Within one pipeline's chunk, order is preserved (single rayon task, one
  encoded buffer).

If any future test or consumer needs total order, they keep using the
buffered API. No forced migration.

---

## 7. Step-by-step plan

### Phase A — Rust streaming primitives (no JS changes)

1. Add `StreamItem` enum and a `chunk_encoder` helper in `advance.rs`
   that encodes a `Vec<RowChange>` into the simpler chunk format
   (header `[u32 count][u8 0]` then per-row encoding). Reuse the existing
   per-row encoders (`encode_str`, `encode_json_value`).
2. Add `AdvanceStream` and `HydrateStream` napi structs in
   `pipeline_manager.rs` with `next()`, `return_()`, and an internal
   `Drop` that signals cancel.
3. Implement `advance_streaming(id, changes_json)`:
   - Parse `changes` (existing logic).
   - Clone the instance Arc; clone the necessary read-only fields
     (specs, permission_tables, companions, db_path).
   - Create `mpsc::channel`, `Arc<AtomicBool>` cancel.
   - Spawn rayon scope inside a helper thread: for each
     `Mutex<PipelineState>` in `instance.pipelines`, `scope.spawn`:
     - Check cancel; if set, return.
     - Lock pipeline; run `advance_persistent_pipeline` (unchanged).
     - Apply permission filter + minRowVersion bump (move existing
       `apply_permission_and_version_filters` logic to operate on the
       chunk).
     - Encode chunk Buffer; send `StreamItem::Chunk` (best-effort —
       receiver may have been closed via cancel).
   - After all tasks join: run companion check (existing
     `check_companions_and_emit`). If reset, send
     `StreamItem::ResetSignal` and drop sender. Else send companion
     row changes as one final `StreamItem::Chunk`.
   - Drop sender → channel closes → JS `next()` resolves with `done`.
4. Implement `hydrate_streaming` and `hydrate_query_streaming`
   symmetrically (no companion / permission steps for these).
5. Rust unit tests:
   - Per-pipeline cancellation: queue 50 pipelines, cancel after 5,
     assert no more than ~5+1 chunks delivered.
   - Reset signal: companion scalar changes mid-stream → reset
     delivered, channel closed.
   - Panic isolation: one pipeline panics, others still complete and
     panic surfaces as `StreamItem::Error`.
   - Determinism within a pipeline (same chunk for same input).

**Deliverable A:** new Rust APIs callable; old APIs untouched; existing
tests pass.

### Phase B — TS decoder & PipelineDriver wrappers (still no consumer migration)

1. Add `decodeAdvanceChunkBuf(buf)` in `decode-advance-buf.ts` —
   header `[u32 count][u8 0]` + per-row decoding. The existing
   `decodeAdvanceResultBuf` stays exactly as is.
2. Add `pipeline-driver.ts::advanceStreaming(timer, vsId?)`:
   - Same setup as `#rustAdvanceAsync` (snapshot diff, swapSnapshot,
     setPermissionTables).
   - Calls `manager.advanceStreaming(...)`.
   - Returns `{ version, numChanges, changes: this.#streamChanges(stream, timer) }`.
   - `#streamChanges` is an `async function*` that loops `stream.next()`,
     decodes each chunk, applies the same `#convertDispatchChanges`
     conversion, yields each `RowChange` and periodic `'yield'` tokens
     based on the timer (replicating today's `#wrapWithTimeout` shape).
   - On reset / error / timeout, calls `stream.return_()` and throws
     `ResetPipelinesSignal` / `Error`.
3. Add `addQueriesStreaming(queries, timer)` similarly, building on the
   logic in `addQueriesAsync`. Companion-bearing queries fall back to
   the existing TS hydrate path (same as today's `rustEligible` check).
4. TS tests for the new methods (parity vs the buffered path on a fixed
   input; cancellation; reset signal).

**Deliverable B:** TS streaming methods exist and work; nothing in
view-syncer or tests has changed; sync `advance` and async-buffered
`advanceAsync` still work bit-for-bit.

### Phase C — View-syncer migration

1. In `view-syncer.ts:#advancePipelines`, replace
   `await this.#pipelines.advanceAsync(...)` with
   `await this.#pipelines.advanceStreaming(...)`.
2. Verify `#processChanges` accepts `AsyncIterable<RowChange | 'yield'>`.
   It probably does `for of` — change to `for await of`. The signature
   change is internal to view-syncer and doesn't leak.
3. Verify CVR updater + poker semantics under interleaved chunks:
   - `pokers.start(...)` happens once, before iteration.
   - `pokers.pokePart(...)` may now fire many times across multiple
     chunks rather than at once.
   - `pokers.end(finalVersion)` still happens once after CVR commit.
   - `pokers.cancel()` on `ResetPipelinesSignal` still fires correctly.
4. Migrate `addQueriesAsync` callsite (the `#hydrateUnchangedQueries`
   path around view-syncer.ts:1898) to `addQueriesStreaming`.
5. Run the full test suite (pipeline-driver tests don't touch
   view-syncer; view-syncer tests should still pass with same outputs
   modulo per-pipeline interleaving).

**Deliverable C:** production consumer streams; all tests pass; sync
`advance` and async-buffered `advanceAsync` are still callable for
benches and any holdouts.

### Phase D — Tuning & polish

1. Channel size: bounded `mpsc::sync_channel(pipeline_count)` — every
   pipeline can buffer one chunk without blocking, but stragglers don't
   accumulate unbounded.
2. Per-pipeline timing instrumentation (optional `flags` bit on chunks
   for benchmark mode, like the existing `AdvanceTimings` trailer).
3. Memory footprint check: with streaming, peak Rust heap drops from
   `O(total_changes)` to `O(max_pipeline_changes)`. Confirm with
   benchmark.
4. Decide whether to deprecate `advanceAsync` (the buffered async)
   eventually. Not in this plan — leave as-is for tests/benches.

**Deliverable D:** parallelism translates to client-visible TTFB
improvement; metrics confirm.

---

## 8. Things that intentionally don't change

- **Sync `advance(timer)`** stays for tests/benches. It calls
  `manager.advance(...)` (the existing buffered method). Same Buffer
  format, same Vec materialization, same iteration semantics, same
  ordering.
- **`advanceAsync`** stays as-is. Equivalent to today's behavior.
- **`addQuery / addQueries / addQueriesAsync`** unchanged.
- **`encode_advance_result_buf` + `decodeAdvanceResultBuf`** unchanged.
- **All existing tests** in `pipeline-driver.*.test.ts`,
  `fuzz-ivm.test.ts`, `decode-advance-buf.test.ts`, Rust unit tests.
- **The poke protocol semantics** (start / part / end / cancel).
- **CVR commit happens exactly once at the end of advance**, after the
  full stream has been consumed. Streaming changes the timing of _when_
  parts arrive, not whether they're committed atomically.
- **`ResetPipelinesSignal`** for `'scalar-subquery'` and
  `'advancement-timeout'` reasons — same throw path, same recovery.
- **Companion subqueries**: companion scalar check still runs once
  globally, after pipelines complete. Companion **rows** (those Rust
  emits when a companion table row matches a where condition) still
  flow as RowChanges, just in a final chunk instead of mixed in.
- **Permission filter and minRowVersion**: same per-row logic, same
  output. Just moved from "applied to combined Vec" to "applied
  per-chunk in the rayon task." Mathematically identical because both
  operations are stateless per-row.
- **`split_edit_keys`** on the source — unchanged. (Bug #2 from the
  audit is orthogonal and should be fixed independently.)

---

## 9. Risks & how we contain them

| Risk                                                                | Containment                                                                                                                                                                 |
| ------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Per-pipeline ordering changes break a test                          | Streaming is opt-in; tests stay on buffered API.                                                                                                                            |
| Reset signal arriving after partial poke parts confuses clients     | Same as today — `pokers.cancel()` handles mid-stream cancellation; verify in Phase C.                                                                                       |
| Cancellation deadlock if a pipeline is mid-push                     | Already bounded by single-pipeline push time, same as today's `#shouldAdvanceYieldMaybeAbortAdvance`. Add a hard timeout on `stream.return_()` join as belt-and-suspenders. |
| Rayon panic in one pipeline poisons the channel                     | Catch in the spawn closure; surface as `StreamItem::Error`; channel still closes cleanly.                                                                                   |
| AsyncIterator overhead per chunk dominates for tiny advance batches | Phase D benchmark; if true, bypass streaming for `numChanges < threshold` and call buffered path. Easy A/B inside `advanceStreaming` itself.                                |
| Memory: many small chunks via napi Buffer allocations               | Phase D — pool buffer allocations or batch tiny pipelines into one chunk.                                                                                                   |

---

## 10. What "done" looks like

- `manager.advanceStreaming` and `manager.hydrateStreaming` exist
  alongside (not replacing) the buffered methods.
- `pipeline-driver.ts::advanceStreaming` and `addQueriesStreaming`
  exist alongside (not replacing) the existing methods.
- `view-syncer.ts:#advancePipelines` uses streaming; tests pass.
- A microbenchmark in `rust-ivm-bench.ts` (or new) shows that with N
  pipelines and a slow tail pipeline, time-to-first-poke drops from
  `max(pipeline_time)` to `min(pipeline_time)`.
- Sync `advance`, async `advanceAsync`, sync `hydrate`, batch
  `addQueries`, and async batch `addQueriesAsync` all still work and
  pass the existing test suite unchanged.
