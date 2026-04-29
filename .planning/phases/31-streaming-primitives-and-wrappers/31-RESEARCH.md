# Phase 31: Rust Streaming Primitives + TS Wrappers - Research

**Researched:** 2026-04-29
**Domain:** napi-rs 3 streaming bridge (Rust mpsc → JS async iterator), per-pipeline rayon fan-out, TS PipelineDriver async wrappers, fast-check parity fuzzing
**Confidence:** HIGH for napi/AsyncTask + rayon (verified against Cargo.lock, existing code, official docs); MEDIUM for the exact rayon-coordinator-thread pattern (no in-repo precedent — first user); HIGH for TS test patterns (mirrors existing fuzz-ivm.test.ts).

## Summary

Phase 31's design is fully locked by `31-CONTEXT.md` (D-01..D-22) and `IVM-STREAMING-PLAN.md` §1-10. The remaining open implementation questions all reduce to mechanical "which knob in napi-rs 3 / rayon / std::sync::mpsc do we turn." The two significant findings:

1. **The canonical streaming pattern is "AsyncTask-per-next()"** — not a single long-lived async task, not `napi::Generator`, not `ThreadsafeFunction`. `AdvanceStream::next()` returns a fresh `AsyncTask<NextChunkTask>` each call; that task's `compute()` runs on a libuv worker thread, calls `rx.recv()` on the std mpsc receiver (blocking on libuv worker is fine — that's exactly what the worker pool exists for), and `resolve()` converts the `StreamItem` into a JS object. This matches the existing `advance_async` pattern in `pipeline_manager.rs:312-333` exactly — same `Task` trait, same `Vec<u8>` → `Buffer` resolve, just N times instead of once. No tokio dependency added; no `async` napi feature added; no `ThreadsafeFunction` complexity.

2. **The rayon fan-out runs on a dedicated coordinator thread spawned via `std::thread::spawn` at `advance_streaming` entry** — not from the AsyncTask's libuv worker. The coordinator owns a `rayon::scope` (synchronous, blocks the coordinator thread) inside which it spawns one `scope.spawn` per pipeline. Each pipeline task does `advance_persistent_pipeline(...)` → permission/version filter → encode chunk → `tx.send(StreamItem::Chunk(...))`. After scope returns (all pipelines joined), coordinator runs `check_companions_and_emit` and sends final `StreamItem::ResetSignal` or `StreamItem::Chunk` for companion rows. Then drops `tx` → channel closes → next `next()` call resolves to `done: true`. This pattern is one-thread-per-`advance_streaming`-call and is bounded by stream lifetime; on Drop/return\_(), coordinator observes the AtomicBool and the rayon scope wraps up normally as each task short-circuits.

**Primary recommendation:** Land 31-01 (Rust) using `AsyncTask`-per-`next()` + `std::thread` coordinator + `std::sync::mpsc::sync_channel(pipeline_count)` + `rayon::scope` inside the coordinator + `Arc<AtomicBool>` cancel + `Drop` impl on `AdvanceStream` that flips the flag. Land 31-02 (TS) using a thin `async function*` wrapper around `manager.advanceStreaming(...)` + `try { for await } finally { stream.return_() }` + a per-chunk decoder that mirrors `decodeAdvanceResultBuf` minus the trailer.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions

#### Plan Granularity

- **D-01:** Two plans, sequential waves. Phase A / Phase B split from `IVM-STREAMING-PLAN.md` matches the natural compile-dependency boundary — TS literally cannot reference the napi `AdvanceStream` class until it exists. Split:
  - **31-01 (Wave 1, autonomous):** All Rust work in `packages/zqlite-rs` — chunk encoder, `StreamItem` enum, `AdvanceStream` / `HydrateStream` napi classes with `next()` + `return_()` + `Drop`-on-cancel, `advance_streaming` / `hydrate_streaming` / `hydrate_query_streaming` method bodies, all Rust unit tests covering TEST-01 / TEST-02 / TEST-03.
  - **31-02 (Wave 2, autonomous, depends_on 31-01):** All TS work in `packages/zero-cache` — `decodeAdvanceChunkBuf` in `decode-advance-buf.ts`, `pipeline-driver.ts::advanceStreaming` and `addQueriesStreaming`, `RustStreamError` class, all TS tests covering TEST-04 / TEST-05.
- **D-02:** No third plan. Tightly coupled within each language; finer granularity adds GSD ceremony without rollback safety.

#### Chunk Format Forward-Compat

- **D-03:** `[u32 count][u8 0]` chunk header treats the second byte as a **reserved flags field** — not zero-padding. Document explicitly in both Rust (`chunk_encoder` doc comment) and TS (`decodeAdvanceChunkBuf` JSDoc): "Reserved flags byte. Must be 0 in v1. Future versions (Phase 33) may set bits for per-pipeline timing telemetry; see `IVM-STREAMING-PLAN.md §10 Phase D`."
- **D-04:** `decodeAdvanceChunkBuf` MUST validate `flags === 0` and throw a clear error if not: `throw new Error('decodeAdvanceChunkBuf: unexpected non-zero flags byte (need decoder upgrade for new format bits)')`.
- **D-05:** Mirrors the pattern already established by `encode_advance_result_buf`'s flags byte — consistency, not speculation.

#### Test Depth

- **D-06:** TEST-01 / TEST-02 / TEST-03 / TEST-05 stay **deterministic** — explicit barriers, fixed inputs, single-rayon-task scheduling where possible.
- **D-07:** **Add one new property-based fuzz test** — `packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts` — using `fast-check` with `FUZZ_NUM_RUNS=1000` (matches the existing `fuzz-ivm.test.ts` cadence). Property: for any random input changes accepted by `advance_async`, `advance_streaming` produces an equivalent `RowChange` multiset modulo cross-pipeline ordering.
- **D-08:** This single fuzz pins TEST-04's parity claim with stochastic depth. Catches multi-pipeline interleave bugs that fixed inputs would miss. Avoid fuzzing cancellation timing — those are easier to test with explicit barriers (TEST-01 / TEST-05).
- **D-09:** All existing tests must pass unchanged: `pipeline-driver.*.test.ts` (135 tests / 12 files), `fuzz-ivm.test.ts` 1k iterations, `decode-advance-buf.test.ts`, `cargo test --release` for both `zero-ivm-rs` and `zqlite-rs`.

#### Error Surface Mapping (Rust → TS)

- **D-10:** Define and export `RustStreamError` from `pipeline-driver.ts`:
  ```ts
  export class RustStreamError extends Error {
    readonly source = 'rust-stream' as const;
    constructor(
      message: string,
      readonly kind: 'panic' | 'rayon_error' | 'channel_closed',
    ) {
      super(message);
      this.name = 'RustStreamError';
    }
  }
  ```
- **D-11:** `StreamItem::Error(message, type)` from Rust → `throw new RustStreamError(message, kind)`. The Rust `type` string is the `kind` field. Allowed kinds: `'panic'` / `'rayon_error'` / `'channel_closed'`. Adding new kinds is additive — TS exhaustiveness checks must use a fallthrough `default` branch, not a type-narrowed switch with `never`.
- **D-12:** `ResetPipelinesSignal('scalar-subquery' | 'advancement-timeout')` throw shape **unchanged** per WRAP-04. `ResetPipelinesSignal` is a recoverable signal; `RustStreamError` is a real failure.
- **D-13:** Phase 32's view-syncer migration MUST use `error.kind === 'panic'` / `instanceof RustStreamError` for branching, not string-matching on `.message`.

#### Cancellation & Lifetime

- **D-14:** `AdvanceStream::Drop` sets the cancel flag. Handles GC-collected-half-consumed-iterator case — Drop impl is the safety net, not the only cancellation path. Explicit `return_()` is primary; Drop is implicit cleanup.
- **D-15:** TS streaming wrapper MUST `try { for await ... } finally { stream.return_() }` to guarantee cancel-on-throw and cancel-on-break, even though Drop will eventually fire. Belt-and-suspenders.

#### Performance / Channel Sizing

- **D-16:** Phase 31 ships **bounded** `mpsc::sync_channel(pipeline_count)` from the start. Reasoning: every pipeline can buffer one chunk without blocking, but stragglers don't accumulate unbounded. Defer further tuning to Phase 33 PERF-01.
- **D-17:** `pipeline_count` is read from `instance.pipelines.len()` after the brief instance-Mutex read. Don't hardcode a constant.

#### Naming

- **D-18:** Rust napi method names use snake_case: `advance_streaming`, `hydrate_streaming`, `hydrate_query_streaming`. TS-side names are camelCase: `advanceStreaming`, `addQueriesStreaming`. The napi-rs binding generates the camelCase JS facade automatically.

#### Rust Crate Layout

- **D-19:** New types in `packages/zqlite-rs/src/`:
  - `chunk_encoder.rs` (new file) — `encode_chunk_buf(rows: &[RowChange]) -> Vec<u8>` + `StreamItem` enum + companion encode helpers. Reuses existing per-row encoders from `advance.rs` (`encode_str`, `encode_json_value`).
  - `pipeline_manager.rs` (existing) — `AdvanceStream`, `HydrateStream` napi-class definitions + `advance_streaming` / `hydrate_streaming` / `hydrate_query_streaming` methods on `RustPipelineManager`.
- **D-20:** Permission filter + minRowVersion bump logic stays where it is today in `pipeline_manager.rs::apply_permission_and_version_filters`; the streaming code factors out the same logic to operate on a per-chunk `Vec<RowChange>` and calls it inside the rayon task. Same logic, same output, just earlier scope.

#### Build & Verification

- **D-21:** Phase 30-05's `assertNapiBinaryFreshness` gate already covers the new napi exports.
- **D-22:** Verifier MUST run `cargo test --release` (not just dev) to exercise any newly promoted `assert!` from Phase 30 in the streaming path.

### Claude's Discretion

- Specific rayon scope vs spawn pattern (helper thread owning a `rayon::scope`, vs `tokio::task::spawn_blocking` wrapping a `rayon::scope`, vs `napi::tokio_runtime::spawn`). Pick the pattern that matches existing `pipeline_manager.rs` patterns and minimizes thread overhead.
- Buffer ownership at the napi boundary (`Vec<u8>` vs `napi::bindgen_prelude::Buffer`) — pick whatever the existing buffered methods use for `Buffer` returns and match.
- Exact `'yield'` token cadence in the TS streaming wrapper — use the same time-based check as today's `#wrapWithTimeout`.
- Internal error type for the Rust `panic::catch_unwind` shim that produces `StreamItem::Error('panic', ...)`. Pick whatever idiom the existing rayon-using code in the repo uses; if none, `std::panic::catch_unwind` + format the payload via `Any::downcast_ref::<&str>()` / `Any::downcast_ref::<String>()` is fine.

