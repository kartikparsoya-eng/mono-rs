# Phase 31: Rust Streaming Primitives + TS Wrappers - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-29
**Phase:** 31-streaming-primitives-and-wrappers
**Areas discussed:** Plan granularity, Forward-compat in chunk format, Test depth beyond TEST-01..05, Error surface mapping (Rust → TS)

---

## Gray Area Selection

| Option                            | Description                                                                                          | Selected |
| --------------------------------- | ---------------------------------------------------------------------------------------------------- | -------- |
| Plan granularity                  | How to slice 17 requirements into PLAN.md files: 1 large plan, 2 plans (Rust + TS), or more granular | ✓        |
| Forward-compat in chunk format    | Reserve & document the `[u8 0]` flags byte now, or treat as zero-padding                             | ✓        |
| Test depth beyond TEST-01..05     | Property-based fuzz for chunk interleaving / cancel timing, or stay deterministic only               | ✓        |
| Error surface mapping (Rust → TS) | Plain `Error` matches today, or new typed `RustStreamError` carrying kind tag                        | ✓        |

**User's choice:** All four areas, with the directive: **"take decision yourself with keep correctness and long term solution in mind"**

**Notes:** User explicitly delegated all four decisions to Claude with the constraint of correctness + long-term solution. Following this with the user's broader anti-hack rule, Claude made each decision with explicit reasoning rather than picking shortcuts. Each decision recorded inline in CONTEXT.md `<decisions>` and itemized below for audit.

---

## Plan Granularity

| Option                                                                                             | Trade-offs                                                                                                  | Selected |
| -------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- | -------- |
| 1 large plan (all 17 reqs)                                                                         | Single review unit; revert-all-or-nothing; too large to reason about                                        |          |
| 2 plans (Rust then TS)                                                                             | Matches Phase A/B from IVM-STREAMING-PLAN.md; clean compile-dependency boundary; atomic per-language revert | ✓        |
| 5+ granular plans (encoder \| AdvanceStream \| HydrateStream \| TS wrapper \| TS decoder \| tests) | Maximum parallelism; too much GSD ceremony for tightly-coupled work                                         |          |

**Decision:** 2 plans, sequential waves.

- 31-01 (Wave 1, autonomous): all Rust
- 31-02 (Wave 2, autonomous, depends_on 31-01): all TS

**Rationale:** TS plan literally cannot compile until the napi class exists; sequential wave is forced by the language barrier. Two plans keep each commit atomically revertable. Finer granularity would increase GSD ceremony without improving rollback safety.

---

## Forward-compat in Chunk Format

| Option                                              | Trade-offs                                                                                                                         | Selected |
| --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- | -------- |
| Reserve & document `[u8 0]` byte as flags field now | Future bench-mode bits land additively; old decoders fail loudly on non-zero; matches existing `encode_advance_result_buf` pattern | ✓        |
| Treat as zero-padding to be repurposed in Phase 33  | Simpler now; risk of silent decoder corruption when Phase 33 adds telemetry bits                                                   |          |

**Decision:** Reserve & document. Decoder validates `flags === 0` and throws clear error if not.

**Rationale:** Wire format already carries the byte; documenting it as reserved makes Phase 33's per-pipeline timing telemetry strictly additive without a format-version bump. Long-term cost of NOT doing this: silent decoder corruption when Phase 33 adds bits — exactly the "stale binary" failure class Phase 30-05 just patched against. Cheap to document now, expensive to debug later.

---

## Test Depth Beyond TEST-01..05

| Option                                                                 | Trade-offs                                                                                                                            | Selected |
| ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| Stay deterministic only                                                | CI-stable; easy to reason about; misses concurrency/interleave bugs                                                                   |          |
| Add full fuzz coverage (cancellation timing, interleave, panic timing) | Strongest catch rate; flaky timing tests; high CI cost                                                                                |          |
| Add ONE parity fuzz test (streaming vs buffered RowChange multisets)   | Catches multi-pipeline ordering bugs deterministically; aligns with existing `fuzz-ivm.test.ts` 1k-iteration pattern; minimal CI cost | ✓        |

**Decision:** Add one new fuzz test `streaming-vs-buffered-parity.fuzz.test.ts` with `FUZZ_NUM_RUNS=1000`. TEST-01/02/03/05 stay deterministic with explicit barriers.

**Rationale:** Streaming has a meaningful concurrency risk (rayon scheduling variance, multi-pipeline interleave) that determinism can't catch. A single parity fuzz pins TEST-04's parity claim with stochastic depth without introducing flaky timing tests. Aligns with the project's existing fuzz suite pattern (`fuzz-ivm.test.ts`).

---

## Error Surface Mapping (Rust → TS)

| Option                                                                            | Trade-offs                                                                                                          | Selected |
| --------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | -------- |
| Plain `Error` (matches today's buffered errors)                                   | Simplest; consumers must string-match `.message` to differentiate panic/cancel/reset; brittle                       |          |
| Typed `RustStreamError` with `kind: 'panic' \| 'rayon_error' \| 'channel_closed'` | `error.kind === 'panic'` instead of fragile string match; small new export; Phase 32 view-syncer can branch cleanly | ✓        |
| Per-kind error classes (`StreamPanicError`, `StreamCancelError`, etc.)            | Most type-safe; over-engineered for 3-4 kinds; harder to extend                                                     |          |

**Decision:** Single typed `RustStreamError` class with `kind` discriminator and `source = 'rust-stream'` brand. `ResetPipelinesSignal` shape unchanged per WRAP-04.

**Rationale:** Streaming introduces failure modes the buffered path didn't (per-pipeline panic, cancel race, channel close). A bare `Error.message` collapses these into one surface. Phase 32's view-syncer needs to differentiate panic (log+crash) vs cancellation (swallow) vs reset (recover) — a kind tag enables `error.kind === 'panic'` instead of fragile string-matching on `.message` text. Long-term cost of NOT doing this: every consumer string-matches on message text, breaking when wording changes.

---

## Claude's Discretion

Areas where Claude has flexibility (recorded in CONTEXT.md `<decisions>` ### Claude's Discretion):

- Specific rayon scope vs spawn pattern (helper thread owning a `rayon::scope`, vs `tokio::task::spawn_blocking` wrapping a `rayon::scope`, vs `napi::tokio_runtime::spawn`)
- Buffer ownership at the napi boundary (`Vec<u8>` vs `napi::bindgen_prelude::Buffer`)
- Exact `'yield'` token cadence in the TS streaming wrapper (match existing `#wrapWithTimeout` shape)
- Internal error type for the Rust `panic::catch_unwind` shim that produces `StreamItem::Error('panic', ...)`

## Deferred Ideas

(Recorded in CONTEXT.md `<deferred>` section)

- Per-pipeline timing telemetry → Phase 33 (PERF-01/02)
- Channel sizing tuning beyond `pipeline_count` → Phase 33 (PERF-01)
- Memory-footprint benchmark → Phase 33 (PERF-02)
- Small-batch streaming bypass → Phase 33 (PERF-02 risk)
- Buffer pool / batch-tiny-pipelines → Phase 33 (PERF-02 risk)
- Deprecating `advanceAsync` → NOT in this milestone
- View-syncer migration → Phase 32 (MIGRATE-01/02)
