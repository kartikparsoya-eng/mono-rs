# Phase 31: Rust Streaming Primitives + TS Wrappers - Context

**Gathered:** 2026-04-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Add the full additive streaming surface to napi: Rust `advance_streaming` / `hydrate_streaming` / `hydrate_query_streaming` methods returning `AdvanceStream` / `HydrateStream` napi classes shaped as JS async iterators, plus TS `pipeline-driver` wrappers (`advanceStreaming`, `addQueriesStreaming`) and a per-chunk decoder (`decodeAdvanceChunkBuf`).

**Nothing in production code consumes these yet.** This phase covers IVM-STREAMING-PLAN.md Phases A and B, intentionally combined per the roadmap. Phase 32 owns view-syncer migration (Phase C in the doc).

**In scope (17 requirements):**

- STREAM-01..06: Rust streaming primitives (`advance_streaming`, `hydrate_streaming`, per-pipeline rayon, cancel via `Arc<AtomicBool>`, companion handling, permission/version filter per chunk)
- WRAP-01..04: TS decoder + pipeline-driver wrappers + reset/timeout throw shape
- COMPAT-01..03: All existing tests pass unchanged; buffered methods byte-for-byte unchanged; `encode_advance_result_buf` / `decodeAdvanceResultBuf` formats unchanged
- TEST-01..05: cancellation, reset, panic-isolation, parity, `return_()` integration tests

**Out of scope:**