### Deferred Ideas (OUT OF SCOPE)

- Per-pipeline timing telemetry (chunk format flags bit) → **Phase 33** (PERF-01 / PERF-02). Reserved byte is encoded today; bits get assigned later.
- Channel sizing tuning beyond the initial `pipeline_count` bound → **Phase 33** (PERF-01).
- Memory-footprint benchmark (peak Rust heap drops from `O(total_changes)` to `O(max_pipeline_changes)`) → **Phase 33** (PERF-02).
- Bypassing streaming for `numChanges < threshold` (small-batch optimization) → **Phase 33** (PERF-02 risk in `IVM-STREAMING-PLAN.md §9`).
- Buffer pool / batch-tiny-pipelines optimization → **Phase 33** (PERF-02 risk).
- Whether to deprecate `advanceAsync` eventually → **NOT in this milestone** per `IVM-STREAMING-PLAN.md §8`.
- View-syncer migration (`#advancePipelines` → `advanceStreaming`, `#processChanges` → `for await`) → **Phase 32** (MIGRATE-01 / MIGRATE-02).
  </user_constraints>

<phase_requirements>

## Phase Requirements

| ID        | Description                                                                                                                                                                                           | Research Support                                                                                 |
| --------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------- |
| STREAM-01 | `RustPipelineManager.advance_streaming(id, changesJson)` napi method emits one chunk per pipeline via mpsc-backed AsyncIterator-shaped class                                                          | §1.1 (AsyncTask-per-next pattern), §2 (existing `advance_async` reference shape)                 |
| STREAM-02 | `RustPipelineManager.hydrate_streaming(id)` and `hydrate_query_streaming(id, queryId)` napi methods, symmetric to streaming advance                                                                   | §1.1, §2 (existing `hydrate_async` / `hydrate_query_async` reference shape)                      |
| STREAM-03 | Per-pipeline rayon tasks lock only their own `Mutex<PipelineState>`; instance lock held briefly only to read immutable specs                                                                          | §1.2 (rayon::scope coordinator), §2 (existing `instance_arc.clone()` pattern in `advance_async`) |
| STREAM-04 | Cancellation via `Arc<AtomicBool>` checked between operator pushes inside `advance_persistent_pipeline` — `iterator.return()` actually stops Rust work                                                | §3.1 (cancel-check insertion points), §1.2 (Drop ordering)                                       |
| STREAM-05 | Companion scalar subquery check runs once after all pipeline tasks join; reset signal arrives as final `StreamItem::ResetSignal`. Companion row changes arrive as one final `StreamItem::Chunk`       | §1.2 (post-scope coordinator), §2 (existing `check_companions_and_emit` location)                |
| STREAM-06 | Permission table filter and minRowVersion bump applied per chunk inside per-pipeline task (mathematically identical to today's combined-Vec pass)                                                     | §2 (`apply_permission_and_version_filters` extraction)                                           |
| WRAP-01   | New `decodeAdvanceChunkBuf(buf): DecodedRowChange[]` per-chunk decoder in `decode-advance-buf.ts`. Existing `decodeAdvanceResultBuf` unchanged                                                        | §1.3 (header diff: chunk = `[u32 count][u8 0]` no error/reset trailer)                           |
| WRAP-02   | New `pipeline-driver.ts::advanceStreaming(timer, vsId?)` returning `Promise<{version, numChanges, changes: AsyncIterable<RowChange                                                                    | 'yield'>}>`                                                                                      | §1.4 (`async function*` wrapper shape), §2 (`#rustAdvanceAsync` reference) |
| WRAP-03   | New `addQueriesStreaming(queries, timer)` returning `AsyncIterable<RowChange                                                                                                                          | 'yield'>`. Companion-bearing queries fall back to TS hydrate via `rustEligible` check            | §1.4, §2 (`addQueriesAsync` reference shape)                               |
| WRAP-04   | TS streaming wrapper surfaces `ResetPipelinesSignal('scalar-subquery')` and `ResetPipelinesSignal('advancement-timeout')` with same throw shape as buffered path; calls `stream.return_()` on timeout | §1.4 (try/finally pattern), §2 (existing `#wrapWithTimeout` reference)                           |
| COMPAT-01 | All existing tests pass unchanged                                                                                                                                                                     | §4 (no signature changes; new methods sit alongside)                                             |
| COMPAT-02 | Buffered methods unchanged in signature and behavior                                                                                                                                                  | §4 (additive-only constraint)                                                                    |
| COMPAT-03 | `encode_advance_result_buf` and `decodeAdvanceResultBuf` formats unchanged                                                                                                                            | §4 (chunk format is a strict subset, separate function)                                          |
| TEST-01   | Rust unit test — per-pipeline cancellation: queue many pipelines, cancel after a few; assert no more than ~cancelled+1 chunks delivered                                                               | §3.2 (deterministic cancellation barrier pattern)                                                |
| TEST-02   | Rust unit test — reset signal mid-stream: companion scalar changes; reset delivered, channel closes, no further chunks                                                                                | §3.2 (companion mock pattern)                                                                    |
| TEST-03   | Rust unit test — panic isolation: one pipeline panics; others still complete; panic surfaces as `StreamItem::Error`                                                                                   | §1.5 (catch_unwind shim), §3.2 (panic injection pattern)                                         |
| TEST-04   | TS test — parity with buffered path on a fixed input (same RowChanges modulo cross-pipeline ordering)                                                                                                 | §3.3 (fast-check parity property + multiset comparator)                                          |
| TEST-05   | TS test — `iterator.return()` (e.g., `for await { break }`) calls Rust `stream.return_()` and stops work                                                                                              | §3.3 (synthetic slow-pipeline pattern)                                                           |

</phase_requirements>

## Architectural Responsibility Map

| Capability                                  | Primary Tier                                                    | Secondary Tier                         | Rationale                                                                                           |
| ------------------------------------------- | --------------------------------------------------------------- | -------------------------------------- | --------------------------------------------------------------------------------------------------- |
| Per-pipeline rayon fan-out                  | Rust (`zqlite-rs::pipeline_manager`)                            | —                                      | All multi-core IVM is in Rust per project constraint; TS is single-threaded coordination only.      |
| mpsc channel + AsyncTask bridge             | Rust (`zqlite-rs::pipeline_manager`)                            | —                                      | The bridge from Rust threads (rayon) to JS (Promise) is napi-rs's job.                              |
| Chunk binary encoding                       | Rust (`zqlite-rs::chunk_encoder`)                               | —                                      | Binary FFI format owned by the Rust producer; TS only decodes.                                      |
| Chunk binary decoding                       | TS (`zero-cache::decode-advance-buf`)                           | —                                      | Symmetric to existing `decodeAdvanceResultBuf` location; consumer side.                             |
| Cancellation flag (Arc<AtomicBool>)         | Rust (`pipeline_manager::AdvanceStream`)                        | TS (calls `return_()`)                 | Flag lives in Rust; TS triggers it via napi method.                                                 |
| Async iterator shape (Symbol.asyncIterator) | TS (`pipeline-driver::advanceStreaming`)                        | Rust (provides `next()` + `return_()`) | JS iteration protocol is a TS concern; Rust exposes the primitive ops.                              |
| Timer / 'yield' token cadence               | TS (`pipeline-driver::#wrapWithTimeout`-equivalent)             | —                                      | Time-based throttling stays in TS — same place it lives today; Rust has no Timer concept.           |
| ResetPipelinesSignal throw                  | TS (`pipeline-driver::advanceStreaming`)                        | Rust (sends `StreamItem::ResetSignal`) | Existing TS class; Rust just signals via stream item.                                               |
| RustStreamError throw                       | TS (`pipeline-driver`)                                          | Rust (sends `StreamItem::Error`)       | New TS class per D-10; Rust signals via stream item kind discriminator.                             |
| Companion scalar check                      | Rust (`pipeline_manager::check_companions_and_emit`)            | —                                      | Already in Rust; just moves call site to post-scope coordinator.                                    |
| Permission filter + minRowVersion bump      | Rust (`pipeline_manager::apply_permission_and_version_filters`) | —                                      | Already in Rust; just moves call site to per-pipeline task per D-20.                                |
| Companion row change emission               | Rust (`pipeline_manager`)                                       | —                                      | Already in Rust; emits as final `StreamItem::Chunk` per STREAM-05.                                  |
| Diff snapshot setup (BEGIN CONCURRENT)      | TS (`pipeline-driver::#snapshotter.advance`)                    | —                                      | Unchanged from existing path — Rust cannot open its own connections (D-20 carry-forward from v4.0). |

## Standard Stack

### Core (already in dep tree, no Cargo.toml changes needed)

| Library       | Version                                       | Purpose                                                                       | Why Standard                                                                                                                    |
| ------------- | --------------------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `napi`        | 3.8.5 (verified Cargo.lock)                   | NAPI-RS Rust bindings — `#[napi]` macros, `AsyncTask`, `Buffer`, `Task` trait | Already used everywhere in `zqlite-rs`. `AsyncTask` is the project-blessed Rust↔JS async bridge (per existing `advance_async`). |
| `napi-derive` | 3.5.4 (verified Cargo.lock)                   | `#[napi]` proc-macros for class/method generation                             | Companion to `napi`.                                                                                                            |
| `rayon`       | 1.10 (verified Cargo.toml)                    | CPU parallelism — `par_iter`, `scope`, `spawn`                                | Already used for `hydrate` parallel fan-out (`pipeline_manager.rs:429`, `advance.rs:1484`).                                     |
| `serde_json`  | 1 with `preserve_order` (verified Cargo.toml) | JSON parsing for `changes_json` input + `RowChange` internal representation   | Existing dependency.                                                                                                            |

### Standard Library (no new deps)

| Item                                             | Purpose                                                                                     | Why Standard                                                                                                                                              |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `std::sync::mpsc::sync_channel(n)`               | Bounded channel for chunk handoff Rust→Rust (rayon task → coordinator → AsyncTask::compute) | Per D-16. Std is sufficient — receiver lives on a libuv worker thread (blocking is fine); sender is on rayon worker threads. No tokio integration needed. |
| `std::sync::atomic::AtomicBool` (in `Arc`)       | Cancellation flag, Ordering::Relaxed for read, Ordering::SeqCst for write                   | Standard cancel pattern; no third-party crate needed.                                                                                                     |
| `std::panic::catch_unwind` + `Any::downcast_ref` | Panic isolation per rayon task per D-claude's-discretion / TEST-03                          | Standard idiom; rayon does NOT auto-catch panics in `scope.spawn` closures (it propagates through `scope` join).                                          |
| `std::thread::spawn`                             | One-off coordinator thread that owns the `rayon::scope` and post-scope companion check      | Bounded by stream lifetime; finishes when scope returns and channel sender is dropped.                                                                    |

### Alternatives Considered (and rejected for Phase 31)

| Instead of                                | Could Use                                                                | Why Rejected                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ----------------------------------------- | ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `AsyncTask`-per-`next()`                  | `napi::bindgen_prelude::Generator` trait                                 | `Generator` is sync-only (`fn next(&mut self, value: Option<Self::Next>) -> Option<Self::Yield>`). Documented as "experimental" per docs.rs. We need async. [VERIFIED: docs.rs/napi Generator trait page]                                                                                                                                                                                                                       |
| `AsyncTask`-per-`next()`                  | `#[napi] async fn next(&self) -> ...`                                    | Requires `async` feature flag, which transitively pulls in tokio runtime. Cargo.toml currently uses `["napi9", "compat-mode"]` — no tokio in dep tree (verified Cargo.lock — no `tokio` package present). Adding tokio just for the iterator pattern is overkill when the existing `AsyncTask` pattern already works on libuv worker threads. [VERIFIED: napi.rs/docs/concepts/async-fn — "async or tokio_rt feature required"] |
| `std::sync::mpsc`                         | `tokio::sync::mpsc`                                                      | Receiver runs on libuv worker (blocking-permitted thread). `tokio::mpsc` requires a tokio runtime to drive the receiver, which we don't have. Std mpsc's `recv()` blocks the calling thread — exactly the right behavior for an `AsyncTask::compute()` running on a libuv worker.                                                                                                                                               |
| `std::sync::mpsc`                         | `crossbeam_channel`                                                      | No win for this use case. Std mpsc is sufficient and avoids a new dependency. crossbeam's sweet spot is high-contention concurrent producers; here we have N=pipeline_count producers (small, bounded) and 1 consumer.                                                                                                                                                                                                          |
| `ThreadsafeFunction` (TSFN)               | —                                                                        | TSFN is the right tool for "Rust thread invokes JS callback." We want the inverse: "JS awaits next chunk." `AsyncTask` is the inverse direction and is much simpler. TSFN would force us to push chunks at JS, which conflates with backpressure (TSFN has its own queue).                                                                                                                                                      |
| Coordinator thread that owns rayon::scope | `rayon::spawn` (no scope) for each pipeline + manual JoinHandle tracking | `rayon::spawn` is fire-and-forget; we lose the natural join point that `scope` provides. Companion check runs after join, so we want join semantics.                                                                                                                                                                                                                                                                            |
| Coordinator thread that owns rayon::scope | `pipelines.par_iter().for_each(...)` directly                            | Won't work — `par_iter` would be called from `AsyncTask::compute()` on the libuv worker thread. The first `next()` call triggers it; subsequent `next()` calls would have nothing to do because `par_iter().for_each` blocks until ALL iterations finish. We want the receiver to drain chunks as they arrive.                                                                                                                  |

**Installation:** Nothing to install. All deps already present in `packages/zqlite-rs/Cargo.toml`. [VERIFIED: read Cargo.toml lines 9-26]

**Version verification:** Confirmed by reading `packages/zqlite-rs/Cargo.lock`:

- `napi = 3.8.5` (latest 3.x, no upgrade needed)
- `napi-derive = 3.5.4`
- `rayon = 1.10` (per Cargo.toml; lockfile not re-checked but compat is fine)
- No `tokio`, no `crossbeam-channel` in lockfile — those are NOT being added.

## Architecture Patterns

### System Architecture Diagram

```
                   ┌─────────────── JS (zero-cache) ───────────────┐
                   │                                                │
TS caller          │  pipeline-driver.advanceStreaming(timer)       │
   │               │              │                                 │
   │               │              ▼                                 │
   │               │  manager.advanceStreaming(id, changesJson)     │
   │               │              │ (sync — returns AdvanceStream)  │
   │               │              ▼                                 │
   │               │   AdvanceStream  ◀── napi class instance       │
   │               │       │                                        │
   │  for await────┼──────►│ stream.next() ──► Promise<{done, val}> │
   │  (rejecting)  │       │                                        │
   │               │       │ (on break/throw/timeout)               │
   │               │       │     stream.return_() (sets cancel)     │
   └───────────────┼───────┴────────────────────────────────────────┘
                   │       │
                   │  ════ napi FFI boundary ════
                   │       │
                   ▼       ▼
                ┌──────── Rust (zqlite-rs) ──────────────────────┐
                │                                                 │
                │  AsyncTask<NextChunkTask>::compute()            │
                │     │ runs on libuv worker thread               │
                │     │ (one libuv worker per next() call)        │
                │     ▼                                           │
                │   rx.recv() ◀── std::sync::mpsc::Receiver       │
                │                  (Arc<Mutex<...>> for repeats)  │
                │                       ▲                         │
                │                       │ tx.send(StreamItem)     │
                │                       │                         │
                │  ┌────────────────────┴───────────────────────┐ │
                │  │ Coordinator thread (one per advance_       │ │
                │  │  streaming call, std::thread::spawn'd):    │ │
                │  │                                            │ │
                │  │  rayon::scope(|scope| {                    │ │
                │  │    for pipeline in instance.pipelines {    │ │
                │  │      scope.spawn(|| {                      │ │
                │  │        if cancel.load() { return }         │ │
                │  │        catch_unwind(|| {                   │ │
                │  │          let mut p = pipeline.lock();      │ │
                │  │          let chunk = advance_persistent_   │ │
                │  │              pipeline(&mut p, ...);        │ │
                │  │          let filtered = apply_permission_  │ │
                │  │              and_version_filters(chunk);   │ │
                │  │          let buf = encode_chunk_buf(       │ │
                │  │              &filtered);                   │ │
                │  │          tx.send(StreamItem::Chunk(buf))   │ │
                │  │        }).map_err(|p| {                    │ │
                │  │          tx.send(StreamItem::Error(        │ │
                │  │              fmt_panic(p), "panic"))       │ │
                │  │        })                                  │ │
                │  │      });                                   │ │
                │  │    }                                       │ │
                │  │  }); // <── all pipelines joined here      │ │
                │  │                                            │ │
                │  │  match check_companions_and_emit(...) {    │ │
                │  │    Reset(r) => tx.send(ResetSignal(r)),    │ │
                │  │    Changes(cs) => if !cs.is_empty() {      │ │
                │  │      tx.send(Chunk(encode_chunk_buf(&cs))) │ │
                │  │    }                                       │ │
                │  │  }                                         │ │
                │  │  drop(tx); // → channel closes → JS done   │ │
                │  └────────────────────────────────────────────┘ │
                └─────────────────────────────────────────────────┘

Cancel path (D-14, D-15):
  JS break / throw → finally → stream.return_() → cancel.store(true)
       OR
  AdvanceStream Drop (GC'd) → cancel.store(true)
       │
       ▼
  Rayon scope.spawn closure checks cancel.load() at:
    1. closure entry (skip if already cancelled)
    2. inside advance_persistent_pipeline between operator pushes
       (STREAM-04 — between iterations of `for change in changes.iter()`
        loop at advance.rs:1261)
    3. (optional) inside push_through_ptrs operator loop — only if
       single-pipeline pushes are demonstrably long-tail
       (skip in Phase 31 unless TEST-01 forces it)
```

### Recommended Project Structure (additive)

```
packages/zqlite-rs/src/
├── advance.rs                    # UNCHANGED public surface; extract
│                                  # apply_permission_and_version_filters
│                                  # to a free function or move to
│                                  # pipeline_manager.rs (it's already there).
│                                  # ADD: cancel.load() check inside
│                                  # advance_persistent_pipeline between
│                                  # operator pushes (per STREAM-04).
├── chunk_encoder.rs              # NEW (D-19) —
│                                  #   StreamItem enum
│                                  #   encode_chunk_buf(rows) -> Vec<u8>
│                                  #   Reuses encode_str / encode_json_value
│                                  #   from advance.rs (those are pub(crate))
├── pipeline_manager.rs           # ADD: AdvanceStream, HydrateStream
│                                  # napi-class structs + impls
│                                  # (next, return_, Drop)
│                                  # ADD: advance_streaming /
│                                  # hydrate_streaming /
│                                  # hydrate_query_streaming methods on
│                                  # RustPipelineManager
│                                  # KEEP: all existing methods byte-for-byte
└── lib.rs                        # ADD: pub mod chunk_encoder;

packages/zero-cache/src/services/view-syncer/
├── decode-advance-buf.ts         # ADD: decodeAdvanceChunkBuf export
│                                  # KEEP: decodeAdvanceResultBuf unchanged
├── decode-advance-buf.test.ts    # ADD: tests for decodeAdvanceChunkBuf
│                                  #      including non-zero-flags throw (D-04)
├── pipeline-driver.ts            # ADD: RustStreamError export
│                                  # ADD: NapiPipelineManager interface
│                                  #      (advanceStreaming, hydrateStreaming,
│                                  #       hydrateQueryStreaming) + nested
│                                  #       AdvanceStream / HydrateStream types
│                                  # ADD: PipelineDriver.advanceStreaming
│                                  # ADD: PipelineDriver.addQueriesStreaming
│                                  # ADD: #streamChanges async generator
│                                  # ADD: #wrapWithTimeoutAsync (or extend
│                                  #      #wrapWithTimeout to handle async)
│                                  # KEEP: advance / advanceAsync /
│                                  #       addQueries / addQueriesAsync byte-for-byte
└── streaming-vs-buffered-parity.fuzz.test.ts   # NEW (D-07) — fast-check
                                                 # parity property
```

### Pattern 1: AsyncTask-per-next() with shared mpsc receiver

**What:** Each `next()` call returns a fresh `AsyncTask` whose `compute()` runs on a libuv worker thread and blocks on `rx.recv()` until a `StreamItem` is available. The receiver is `Arc<Mutex<mpsc::Receiver<StreamItem>>>` so multiple `next()` calls don't race (Mutex serializes them naturally — JS calls are sequential anyway, but the Mutex makes the Send/Sync story trivial).

**When to use:** Any place we want a Rust struct to look like a JS `AsyncIterator`. This is the napi-rs 3 pattern that doesn't require the `async` feature flag (which would pull in tokio).

**Example (sketch):**

```rust
// Source: extends pattern from packages/zqlite-rs/src/pipeline_manager.rs:312-333
//         (existing advance_async pattern), no Context7 / external doc since
//         this is a derived pattern. [ASSUMED — pattern derived from existing code]

#[napi]
pub struct AdvanceStream {
    rx: Arc<Mutex<mpsc::Receiver<StreamItem>>>,
    cancel: Arc<AtomicBool>,
    // coordinator handle — kept so we can join on Drop if we want; can be None
    // after the channel closes.
    _coordinator: Option<std::thread::JoinHandle<()>>,
}

#[napi]
impl AdvanceStream {
    /// JS: stream.next() → Promise<NextChunkValue>
    #[napi(ts_return_type = "Promise<NextChunkValue>")]
    pub fn next(&self) -> AsyncTask<NextChunkTask> {
        AsyncTask::new(NextChunkTask {
            rx: self.rx.clone(),
        })
    }

    /// JS: stream.return_() → void  (synchronous; just flips the flag)
    #[napi]
    pub fn return_(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for AdvanceStream {
    fn drop(&mut self) {
        // D-14: belt-and-suspenders cancel.
        self.cancel.store(true, Ordering::SeqCst);
        // Don't join coordinator here — would block JS GC thread.
        // Coordinator notices cancel via the sender side: tx.send returns
        // Err once receiver is dropped, which it isn't yet (rx is in Arc),
        // BUT the cancel flag check at scope.spawn entry will short-circuit
        // remaining work.
    }
}

pub struct NextChunkTask {
    rx: Arc<Mutex<mpsc::Receiver<StreamItem>>>,
}

unsafe impl Send for NextChunkTask {}

impl Task for NextChunkTask {
    type Output = Option<StreamItem>;  // None = channel closed = done: true
    type JsValue = NextChunkValue;     // { done: bool, value?: ChunkValue }

    fn compute(&mut self) -> napi::Result<Self::Output> {
        // Blocking recv on libuv worker thread — this is what worker
        // threads exist for. RecvError means sender dropped → done.
        let rx = self.rx.lock().map_err(|e| {
            napi::Error::from_reason(format!("Stream rx mutex poisoned: {e}"))
        })?;
        match rx.recv() {
            Ok(item) => Ok(Some(item)),
            Err(_) => Ok(None),
        }
    }

    fn resolve(&mut self, env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        // Convert StreamItem → JS object shape.
        // Buffer wrap happens here on the main JS thread, where Env is valid.
        // [ASSUMED — exact NextChunkValue shape is per-implementation; could
        //  use a #[napi(object)] struct or napi::JsObject built manually.]
        match output {
            None => Ok(NextChunkValue { done: true, ..Default::default() }),
            Some(StreamItem::Chunk(buf)) => Ok(NextChunkValue {
                done: false,
                kind: Some("chunk".into()),
                chunk: Some(Buffer::from(buf)),
                ..Default::default()
            }),
            Some(StreamItem::ResetSignal(reason)) => Ok(NextChunkValue {
                done: false,
                kind: Some("reset".into()),
                reason: Some(reason),
                ..Default::default()
            }),
            Some(StreamItem::Error(msg, kind)) => Ok(NextChunkValue {
                done: false,
                kind: Some("error".into()),
                error_msg: Some(msg),
                error_kind: Some(kind),
                ..Default::default()
            }),
        }
    }
}
```

_Source provenance:_ `[ASSUMED]` — pattern is constructed from the existing `AdvanceTask`/`HydrateTask` shape at `pipeline_manager.rs:462-533`, the napi-rs `Task` trait docs at `napi.rs/docs/concepts/async-task`, and the `IVM-STREAMING-PLAN.md §2` skeleton. No exact precedent in repo or external example for the iterator-shaped variant; first user of this pattern in the codebase. The planner should treat this as the recommended approach but verify behavior in the integration test (TEST-05).

### Pattern 2: Coordinator thread + rayon::scope

**What:** At `advance_streaming` entry, spawn one `std::thread` (the "coordinator") that owns a `rayon::scope`. Inside the scope, one `scope.spawn` per pipeline. This decouples the long-lived rayon work from the per-`next()` libuv worker thread.

**When to use:** When you need synchronous "fan out, collect" semantics with chunked output via channel. The coordinator is bounded in lifetime by the stream — when the last pipeline finishes (or all are cancelled), coordinator drops `tx`, channel closes, JS sees `done: true` on next pull.

**Example (sketch):**

```rust
// Source: derived from IVM-STREAMING-PLAN.md §3-4 + existing patterns.
// [ASSUMED — pattern derived from plan + existing code]

pub fn advance_streaming(
    &self,
    id: String,
    changes_json: String,
) -> napi::Result<AdvanceStream> {
    let changes: Vec<Change> = serde_json::from_str(&changes_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;

    let instance_arc = {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?
            .clone()
    };

    // D-17: read pipeline_count under brief instance lock.
    let pipeline_count = {
        let instance = instance_arc.lock()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        instance.pipelines.len()
    };
    // D-16: bounded channel, capacity == pipeline_count.
    let (tx, rx) = mpsc::sync_channel::<StreamItem>(pipeline_count.max(1));
    let cancel = Arc::new(AtomicBool::new(false));

    let cancel_for_thread = cancel.clone();
    let coordinator = std::thread::Builder::new()
        .name(format!("advance-stream-{id}"))
        .spawn(move || {
            // Lock instance (held for the whole rayon scope — safe because
            // existing advance_async also holds this lock for its compute()).
            let instance = match instance_arc.lock() {
                Ok(g) => g,
                Err(_) => {
                    let _ = tx.send(StreamItem::Error(
                        "instance lock poisoned".into(), "rayon_error".into()));
                    return;
                }
            };

            rayon::scope(|s| {
                for pm in instance.pipelines.iter() {
                    let tx = tx.clone();
                    let cancel = cancel_for_thread.clone();
                    let permission_tables = &instance.permission_tables;
                    let syncable_tables = &instance.syncable_tables;
                    let db_path = instance.db_path.as_str();
                    let changes = &changes;
                    s.spawn(move |_| {
                        if cancel.load(Ordering::Relaxed) { return; }
                        // Catch panics so a panicking pipeline doesn't poison
                        // the rayon pool or leak across the napi boundary.
                        let result = std::panic::catch_unwind(
                            std::panic::AssertUnwindSafe(|| {
                                let mut pipeline = pm.lock().unwrap();
                                let chunk = advance_persistent_pipeline_with_cancel(
                                    &mut pipeline, changes, db_path, &cancel,
                                );
                                // STREAM-06 / D-20: filter per-chunk
                                let filtered = filter_chunk(
                                    chunk, permission_tables, syncable_tables);
                                if filtered.is_empty() { return None; }
                                Some(crate::chunk_encoder::encode_chunk_buf(&filtered))
                            }));
                        match result {
                            Ok(Some(buf)) => {
                                // Best-effort send — receiver may have dropped.
                                let _ = tx.send(StreamItem::Chunk(buf));
                            }
                            Ok(None) => { /* nothing to send */ }
                            Err(panic_payload) => {
                                let msg = format_panic_payload(panic_payload);
                                let _ = tx.send(StreamItem::Error(msg, "panic".into()));
                            }
                        }
                    });
                }
            }); // ← all pipelines joined here

            // STREAM-05: companion check after join.
            if !instance.companions.is_empty() {
                let changed_tables: HashSet<&str> =
                    changes.iter().map(|c| c.table.as_str()).collect();
                match check_companions_and_emit(
                    &instance.companions, &changes,
                    &changed_tables, &instance.db_path,
                ) {
                    CompanionResult::Reset(reason) => {
                        let _ = tx.send(StreamItem::ResetSignal(reason));
                    }
                    CompanionResult::Changes(companion_changes) => {
                        if !companion_changes.is_empty() {
                            // Filter companion rows too.
                            let mut cs = companion_changes;
                            apply_permission_and_version_filters(
                                &mut cs,
                                &instance.permission_tables,
                                &instance.syncable_tables,
                            );
                            if !cs.is_empty() {
                                let buf = crate::chunk_encoder::encode_chunk_buf(&cs);
                                let _ = tx.send(StreamItem::Chunk(buf));
                            }
                        }
                    }
                }
            }
            // tx dropped here → rx.recv() returns Err → JS sees done: true.
        })
        .map_err(|e| napi::Error::from_reason(format!("Failed to spawn coordinator: {e}")))?;

    Ok(AdvanceStream {
        rx: Arc::new(Mutex::new(rx)),
        cancel,
        _coordinator: Some(coordinator),
    })
}
```

_Source provenance:_ `[ASSUMED]` — pattern is derived from `IVM-STREAMING-PLAN.md §3-4` ("Spawn rayon scope inside a helper thread") + `pipeline_manager.rs:379-457` (existing `advance_instance` shape). No external docs read for this exact composition; the planner should sanity-check against the existing `advance_async` AsyncTask pattern.

### Anti-Patterns to Avoid

- **Don't call `par_iter().for_each(...)` from `AsyncTask::compute`.** That blocks the libuv worker until ALL pipelines finish — destroys the streaming property. Use a separate coordinator thread + scope.spawn + channel send.
- **Don't use `tokio::sync::mpsc` thinking it's "more async."** Without a tokio runtime running, `tokio::mpsc::Receiver::recv().await` panics. Std mpsc's blocking recv on a libuv worker is the right primitive.
- **Don't `try_recv` in a busy loop.** Wastes a libuv worker thread. Block with `recv()`.
- **Don't hold the instance Mutex across the channel send.** Coordinator thread legitimately holds it for the scope lifetime, but per-pipeline tasks should `pm.lock()` only their own per-pipeline mutex (verified safe today: `pipeline_manager.rs:386` already does this in `advance_instance`).
- **Don't make `StreamItem::Error` recoverable.** Per D-11, an `Error` chunk means the remaining stream is best-effort — the channel may close immediately after. Don't try to keep iterating past an error.
- **Don't change `encode_advance_result_buf` to share code with `encode_chunk_buf`.** Per COMPAT-03, the buffered format is byte-for-byte locked. Encoders share helpers (`encode_str`, `encode_json_value`) but the framing functions are independent.

## Don't Hand-Roll

| Problem                             | Don't Build                                                     | Use Instead                                                             | Why                                                                                                                                                                      |
| ----------------------------------- | --------------------------------------------------------------- | ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Async iterator over Rust state      | Custom napi `JsObject` with manual `Symbol.asyncIterator` setup | `AsyncTask`-returning `next()` method; let TS wrap in `async function*` | napi-rs `AsyncTask` already handles Promise creation, libuv worker dispatch, and error mapping. JS-side `async function*` provides `Symbol.asyncIterator` automatically. |
| Cross-thread bounded channel        | `Mutex<VecDeque<StreamItem>>` + `Condvar`                       | `std::sync::mpsc::sync_channel(n)`                                      | Std lib already implements bounded MPSC with backpressure. No reason to reinvent.                                                                                        |
| Cancel flag                         | `Mutex<bool>`                                                   | `Arc<AtomicBool>`                                                       | Atomic load is lock-free; this gets checked frequently in tight loops.                                                                                                   |
| Panic catching in rayon             | Custom panic hook + thread-local error state                    | `std::panic::catch_unwind` per-task                                     | catch_unwind is the standard, well-understood primitive. Custom hooks affect global state.                                                                               |
| JS exception class hierarchy        | Inheritance chains                                              | Plain class extending Error, with `kind` discriminator (per D-10)       | Avoid TS class-instanceof brittleness across module boundaries; `kind` is a flat string discriminator.                                                                   |
| Multiset comparison for fuzz parity | Sort-then-zip                                                   | `compareChanges` from `dual-executor.ts` (already exists)               | Existing utility that handles ordering-insensitive comparison; reuse.                                                                                                    |

**Key insight:** napi-rs gives us _all_ the primitives we need (AsyncTask, Buffer, #[napi] class, Drop) — the streaming surface is "wire them together with a coordinator thread + std mpsc + AtomicBool." Resist the urge to add new dependencies; everything is already in the dep tree.

## Runtime State Inventory

> Phase 31 is **purely additive code** — no rename, no migration. No runtime state to inventory.

| Category            | Items Found                                                                                                                                                               | Action Required                                                                                                       |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Stored data         | None — no schema changes, no DB writes from streaming code                                                                                                                | None                                                                                                                  |
| Live service config | None — the napi binary is the only artifact, and Phase 30-05's `assertNapiBinaryFreshness` gate (per D-21) catches stale builds                                           | None                                                                                                                  |
| OS-registered state | None                                                                                                                                                                      | None                                                                                                                  |
| Secrets/env vars    | `FUZZ_NUM_RUNS` env var read by the new fuzz test (matches existing `fuzz-ivm.test.ts` convention)                                                                        | None — already supported                                                                                              |
| Build artifacts     | New `chunk_encoder.rs` source file; new napi exports (`AdvanceStream`, `HydrateStream`, `advanceStreaming`, etc.) generated into `index.d.ts` by napi-build at build time | Run `npm run build` in `packages/zqlite-rs/` after changes — already enforced by `assertNapiBinaryFreshness` per D-21 |

## Common Pitfalls

### Pitfall 1: Cancel-deadlock on full channel + slow JS consumer

**What goes wrong:** `mpsc::sync_channel(pipeline_count)` is bounded. If JS stops calling `next()` (e.g., it's awaiting CVR commit), all pipelines may finish, fill the channel, and block on `tx.send`. If we then call `return_()` which sets the cancel flag, the senders are already past their cancel checks (they're inside `tx.send`).

**Why it happens:** `tx.send` blocks indefinitely until either (a) the receiver reads, or (b) the receiver is dropped. The cancel flag is checked at task entry and between operator pushes — NOT inside `tx.send`.

**How to avoid:**

1. Per D-15, the TS wrapper MUST call `stream.return_()` in `finally`. That alone isn't enough; we also need the receiver to be drained or dropped.
2. Add a drain step inside `return_()`: after setting cancel, also drain any pending items from the receiver in a non-blocking way (or simply drop-and-replace the receiver). This unblocks any stuck senders, which then exit normally.
3. **Cleaner alternative:** Use `tx.try_send` instead of `tx.send` after the first chunk is enqueued, and on `TrySendError::Full` check the cancel flag, sleep briefly, and retry. Adds latency on full channel but avoids deadlock.
4. **Cleanest alternative:** Use `std::sync::mpsc::sync_channel(pipeline_count + 1)` so the channel always has at least one slot for the final ResetSignal/error item from the coordinator. (Trivial cost; large robustness win.)

**Warning signs:** Test hangs in CI; `pipeline-driver.*.test.ts` time out only when streaming methods are exercised. Mitigation: TEST-05 should explicitly construct this scenario (slow JS, fast pipelines, then break out of the loop) and assert termination within a reasonable timeout.

### Pitfall 2: `Drop` not called when JS process exits

**What goes wrong:** napi-rs documents that `Drop` runs when the JS object is garbage collected. If the process exits while an `AdvanceStream` is alive (held by a closure, a still-running view-syncer, etc.), the GC may never run, and the coordinator thread may keep running until the process is killed. Worse — the coordinator may try to `tx.send` on a dropped sender or hold the instance Mutex on a dying process.

**Why it happens:** napi-rs's class finalize hooks run via Node-API's `napi_finalize`, which is invoked by V8's GC. Process exit doesn't always GC; it may just abort.

**How to avoid:**

1. Phase 31 doesn't need to fully solve this — view-syncer (Phase 32) owns the lifecycle and can call `return_()` on shutdown.
2. For Phase 31 tests: ensure tests fully consume each stream (or call `return_()` explicitly) before the test ends. Vitest's parallel test isolation means a leaked thread won't block tests, but it can leak file handles.
3. Document in the `AdvanceStream` doc comment: "Callers must call `return_()` explicitly on graceful shutdown; Drop is a safety net for GC, not a guarantee for process exit."

**Warning signs:** Test process doesn't exit cleanly; CI process hangs. Run `cargo test --release -- --test-threads 1 --nocapture` if you suspect leaks.

### Pitfall 3: Rayon scope panics propagate to coordinator thread

**What goes wrong:** `rayon::scope(|s| { ... })` documentation says: "If a panic occurs, either in the closure given to scope() or in any of the spawned jobs, that panic will be propagated and the call to scope() will panic." So if `catch_unwind` is missing or wraps something that doesn't actually catch (e.g., `Mutex` poisoning during a panic), the scope panics, the coordinator thread panics, and the std::thread spawn — well, panicking in a spawned thread doesn't take down the process, but the coordinator thread dies and `tx` is dropped.

**Why it happens:** The plan calls for `catch_unwind` per task. If we mess up the AssertUnwindSafe wrapping (e.g., capturing a `&mut` that doesn't impl `UnwindSafe`), Rust will refuse to compile or the catch will be ineffective.

**How to avoid:**

1. Use `std::panic::AssertUnwindSafe(|| { ... })` per the example above. This is the standard idiom for telling the borrow checker "I know this isn't UnwindSafe, but I'm choosing to catch the panic anyway."
2. Inside the catch_unwind, don't unwind through ANY mutex guards — drop the guard before triggering an Ok branch, or accept that a panic poisons the per-pipeline mutex (which is fine because we're failing the whole stream anyway).
3. Test: `TEST-03` panics one pipeline; sibling pipelines must still succeed. This explicitly verifies the panic isolation works.

**Warning signs:** `TEST-03` shows "thread '<unnamed>' panicked at ..." in test output and the test process aborts. Indicates the catch_unwind is broken or absent.

[VERIFIED: docs.rs/rayon scope documentation]

### Pitfall 4: Encoding companion rows AFTER permission filter

**What goes wrong:** STREAM-05 says companion row changes arrive as one final `StreamItem::Chunk`. STREAM-06 / D-20 says permission filter runs per-chunk inside the rayon task. The companion check happens AFTER the rayon scope, so its output bypasses the per-task filter loop. If we forget to filter companion rows, permission table rows leak through (security regression).

**Why it happens:** Easy to look at `apply_permission_and_version_filters` and think "rayon tasks call this; we're done." Companions are a separate code path.

**How to avoid:** In the post-scope companion handling, also call `apply_permission_and_version_filters(&mut companion_changes, &instance.permission_tables, &instance.syncable_tables)` before encoding. Mirror exactly what `advance_instance` does today (it filters the combined Vec which includes companion rows because they're appended via `extend`).

**Warning signs:** Parity fuzz (D-07) detects extra rows from the streaming path that the buffered path doesn't produce, OR existing pipeline-driver tests that use permission tables fail when run against streaming.

### Pitfall 5: `encode_advance_result_buf` accidentally shared with chunk encoder

**What goes wrong:** Tempting to refactor the buffered encoder to call the chunk encoder + add framing. Per COMPAT-03, the byte format of `encode_advance_result_buf` is locked. Any refactor risks changing it.

**How to avoid:** The two encoders share lower-level helpers (`encode_str`, `encode_json_value` — already `pub(crate)` per `advance.rs:822-820`) but their framing layers are independent. Don't refactor `encode_advance_result_buf`. Add a golden-bytes test in 31-01 that pickles a known `AdvanceResult` and asserts the byte hash is identical to a pre-Phase-31 baseline. (Or just trust the existing `decode-advance-buf.test.ts` — it's a structural test, but if buffered encoding stays untouched it remains green.)

### Pitfall 6: `Arc<Mutex<Receiver>>` pattern subtleties

**What goes wrong:** napi-rs may call `next()` from JS in rapid succession (some JS code does `const [a, b] = await Promise.all([s.next(), s.next()])`). Two AsyncTasks would both block on `rx.lock()` then `rx.recv()`; this works but the second recv returns the second item. JS code should NOT do this — async iterator protocol assumes serial `next()` — but our Rust code shouldn't crash if it happens.

**How to avoid:** The `Arc<Mutex<Receiver>>` pattern naturally serializes — second AsyncTask waits for the Mutex. Each gets a separate `recv()` and a separate item. JS code that violates the protocol gets undefined ordering; that's its bug, not ours.

**Warning signs:** Tests with parallel `Promise.all` over the same stream produce out-of-order results. Document in JSDoc: "Iterate sequentially; do not call next() concurrently."

### Pitfall 7: napi-rs `#[napi]` method names ending in underscore

**What goes wrong:** Rust `return` is a reserved word, so we name the method `return_`. napi-rs auto-converts to `return_` in JS too — but JS code would prefer `return` to match the iterator protocol.

**How to avoid:** Either (a) live with `stream.return_()` (slightly off-spec but documented), or (b) use `#[napi(js_name = "return")]` to override the JS name. Per the iterator protocol, JS expects `iterator.return()`. Per D-15, the wrapper calls `stream.return_()` — but the wrapper is internal; we can rename to `return` at the napi layer. Recommend `#[napi(js_name = "return")]` for cleaner JS API. Verify this doesn't break napi-rs codegen; if it does, fall back to `return_`. [ASSUMED — needs verification at build time]

## Code Examples

### Per-chunk decoder (TS) — strict subset of buffered decoder

```ts
// Source: derived from packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts
//         (lines 67-163, decodeAdvanceResultBuf), stripped of error-flag,
//         timings, and reset_signal trailers per D-03 chunk format.
// [ASSUMED — derived from existing decoder + plan §7]

/**
 * Decode a per-pipeline chunk buffer from advance_streaming / hydrate_streaming.
 *
 * Binary format (little-endian):
 *   [u32] change_count
 *   [u8]  flags  (RESERVED — must be 0 in v1; future versions may set bits
 *                  for per-pipeline timing telemetry. Decoder MUST throw on
 *                  non-zero to force decoder upgrade. See IVM-STREAMING-PLAN.md
 *                  §10 Phase D.)
 *   Per RowChange: identical encoding to decodeAdvanceResultBuf — see that
 *                  function for the per-row layout.
 *
 * NOTE: chunk format does NOT include error flags or reset_signal trailers.
 * Errors and resets travel as separate StreamItem variants (StreamItem::Error,
 * StreamItem::ResetSignal) outside the chunk encoding.
 */
export function decodeAdvanceChunkBuf(buf: Buffer): DecodedRowChange[] {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  let offset = 0;

  const changeCount = view.getUint32(offset, true);
  offset += 4;

  const flags = buf[offset++];
  if (flags !== 0) {
    // D-04: fail loud on unknown flags rather than silently misinterpret.
    throw new Error(
      'decodeAdvanceChunkBuf: unexpected non-zero flags byte ' +
        `(got 0x${flags.toString(16)}; need decoder upgrade for new format bits)`,
    );
  }

  const changes: DecodedRowChange[] = Array.from({length: changeCount});
  for (let i = 0; i < changeCount; i++) {
    const ct = buf[offset++];
    let queryID: string;
    [queryID, offset] = readStr(buf, view, offset);
    let table: string;
    [table, offset] = readStr(buf, view, offset);
    let rowKey: unknown;
    [rowKey, offset] = readJsonValue(buf, view, offset);

    const hasRow = buf[offset++];
    let row: Record<string, unknown> | null = null;
    if (hasRow) {
      const colCount = view.getUint16(offset, true);
      offset += 2;
      row = {};
      for (let c = 0; c < colCount; c++) {
        let colName: string;
        [colName, offset] = readStr(buf, view, offset);
        let colValue: unknown;
        [colValue, offset] = readJsonValue(buf, view, offset);
        row[colName] = colValue;
      }
    }

    changes[i] = {
      queryID,
      table,
      row_key: rowKey,
      row,
      type: CHANGE_TYPES[ct] ?? 'other',
    };
  }
  return changes;
}
```

### TS streaming wrapper (advanceStreaming)

```ts
// Source: derived from packages/zero-cache/src/services/view-syncer/pipeline-driver.ts
//         #rustAdvanceAsync (lines 1375-1443) — reuses snapshotter, swapSnapshot,
//         setPermissionTables, #convertDispatchChanges, but iterates a stream
//         instead of buffering.
// [ASSUMED — derived from existing wrapper + plan]

async advanceStreaming(
  timer: Timer,
  _vsId?: string | undefined,
): Promise<{
  version: string;
  numChanges: number;
  changes: AsyncIterable<RowChange | 'yield'>;
}> {
  assert(this.initialized(), 'Pipeline driver must be initialized before advancing');
  assert(this.#manager, 'RustPipelineManager must be available — Rust is the sole advance path');

  const diff = this.#snapshotter.advance(this.#tableSpecs, this.#allTableNames);
  const {prev, curr, changes: numChanges} = diff;
  this.#lc.debug?.(
    `rust_advance_streaming ${prev.version} => ${curr.version}: ${numChanges} changes, ${this.#pipelines.size} pipelines`,
  );

  const collectedChanges: Array<{
    table: string;
    prevValues: ReadonlyArray<Readonly<Row>>;
    nextValue: Readonly<Row> | null;
    rowKey: unknown;
  }> = [];
  for (const change of diff) {
    collectedChanges.push(change);
  }

  this.#manager.swapSnapshot(this.#instanceId, curr.db.db.name);
  const permTables = this.#combinedPermissionTables();
  if (permTables && permTables.size > 0) {
    this.#manager.setPermissionTables(this.#instanceId, JSON.stringify([...permTables]));
  }

  for (const table of this.#tables.values()) {
    table.setDB(curr.db.db);
  }
  this.#ensureCostModelExistsIfEnabled(curr.db.db);

  const stream = this.#manager.advanceStreaming(
    this.#instanceId,
    JSON.stringify(collectedChanges),
  );

  return {
    version: curr.version,
    numChanges,
    changes: this.#streamChanges(stream, timer, numChanges),
  };
}

async *#streamChanges(
  stream: AdvanceStream,
  timer: Timer,
  numChanges: number,
): AsyncIterable<RowChange | 'yield'> {
  const totalHydrationTimeMs = this.totalHydrationTimeMs();
  this.#advanceContext = {timer, totalHydrationTimeMs, numChanges, pos: 0};
  try {
    while (true) {
      const item = await stream.next();
      if (item.done) break;

      switch (item.kind) {
        case 'reset':
          throw new ResetPipelinesSignal(item.reason!, 'scalar-subquery');
        case 'error':
          // D-10/D-11: Rust→TS error mapping
          throw new RustStreamError(
            item.error_msg!,
            item.error_kind as 'panic' | 'rayon_error' | 'channel_closed',
          );
        case 'chunk': {
          const decoded = decodeAdvanceChunkBuf(item.chunk!);
          for (const change of this.#convertDispatchChanges(decoded)) {
            // Same yield-token cadence as #wrapWithTimeout
            if (this.#shouldAdvanceYieldMaybeAbortAdvance()) {
              yield 'yield';
            }
            yield change;
            if (change !== 'yield') {
              this.#advanceContext!.pos++;
            }
          }
          break;
        }
      }
    }
  } finally {
    // D-15: belt-and-suspenders cancel.
    try { stream.return_(); } catch { /* swallow */ }
    this.#advanceContext = null;
  }
}
```

### Fast-check parity property (streaming-vs-buffered-parity.fuzz.test.ts)

```ts
// Source: pattern derived from packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts
//         (FUZZ_NUM_RUNS env var, fc.assert + fc.property shape) +
//         dual-executor.compareChanges utility.
// [ASSUMED — pattern derived from existing fuzz + plan D-07]