- View-syncer migration (Phase 32, MIGRATE-01/02)
- Performance benchmarks / channel-size tuning (Phase 33, PERF-01/02 — Phase D in the doc)
- Any change to existing buffered methods or wire formats
- Per-pipeline timing telemetry (Phase 33; we reserve the chunk flags byte but don't emit telemetry now)

</domain>

<decisions>
## Implementation Decisions

### Plan Granularity

- **D-01:** Two plans, sequential waves. The Phase A / Phase B split from `IVM-STREAMING-PLAN.md` matches the natural compile-dependency boundary — TS literally cannot reference the napi `AdvanceStream` class until it exists. Split:
  - **31-01 (Wave 1, autonomous):** All Rust work in `packages/zqlite-rs` — chunk encoder, `StreamItem` enum, `AdvanceStream` / `HydrateStream` napi classes with `next()` + `return_()` + `Drop`-on-cancel, `advance_streaming` / `hydrate_streaming` / `hydrate_query_streaming` method bodies, all Rust unit tests covering TEST-01 / TEST-02 / TEST-03.
  - **31-02 (Wave 2, autonomous, depends_on 31-01):** All TS work in `packages/zero-cache` — `decodeAdvanceChunkBuf` in `decode-advance-buf.ts`, `pipeline-driver.ts::advanceStreaming` and `addQueriesStreaming`, `RustStreamError` class, all TS tests covering TEST-04 / TEST-05.
- **D-02:** No third plan. The work is tightly coupled within each language; finer granularity would increase GSD ceremony without improving rollback safety.

### Chunk Format Forward-Compat

- **D-03:** The `[u32 count][u8 0]` chunk header treats the second byte as a **reserved flags field** — not zero-padding. Document it explicitly in both Rust (`chunk_encoder` doc comment) and TS (`decodeAdvanceChunkBuf` JSDoc) as: "Reserved flags byte. Must be 0 in v1. Future versions (Phase 33) may set bits for per-pipeline timing telemetry; see `IVM-STREAMING-PLAN.md §10 Phase D`."
- **D-04:** `decodeAdvanceChunkBuf` MUST validate `flags === 0` and throw a clear error if not: `throw new Error('decodeAdvanceChunkBuf: unexpected non-zero flags byte (need decoder upgrade for new format bits)')`. This makes Phase 33 telemetry additions strictly additive — old decoders fail loudly rather than silently misinterpret subsequent bytes.
- **D-05:** Mirrors the pattern already established by `encode_advance_result_buf`'s flags byte — consistency, not speculation.

### Test Depth

- **D-06:** TEST-01 / TEST-02 / TEST-03 / TEST-05 stay **deterministic** — explicit barriers, fixed inputs, single-rayon-task scheduling where possible. Easier to reason about and CI-stable.
- **D-07:** **Add one new property-based fuzz test** — `packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts` — using `fast-check` with `FUZZ_NUM_RUNS=1000` (matches the existing `fuzz-ivm.test.ts` cadence). The property: for any random input changes accepted by `advance_async`, `advance_streaming` produces an equivalent `RowChange` multiset modulo cross-pipeline ordering.
- **D-08:** This single fuzz pins TEST-04's parity claim with stochastic depth. Catches multi-pipeline interleave bugs that fixed inputs would miss. Avoid fuzzing cancellation timing — those are easier to test with explicit barriers (TEST-01 / TEST-05).
- **D-09:** All existing tests must pass unchanged: `pipeline-driver.*.test.ts` (135 tests / 12 files), `fuzz-ivm.test.ts` 1k iterations, `decode-advance-buf.test.ts`, `cargo test --release` for both `zero-ivm-rs` and `zqlite-rs`. Verified via the same broader-glob check Phase 30-05 promoted to blocking.

### Error Surface Mapping (Rust → TS)

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
- **D-11:** `StreamItem::Error(message, type)` from Rust → `throw new RustStreamError(message, kind)`. The Rust `type` string is the `kind` field. Allowed kinds: `'panic'` (rayon task panicked), `'rayon_error'` (scope/spawn failure), `'channel_closed'` (sender dropped without expected close). Adding new kinds is additive — TS exhaustiveness checks must use a fallthrough `default` branch, not a type-narrowed switch with `never`.
- **D-12:** `ResetPipelinesSignal('scalar-subquery' | 'advancement-timeout')` throw shape is **unchanged** per WRAP-04. That class and its `reason` field are separate from `RustStreamError` — `ResetPipelinesSignal` is a recoverable signal (view-syncer rolls back the advance and retries), `RustStreamError` is a real failure (view-syncer logs + bubbles).
- **D-13:** Phase 32's view-syncer migration MUST use `error.kind === 'panic'` / `instanceof RustStreamError` for branching, not string-matching on `.message`. Calling out here so the planner for Phase 32 inherits this constraint.

### Cancellation & Lifetime

- **D-14:** `AdvanceStream::Drop` sets the cancel flag (per `IVM-STREAMING-PLAN.md §4`). This handles the GC-collected-half-consumed-iterator case — the stream's underlying `Drop` impl is the safety net, not the only cancellation path. Explicit `return_()` from JS is the primary path; Drop is the implicit cleanup.
- **D-15:** TS streaming wrapper MUST `try { for await ... } finally { stream.return_() }` to guarantee cancel-on-throw and cancel-on-break, even though Drop will eventually fire. Belt-and-suspenders pattern.

### Performance / Channel Sizing

- **D-16:** Phase 31 ships **bounded** `mpsc::sync_channel(pipeline_count)` from the start (per `IVM-STREAMING-PLAN.md §10 Phase D` recommendation). Reasoning: every pipeline can buffer one chunk without blocking, but stragglers don't accumulate unbounded. Defer further tuning to Phase 33 PERF-01.
- **D-17:** `pipeline_count` is read from `instance.pipelines.len()` after the brief instance-Mutex read. Don't hardcode a constant.

### Naming

- **D-18:** Rust napi method names use snake_case as napi-rs convention dictates: `advance_streaming`, `hydrate_streaming`, `hydrate_query_streaming`. TS-side names are camelCase per project convention: `advanceStreaming`, `addQueriesStreaming`. The napi-rs binding generates the camelCase JS facade automatically.

### Rust Crate Layout

- **D-19:** New types in `packages/zqlite-rs/src/`:
  - `chunk_encoder.rs` (new file) — `encode_chunk_buf(rows: &[RowChange]) -> Vec<u8>` + `StreamItem` enum + companion encode helpers. Reuses existing per-row encoders from `advance.rs` (`encode_str`, `encode_json_value`).
  - `pipeline_manager.rs` (existing) — `AdvanceStream`, `HydrateStream` napi-class definitions + `advance_streaming` / `hydrate_streaming` / `hydrate_query_streaming` methods on `RustPipelineManager`.
- **D-20:** Permission filter + minRowVersion bump logic stays where it is today in `advance.rs`; the streaming code factors out `apply_permission_and_version_filters(chunk: Vec<RowChange>) -> Vec<RowChange>` (or similar borrow-friendly signature) and calls it per-chunk inside the rayon task. Same logic, same output, just earlier scope.

### Build & Verification

- **D-21:** Phase 30-05's `assertNapiBinaryFreshness` gate already covers the new napi exports — when 31-01 lands new methods on `RustPipelineManager`, the freshness gate will fail-fast on stale binaries until the user runs `npm run build`. No new gate needed.
- **D-22:** Verifier MUST run `cargo test --release` (not just dev) to exercise any newly promoted `assert!` from Phase 30 in the streaming path (none expected, but cheap insurance).

### Claude's Discretion

- Specific rayon scope vs spawn pattern (helper thread owning a `rayon::scope`, vs `tokio::task::spawn_blocking` wrapping a `rayon::scope`, vs `napi::tokio_runtime::spawn`). The doc says "Spawn rayon scope inside a helper thread" — pick the pattern that matches existing `pipeline_manager.rs` patterns and minimizes thread overhead.
- Buffer ownership at the napi boundary (`Vec<u8>` vs `napi::bindgen_prelude::Buffer`) — pick whatever the existing buffered methods use for `Buffer` returns and match.
- Exact `'yield'` token cadence in the TS streaming wrapper — use the same time-based check as today's `#wrapWithTimeout`. If the streaming version naturally yields between chunks, that's fine; just match the existing throughput-vs-latency tradeoff.
- Internal error type for the Rust `panic::catch_unwind` shim that produces `StreamItem::Error('panic', ...)`. Pick whatever idiom the existing rayon-using code in the repo uses; if none, `std::panic::catch_unwind` + format the payload via `Any::downcast_ref::<&str>()` / `Any::downcast_ref::<String>()` is fine.

### Folded Todos

None — no pending todos relevant to this phase.

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Streaming design (primary source of truth)

- `.planning/IVM-STREAMING-PLAN.md` — full streaming plan §1-10. Phase 31 implements §7 Phase A + Phase B. §8 enumerates "things that intentionally don't change." §9 risks. §10 acceptance.
- `.planning/IVM-PORT-AUDIT.md` — referenced by the plan §8 (split_edit_keys is orthogonal to streaming, fixed in Phase 30).

### Existing surface to preserve (must not change)

- `packages/zqlite-rs/src/pipeline_manager.rs` — `RustPipelineManager` napi class. Streaming methods are NEW on this class; existing methods (`advance`, `advance_async`, `hydrate*`, `add_query*`, `set_table_specs`, `set_permission_tables`, `set_query_companions`, `swap_snapshot`, `pipeline_count`) must be byte-for-byte unchanged.
- `packages/zqlite-rs/src/advance.rs` — `advance_persistent_pipeline`, `apply_permission_and_version_filters` (extract for per-chunk reuse), `encode_advance_result_buf` (unchanged), `check_companions_and_emit` (called once globally after pipeline join in streaming path).
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — `advance` / `advanceAsync` / `addQuery` / `addQueries` / `addQueriesAsync` and the `rustEligible` check. New streaming methods sit alongside, not replace.
- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` — `decodeAdvanceResultBuf` (unchanged). New `decodeAdvanceChunkBuf` is a strict subset.

### Project conventions

- `./CLAUDE.md` — project instructions, oxlint/oxfmt, ESM, kebab-case files, `Disposable` pattern, `LogContext`, `.allBuf()` vs `.all()` rule.
- `./AGENTS.md` — TypeScript optional fields rule (`type | undefined` not `type?`), import patterns (no `mod.ts`, `import type`, no dynamic imports), package dependency hierarchy.
- `.planning/PROJECT.md` — core value: all SQLite I/O and row-level computation in Rust, multi-core parallelism. Streaming is the consumer-visible payoff for that parallelism.

### Phase 30 carry-forward (relevant invariants)

- `.planning/phases/30-audit-fixes/30-CONTEXT.md` — D-04..D-07 (AUDIT-02 split_edit_keys for EXISTS) is the precondition for any streaming Edit handling: `parent_field` doesn't change in an Edit reaching `ExistsOperator` because the source already split it.
- `.planning/phases/30-audit-fixes/30-05-SUMMARY.md` — `assertNapiBinaryFreshness` build-freshness gate; covers Phase 31's new exports automatically.
- `packages/zqlite-rs/src/hydrate.rs:769` — AUDIT-01 fix (LIKE case-sensitive). No streaming impact.

### Streaming test patterns

- `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — existing 1k-iteration `fast-check` fuzz. Mirror this pattern for the new `streaming-vs-buffered-parity.fuzz.test.ts`.
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts` — pattern for end-to-end TS-Rust integration tests; the freshness gate ensures we exercise current binary, not stale.

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- **Per-row encoders in `advance.rs`** (`encode_str`, `encode_json_value`, etc.) — reuse directly in `chunk_encoder.rs`. Per-row format is identical to the buffered format; only the chunk header differs (`[u32 count][u8 0]` vs the buffered header + flags byte + reset_signal trailer).
- **`apply_permission_and_version_filters`** (or its current equivalent in `advance.rs`) — extract or duplicate as `apply_permission_and_version_filters_to_chunk(rows: Vec<RowChange>) -> Vec<RowChange>`. Per `IVM-STREAMING-PLAN.md §6`, the operation is stateless per-row → mathematically identical when applied per-chunk vs combined-Vec.
- **`check_companions_and_emit`** — called once globally after pipeline tasks join. No change to its body; only the calling location moves to the post-join coordinator.
- **napi-rs `AsyncTask` pattern** — used by existing `advance_async` etc. Follow the same pattern for `AdvanceStream::next()`.
- **`fuzz-ivm.test.ts`** — fast-check pattern for the new parity fuzz test.

### Established Patterns

- **napi-rs class macro pattern** — `#[napi]` on struct + `#[napi] impl` for methods. Match the existing `RustPipelineManager` shape.
- **`Arc<Mutex<...>>` for shared receivers** — `AdvanceStream` holds `Arc<Mutex<mpsc::Receiver<StreamItem>>>` so JS can call `next()` repeatedly without consuming the stream. (Per the doc.)
- **`Disposable` pattern (TS side)** — `AdvanceStream` doesn't need it because JS GC + Drop on Rust side handle cleanup; explicit `return_()` is the user-visible escape.
- **TDD red-green commits** — phase 30 uses RED commit (failing test) → GREEN commit (fix). Apply same shape for new tests.

### Integration Points

- `RustPipelineManager` (Rust) — NEW methods added; existing methods unchanged.
- `pipeline-driver.ts` (TS) — NEW methods added alongside `advance`/`advanceAsync`; existing methods unchanged.
- `decode-advance-buf.ts` (TS) — NEW `decodeAdvanceChunkBuf` exported alongside `decodeAdvanceResultBuf`; existing decoder unchanged.
- No view-syncer changes (Phase 32 owns that).
- No cargo manifest changes expected (rayon, mpsc, napi already in scope per existing `pipeline_manager.rs`).

</code_context>

<specifics>
## Specific Ideas

- The `[u8 0]` flags byte in the chunk header is a deliberate forward-compat slot, NOT zero-padding. Decoder validates strict-zero in v1.
- Use `mpsc::sync_channel(pipeline_count)` from the start — bounded but generous enough to never block one-chunk-per-pipeline.
- Property-based fuzz uses `FUZZ_NUM_RUNS=1000` (matching `fuzz-ivm.test.ts`).
- `RustStreamError` includes a `kind` discriminator. Phase 32 view-syncer must check `instanceof` + `error.kind`, not message strings.
- `ResetPipelinesSignal` is unchanged — keep the existing class and reason strings.

</specifics>

<deferred>
## Deferred Ideas

- Per-pipeline timing telemetry (chunk format flags bit) → **Phase 33** (PERF-01 / PERF-02). Reserved byte is encoded today; bits get assigned later.
- Channel sizing tuning beyond the initial `pipeline_count` bound → **Phase 33** (PERF-01).
- Memory-footprint benchmark (peak Rust heap drops from `O(total_changes)` to `O(max_pipeline_changes)`) → **Phase 33** (PERF-02).
- Bypassing streaming for `numChanges < threshold` (small-batch optimization) → **Phase 33** (PERF-02 risk in `IVM-STREAMING-PLAN.md §9`).
- Buffer pool / batch-tiny-pipelines optimization → **Phase 33** (PERF-02 risk).
- Whether to deprecate `advanceAsync` eventually → **NOT in this milestone** per `IVM-STREAMING-PLAN.md §8` (keep for tests/benches indefinitely).
- View-syncer migration (`#advancePipelines` → `advanceStreaming`, `#processChanges` → `for await`) → **Phase 32** (MIGRATE-01 / MIGRATE-02).

### Reviewed Todos (not folded)

None — no pending todos relevant to this phase.

</deferred>

---

_Phase: 31-streaming-primitives-and-wrappers_
_Context gathered: 2026-04-29_