import fc from 'fast-check';
import {describe, expect, test} from 'vitest';
import {compareChanges} from './dual-executor.ts';
import {
  arbRow /* etc. — copy from fuzz-ivm.test.ts */,
} from './fuzz-ivm.test.ts';
// (or extract arbRow / arbScalar / arbFilterForRow into a shared file
//  if cleaner; that's a discretion call.)

const NUM_RUNS = parseInt(process.env['FUZZ_NUM_RUNS'] ?? '1000', 10);

describe('streaming-vs-buffered parity', () => {
  test('advance_streaming produces same RowChange multiset as advance_async', async () => {
    await fc.assert(
      fc.asyncProperty(
        // Generate: a small schema, a query set, an initial row population,
        // and a sequence of changes. Sized small to keep iteration fast.
        // Reuse arbitraries from fuzz-ivm.test.ts where applicable.
        arbScenario, // see helper below
        async scenario => {
          const driverA = await buildDriver(scenario);
          const driverB = await buildDriver(scenario); // identical state

          // Buffered path
          const bufferedResult = await driverA.advanceAsync(NO_TIME_TIMER);
          const bufferedChanges = materializeChanges(bufferedResult.changes);

          // Streaming path
          const streamingResult = await driverB.advanceStreaming(NO_TIME_TIMER);
          const streamingChanges: RowChange[] = [];
          try {
            for await (const c of streamingResult.changes) {
              if (c !== 'yield') streamingChanges.push(c);
            }
          } finally {
            // belt-and-suspenders, even though for-await consumed naturally
          }

          const cmp = compareChanges(bufferedChanges, streamingChanges);
          if (!cmp.match) {
            throw new Error(
              `Parity mismatch:\n  scenario: ${JSON.stringify(scenario)}\n` +
                `  mismatches: ${JSON.stringify(cmp.mismatches, null, 2)}`,
            );
          }
        },
      ),
      {numRuns: NUM_RUNS},
    );
  });
});
```

## State of the Art

| Old Approach                                                                                     | Current Approach                                                                                                            | When Changed    | Impact                                                                                                                                                                                              |
| ------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------- | --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `advance_async` returns `Promise<Buffer>` containing all pipeline output materialized as one Vec | `advance_streaming` returns `AdvanceStream` (napi class) whose `next()` yields per-pipeline chunks                          | Phase 31 (this) | TTFB drops from `max(pipeline_time)` to `min(pipeline_time)` for the migrated consumer (Phase 32). Memory footprint drops from `O(total_changes)` to `O(max_pipeline_changes)` (verified Phase 33). |
| TS `#wrapWithTimeout` only stops _processing_ on timeout; Rust has already finished its work     | TS streaming wrapper calls `stream.return_()` on timeout → Rust observes cancel flag and exits in-flight pipelines mid-push | Phase 31 (this) | Rust work actually stops. Bounded by single-pipeline push time (same as today's `#shouldAdvanceYieldMaybeAbortAdvance`).                                                                            |

**Deprecated/outdated:** Nothing deprecated in Phase 31. `advance` / `advanceAsync` / `addQueriesAsync` remain supported indefinitely (per `IVM-STREAMING-PLAN.md §8`).

## Assumptions Log

| #   | Claim                                                                                                                                                                                          | Section                             | Risk if Wrong                                                                                                                                                                                                                                                                                       |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- | ---------------------------------------------- |
| A1  | `AsyncTask::compute()` runs on a libuv worker thread that permits blocking calls (i.e., `mpsc::Receiver::recv()` blocking is OK)                                                               | Pattern 1, Standard Stack           | LOW — official napi-rs docs explicitly state compute() runs on libuv thread for "heavy computation without blocking JavaScript." Blocking on a channel is the exact intended use. If wrong, fallback is to use try_recv in a sleep loop.                                                            |
| A2  | `std::thread::spawn` from inside a `#[napi]` method body is safe (won't poison napi runtime)                                                                                                   | Pattern 2, advance_streaming sketch | LOW — std::thread is a normal OS thread; napi-rs only restricts use of `napi::Env` / JS values across threads. Coordinator thread does NOT touch JS values; it only sends via mpsc and the AsyncTask resolve() handles JS conversion.                                                               |
| A3  | `std::panic::AssertUnwindSafe` correctly catches panics from `pipeline.lock().unwrap()` and the Rust IVM operator code                                                                         | Pattern 2, Pitfall 3                | MEDIUM — most operator code is panic-clean (returns Result). The new asserts in Phase 30-04 (AUDIT-03) deliberately panic on framework invariant violations; those are exactly the panics we want to catch and surface as `StreamItem::Error('panic', ...)`. Test TEST-03 verifies this end-to-end. |
| A4  | Drop on a `#[napi]` class is invoked when JS GC's the wrapping object, even if the Rust struct holds a `JoinHandle` to a still-running thread                                                  | Pattern 1, Pitfall 2                | MEDIUM — verified via napi-rs class docs, but timing of GC is non-deterministic. The plan handles this by making `return_()` the primary path (D-14/D-15). Drop is fallback.                                                                                                                        |
| A5  | `#[napi(js_name = "return")]` works to expose a JS method named `return` from a Rust method named `return_`                                                                                    | Pitfall 7                           | LOW — napi-rs `js_name` attribute is a documented feature; if it doesn't work for reserved JS words, fall back to `return_`. Cheap to test at build time.                                                                                                                                           |
| A6  | Reusing existing `apply_permission_and_version_filters` (in `pipeline_manager.rs:588-619`) on a per-pipeline `Vec<RowChange>` produces the same output as applying it once on the combined Vec | STREAM-06 / D-20                    | LOW — function is purely per-row (`changes.retain_mut(...)`); no cross-row state. Mathematically identical. But explicitly note: companion rows must ALSO be filtered (Pitfall 4).                                                                                                                  |
| A7  | The Rust `chunk_encoder.rs` can re-export `pub(crate) encode_str` / `encode_json_value` from `advance.rs` without circular module imports                                                      | D-19                                | LOW — both files are siblings under `crate::`, no circular dep. If the helpers need to move, that's a trivial refactor.                                                                                                                                                                             |
| A8  | `compareChanges` from `dual-executor.ts` already implements ordering-insensitive multiset comparison suitable for streaming-vs-buffered parity                                                 | TEST-04                             | LOW — verified by reading `dual-executor.ts:261-271` (materializeChanges) + the fact that `fuzz-ivm.test.ts` uses it. The comparator handles `kind: 'ts_only'                                                                                                                                       | 'rust_only' | 'row_diff'` mismatches — exactly what we need. |
| A9  | `mpsc::sync_channel(pipeline_count.max(1))` does not deadlock on the final ResetSignal/error item from the coordinator                                                                         | Pitfall 1                           | MEDIUM — covered by recommendation to use `pipeline_count + 1` capacity (cheap robustness win), or by draining inside `return_()`. Planner should pick one approach explicitly.                                                                                                                     |
| A10 | The chunk format byte `[u32 count][u8 0]` is interpreted as little-endian (matches existing buffered format)                                                                                   | D-03, decoder example               | LOW — existing decoder uses little-endian throughout (`view.getUint32(offset, true)` per `decode-advance-buf.ts:71`). Streaming chunks must match.                                                                                                                                                  |

## Open Questions

1. **Should the AdvanceStream `Drop` impl join the coordinator thread?**
   - What we know: D-14 says Drop sets the cancel flag. The coordinator may still be inside `rayon::scope(|s| { ... })` waiting for the last pipeline; cancel makes new tasks short-circuit but in-flight pipelines finish their current change.
   - What's unclear: Does Drop block JS GC waiting for the coordinator to finish? If yes, that's a problem (GC pauses).
   - Recommendation: Don't join. Set cancel flag, let coordinator finish naturally on its own thread. The senders fail with `SendError` once the receiver (held in Arc<Mutex>) is dropped; coordinator exits cleanly. Document the non-deterministic shutdown ordering in a comment.

2. **What's the right cadence for `cancel.load()` checks inside `advance_persistent_pipeline`?**
   - What we know: STREAM-04 says "between operator pushes." The natural boundary is between iterations of `for change in changes.iter()` at `advance.rs:1261`.
   - What's unclear: Is once per _change_ sufficient, or do we also need checks inside `push_through_ptrs` between operators (`advance.rs:1231-1242`)?
   - Recommendation: Start with once-per-change (cheap, single check). If TEST-01 shows >cancelled+1 chunks delivered for big-pipeline scenarios, escalate to once-per-operator. Document the contract: "Cancellation is observed at change boundaries; a single change with N operators may complete before observation."

3. **Should `advance_streaming` accept the `numChanges = 0` case eagerly?**
   - What we know: Buffered `advance_instance` does `if changes.is_empty() { return early }` (`pipeline_manager.rs:380-382`).
   - What's unclear: For streaming with 0 changes, do we still spawn the coordinator + scope just to send "done"?
   - Recommendation: Mirror the early-return; if `changes.is_empty()`, return an `AdvanceStream` whose channel is already closed (drop sender immediately). First `next()` returns `done: true`. Saves a thread spawn for no work.

4. **Companion-bearing queries in `addQueriesStreaming` — what does "fall back to TS hydrate" look like in async generator form?**
   - What we know: WRAP-03 says fall back to TS hydrate via the same `rustEligible` check as `addQueriesAsync`.
   - What's unclear: `addQueriesAsync` currently returns `Promise<Iterable<RowChange | 'yield'>>` — synchronous iterable, materialized in memory. For streaming, do we yield TS-hydrated results inline alongside Rust streaming results, or do we drain TS first then start Rust?
   - Recommendation: For Phase 31, drain TS hydrate first (synchronous) and yield those as `RowChange` items, then start Rust streaming and yield those. Same final ordering as `addQueriesAsync`. Per D-09, existing tests must pass — this preserves their ordering expectation. Phase 32's view-syncer can revisit if it needs interleaved emission.

5. **Does `napi9` feature require any additional Cargo.toml changes for `AsyncTask` returning a Promise from a method?**
   - What we know: `AsyncTask` works with `napi9 + compat-mode` — the existing `advance_async` proves it (no `async` feature needed). [VERIFIED: Cargo.toml + `advance_async` works in v4.0]
   - What's unclear: Do we need to bump napi feature flags for the `#[napi(ts_return_type = ...)]` annotation if we want a custom TypeScript signature for `next()` returning `Promise<NextChunkValue>`?
   - Recommendation: Try without changes first. The existing `advance_async` already uses `ts_return_type` annotation (`pipeline_manager.rs:312`). Should work identically for streaming methods.

## Environment Availability

| Dependency           | Required By                                  | Available | Version                  | Fallback |
| -------------------- | -------------------------------------------- | --------- | ------------------------ | -------- |
| Rust toolchain       | Building zqlite-rs napi binary               | ✓         | edition 2021             | —        |
| `cargo`              | `cargo test --release` per D-22              | ✓         | (whatever Phase 30 used) | —        |
| `napi-build` 2       | Build script                                 | ✓         | per Cargo.toml           | —        |
| Node.js              | Running vitest, loading napi binary          | ✓         | (project minimum)        | —        |
| `npm`                | Running test commands                        | ✓         | —                        | —        |
| `vitest` 4.1.3       | Running TS tests                             | ✓         | per package.json root    | —        |
| `fast-check` ^3.18.0 | Running streaming-vs-buffered-parity fuzz    | ✓         | per package.json root    | —        |
| `tempfile` 3         | Rust dev-dependency for in-test SQLite paths | ✓         | per Cargo.toml           | —        |

**Missing dependencies with no fallback:** None.

**Missing dependencies with fallback:** None.

All required tooling is present (verified by reading Cargo.toml and the Phase 30 completion of `cargo test --release` runs).

## Validation Architecture

### Test Framework

| Property                  | Value                                                                                                                                                                                                                                                             |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TS framework              | vitest 4.1.3                                                                                                                                                                                                                                                      |
| Rust framework            | `cargo test` (built-in)                                                                                                                                                                                                                                           |
| Config files              | `packages/zero-cache/vitest.config.ts`, `packages/zqlite-rs/Cargo.toml`                                                                                                                                                                                           |
| Quick run command (TS)    | `npx vitest run packages/zero-cache/src/services/view-syncer/<file>.test.ts`                                                                                                                                                                                      |
| Quick run command (Rust)  | `cargo test --release -p zqlite-rs <test_name>`                                                                                                                                                                                                                   |
| Full suite command (TS)   | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.*.test.ts && npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts && npx vitest run packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts` |
| Full suite command (Rust) | `cargo test --release` in `packages/zqlite-rs/` and `packages/zero-ivm-rs/`                                                                                                                                                                                       |
| Phase gate                | All of the above green; plus the new `streaming-vs-buffered-parity.fuzz.test.ts` with FUZZ_NUM_RUNS=1000                                                                                                                                                          |

### Phase Requirements → Test Map

| Req ID    | Behavior                                                                 | Test Type                        | Automated Command                                                                                                                                                       | File Exists?         |
| --------- | ------------------------------------------------------------------------ | -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------- |
| STREAM-01 | `advance_streaming` emits one chunk per pipeline                         | unit (Rust)                      | `cargo test --release -p zqlite-rs advance_streaming_emits_per_pipeline_chunks`                                                                                         | ❌ Wave 0 (in 31-01) |
| STREAM-02 | `hydrate_streaming` / `hydrate_query_streaming` symmetric                | unit (Rust)                      | `cargo test --release -p zqlite-rs hydrate_streaming_emits_per_pipeline_chunks`                                                                                         | ❌ Wave 0 (in 31-01) |
| STREAM-03 | Per-pipeline rayon tasks lock only own mutex                             | unit (Rust)                      | `cargo test --release -p zqlite-rs streaming_tasks_lock_only_own_pipeline`                                                                                              | ❌ Wave 0 (in 31-01) |
| STREAM-04 | Cancel flag observed between operator pushes                             | unit (Rust) — same as TEST-01    | `cargo test --release -p zqlite-rs cancel_stops_in_flight_pipelines`                                                                                                    | ❌ Wave 0 (in 31-01) |
| STREAM-05 | Companion check after join; reset/companion-rows arrive last             | unit (Rust) — partial in TEST-02 | `cargo test --release -p zqlite-rs companion_reset_arrives_after_pipelines`                                                                                             | ❌ Wave 0 (in 31-01) |
| STREAM-06 | Per-chunk permission filter + minRowVersion bump                         | unit (Rust)                      | `cargo test --release -p zqlite-rs per_chunk_permission_filter`                                                                                                         | ❌ Wave 0 (in 31-01) |
| WRAP-01   | `decodeAdvanceChunkBuf` decodes correctly + throws on non-zero flags     | unit (TS)                        | `npx vitest run packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts -t 'decodeAdvanceChunkBuf'`                                                     | ❌ Wave 0 (in 31-02) |
| WRAP-02   | `pipeline-driver.advanceStreaming` exists + matches signature            | unit + integration (TS)          | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts`                                                                         | ❌ Wave 0 (in 31-02) |
| WRAP-03   | `addQueriesStreaming` exists + companion fallback                        | unit + integration (TS)          | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts -t 'addQueriesStreaming'`                                                | ❌ Wave 0 (in 31-02) |
| WRAP-04   | TS wrapper throws ResetPipelinesSignal correctly                         | unit (TS)                        | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts -t 'reset signal'`                                                       | ❌ Wave 0 (in 31-02) |
| COMPAT-01 | All existing tests pass unchanged                                        | regression                       | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.*.test.ts && npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` | ✅                   |
| COMPAT-02 | Buffered method signatures unchanged                                     | byte-diff (manual / lint)        | `git diff vN/v4.0..HEAD -- packages/zqlite-rs/src/pipeline_manager.rs packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` (manual review)                  | — (manual gate)      |
| COMPAT-03 | `encode_advance_result_buf` / `decodeAdvanceResultBuf` formats unchanged | regression                       | `npx vitest run packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts`                                                                                | ✅                   |
| TEST-01   | Cancel after few pipelines → ≤cancelled+1 chunks                         | unit (Rust)                      | `cargo test --release -p zqlite-rs streaming_cancellation_bounded_chunks`                                                                                               | ❌ Wave 0 (in 31-01) |
| TEST-02   | Companion scalar reset closes channel                                    | unit (Rust)                      | `cargo test --release -p zqlite-rs streaming_companion_reset_closes_channel`                                                                                            | ❌ Wave 0 (in 31-01) |
| TEST-03   | Panic isolation, panic surfaces as Error                                 | unit (Rust)                      | `cargo test --release -p zqlite-rs streaming_panic_in_one_pipeline_others_complete`                                                                                     | ❌ Wave 0 (in 31-01) |
| TEST-04   | Streaming-vs-buffered parity (1k fuzz)                                   | property (TS)                    | `FUZZ_NUM_RUNS=1000 npx vitest run packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts`                                              | ❌ Wave 0 (in 31-02) |
| TEST-05   | `for await { break }` calls `return_()` and stops Rust work              | integration (TS)                 | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts -t 'iterator return cancels rust'`                                       | ❌ Wave 0 (in 31-02) |

### Sampling Rate

The streaming behavior under test produces signals at three frequencies:

- **Chunk arrival** — milliseconds per chunk. Sampled implicitly by every `next()` call in TEST-04 fuzz (1k runs × ~3 pipelines = ~3k chunks per fuzz run).
- **Cancellation point** — once per stream lifetime. TEST-01 + TEST-05 sample explicitly.
- **Panic timing** — once per stream lifetime, at varying positions. TEST-03 samples at one fixed position; reasonable given Nyquist (one panic per stream is a single event, not a continuous signal).

**Per task commit:** `cargo test --release -p zqlite-rs streaming_` (only streaming-related tests, ~5 tests) + `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts` (only new streaming tests, ~10 tests). Should run in <30s.

**Per wave merge:** Full `pipeline-driver.*.test.ts` + `fuzz-ivm.test.ts` (1k iterations) + `decode-advance-buf.test.ts` + `cargo test --release` for both Rust crates + new `streaming-vs-buffered-parity.fuzz.test.ts` (1k iterations).

**Phase gate:** Full suite green before `/gsd-verify-work`. Manual byte-diff review for COMPAT-02.

### Wave 0 Gaps

- [ ] `packages/zqlite-rs/src/pipeline_manager.rs` — `#[cfg(test)] mod streaming_tests { ... }` covering TEST-01, TEST-02, TEST-03, plus STREAM-{01..06} unit tests. (~6 new test functions in 31-01.)
- [ ] `packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts` — extend with `describe('decodeAdvanceChunkBuf', () => { ... })` block covering happy path, non-zero-flags throw (D-04), empty chunk, single-row chunk.
- [ ] `packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts` — NEW file covering WRAP-01..04 + TEST-05.
- [ ] `packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts` — NEW file (D-07) covering TEST-04.
- [ ] `RustStreamError` class export from `pipeline-driver.ts` (D-10).

_(Framework install: not needed — vitest, fast-check, cargo all already present.)_

## Sources

### Primary (HIGH confidence)

- **Read** `.planning/IVM-STREAMING-PLAN.md` — full streaming plan §1-10 (in-repo, canonical design source)
- **Read** `.planning/REQUIREMENTS.md` — STREAM-01..06, WRAP-01..04, COMPAT-01..03, TEST-01..05
- **Read** `.planning/phases/31-streaming-primitives-and-wrappers/31-CONTEXT.md` — D-01..D-22 (the locked decisions)
- **Read** `packages/zqlite-rs/Cargo.toml` — verified napi-rs 3 + napi9 + compat-mode, no tokio, rayon 1.10
- **Read** `packages/zqlite-rs/Cargo.lock` — verified `napi = 3.8.5`, `napi-derive = 3.5.4`, no tokio package present
- **Read** `packages/zqlite-rs/src/pipeline_manager.rs` — entire file (existing AsyncTask pattern at lines 312-373, 459-533; existing `advance_instance`, `apply_permission_and_version_filters`, `check_companions_and_emit`)
- **Read** `packages/zqlite-rs/src/advance.rs` — lines 787-873 (encoders), 1205-1428 (advance_persistent_pipeline)
- **Read** `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — lines 60-77 (NapiPipelineManager iface), 220-300, 780-980 (addQueriesAsync), 1300-1520 (advance/advanceAsync/#rustAdvanceAsync), 1780-1850 (#wrapWithTimeout)
- **Read** `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` — entire 234 lines (decoder pattern to mirror)
- **Read** `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — entire 344 lines (fuzz pattern to mirror)
- **Read** `packages/zero-cache/src/services/view-syncer/snapshotter.ts:266-274` — `ResetPipelinesSignal` definition
- **Read** `packages/zero-cache/src/services/view-syncer/dual-executor.ts:258-271` — `materializeChanges` + `compareChanges`
- **Read** `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts` — integration test pattern reference
- [napi.rs/docs/concepts/async-task](https://napi.rs/docs/concepts/async-task) — Task trait + AsyncTask::new pattern
- [napi.rs/docs/concepts/class](https://napi.rs/docs/concepts/class) — `#[napi]` class + Drop on GC + ObjectFinalize
- [napi.rs/docs/concepts/async-fn](https://napi.rs/docs/concepts/async-fn) — confirmed `async fn` requires `async` feature (which pulls tokio); NOT what we want for Phase 31
- [docs.rs/napi Generator trait](https://docs.rs/napi/latest/napi/bindgen_prelude/trait.Generator.html) — confirmed Generator is sync + experimental; NOT applicable here

### Secondary (MEDIUM confidence)

- [napi.rs/docs/concepts/threadsafe-function](https://napi.rs/docs/concepts/threadsafe-function) — confirmed TSFN is for Rust→JS callback push, NOT for our pull-based async iterator pattern
- [users.rust-lang.org rayon catching panic](https://users.rust-lang.org/t/rayon-catching-panic-from-par-iter/35136) — confirmed `catch_unwind` per spawn closure is the standard idiom
- [docs.rs/rayon scope](https://docs.rs/rayon/latest/rayon/fn.scope.html) — confirmed `rayon::scope` joins all spawned tasks before returning; panic in any task propagates to scope() caller
- [github.com/rayon-rs/rayon issue #638](https://github.com/rayon-rs/rayon/issues/638) — discussion of panic semantics in par_iter (confirms `par_iter` does NOT auto-stop on panic; relevant for understanding the explicit `catch_unwind` requirement)
- [doc.rust-lang.org std::panic::catch_unwind](https://doc.rust-lang.org/std/panic/fn.catch_unwind.html) — confirmed Any payload + downcast pattern for extracting panic message

### Tertiary (LOW confidence)

- [users.rust-lang.org "How to correctly exit the thread blocking on mpsc::Receiver"](https://users.rust-lang.org/t/how-to-correctly-exit-the-thread-blocking-on-mpsc-receiver/2276) — referenced for the receiver-drop-unblocks-sender pattern (Pitfall 1 mitigation)

## Metadata

**Confidence breakdown:**

- Standard stack (napi-rs 3 + AsyncTask + std::sync::mpsc + rayon): **HIGH** — all primitives already exist in dep tree and are exercised by the existing `advance_async` / `hydrate_async` code paths. The streaming pattern is a mechanical composition.
- Architecture (coordinator thread + scope + per-chunk send): **MEDIUM** — no in-repo precedent for the exact composition; no external napi-rs example for "AsyncTask returning Promise N times to drive a JS asyncIterator." However, the components are individually documented and the composition follows directly from `IVM-STREAMING-PLAN.md §3-4`. Plan should validate the pattern works in 31-01's first commit (RED test → minimal GREEN scaffold).
- Pitfalls: **HIGH** — derived from documented behavior of std::mpsc, rayon, and napi-rs Drop semantics. Mitigations are concrete and testable.
- Test patterns: **HIGH** — fast-check fuzz pattern is a direct mirror of `fuzz-ivm.test.ts`; `compareChanges` exists; deterministic test scaffolding (TEST-01/02/03/05) follows existing pipeline-driver test conventions.

**Research date:** 2026-04-29
**Valid until:** 2026-05-29 (napi-rs 3 is stable; the rayon pattern is stable; only churn risk is napi-rs minor version bumps that might affect Generator trait or AsyncTask shape — neither of which affects this design directly).

## RESEARCH COMPLETE
