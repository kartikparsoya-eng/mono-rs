---
phase: 31
slug: streaming-primitives-and-wrappers
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-29
---

# Phase 31 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution. Sourced from `31-RESEARCH.md` § Validation Architecture and CONTEXT.md § Test Depth (D-06..D-09).

---

## Test Infrastructure

| Property                      | Value                                                                                                                                                                                                                                                                  |
| ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Frameworks**                | `cargo test` (Rust unit, both crates) + `vitest` 4.1.3 (TS unit / fuzz)                                                                                                                                                                                                |
| **Config files**              | `packages/zqlite-rs/Cargo.toml`, `packages/zero-cache/vitest.config.ts`                                                                                                                                                                                                |
| **Quick run command (Rust)**  | `cargo test --release -p zqlite-rs --lib stream_`                                                                                                                                                                                                                      |
| **Quick run command (TS)**    | `cd packages/zero-cache && npx vitest run src/services/view-syncer/pipeline-driver.streaming`                                                                                                                                                                          |
| **Full suite command (Rust)** | `cargo test --release -p zero-ivm-rs && cargo test --release -p zqlite-rs`                                                                                                                                                                                             |
| **Full suite command (TS)**   | `cd packages/zero-cache && npx vitest run "src/services/view-syncer/pipeline-driver.*.test.ts" "src/services/view-syncer/decode-advance-buf.test.ts" "src/services/view-syncer/fuzz-ivm.test.ts" "src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts"` |
| **Estimated runtime**         | Rust quick: ~5s · TS quick: ~10s · Full suite: ~90s (with 1k fuzz iterations)                                                                                                                                                                                          |

---

## Sampling Rate

- **After every task commit:** Run quick command for the language touched (Rust quick after Rust commits; TS quick after TS commits).
- **After every plan wave:** Run the full suite for the language(s) touched.
- **Before `/gsd-verify-work`:** Full Rust + Full TS suites must be green; broader `pipeline-driver.*.test.ts` glob must show zero failed test files (per Phase 30-05 blocking gate).
- **Max feedback latency:** 10s for unit tests; 90s for fuzz-inclusive full suite.

---

## Per-Task Verification Map

(To be filled by planner — placeholders here for the expected shape.)

| Task ID  | Plan | Wave | Requirement         | Test Type   | Automated Command                                                                                                          | Status     |
| -------- | ---- | ---- | ------------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------- | ---------- |
| 31-01-01 | 01   | 1    | STREAM-01..03       | unit        | `cargo test --release -p zqlite-rs --lib stream_advance`                                                                   | ⬜ pending |
| 31-01-02 | 01   | 1    | STREAM-04 (TEST-01) | unit        | `cargo test --release -p zqlite-rs --lib stream_cancel`                                                                    | ⬜ pending |
| 31-01-03 | 01   | 1    | STREAM-05 (TEST-02) | unit        | `cargo test --release -p zqlite-rs --lib stream_reset`                                                                     | ⬜ pending |
| 31-01-04 | 01   | 1    | TEST-03             | unit        | `cargo test --release -p zqlite-rs --lib stream_panic`                                                                     | ⬜ pending |
| 31-02-01 | 02   | 2    | WRAP-01             | unit        | `cd packages/zero-cache && npx vitest run src/services/view-syncer/decode-advance-buf.test.ts`                             | ⬜ pending |
| 31-02-02 | 02   | 2    | WRAP-02..04         | integration | `cd packages/zero-cache && npx vitest run src/services/view-syncer/pipeline-driver.streaming.test.ts`                      | ⬜ pending |
| 31-02-03 | 02   | 2    | TEST-04             | fuzz        | `FUZZ_NUM_RUNS=1000 npx vitest run src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts`                     | ⬜ pending |
| 31-02-04 | 02   | 2    | TEST-05             | unit        | `cd packages/zero-cache && npx vitest run src/services/view-syncer/pipeline-driver.streaming.test.ts -t "iterator.return"` | ⬜ pending |

_Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky_

---

## Wave 0 Requirements

- [ ] `packages/zqlite-rs/src/chunk_encoder.rs` — new module, requires test stubs for chunk format header (`[u32 count][u8 0]`) and per-row encoding parity with `encode_advance_result_buf`.
- [ ] `packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts` — new file, requires `fast-check` arbitrary for `RowChange` inputs (mirror `fuzz-ivm.test.ts` shape).
- [ ] No new framework install — `cargo test`, `vitest`, `fast-check` already in deps.
- [ ] No new env vars beyond `FUZZ_NUM_RUNS` (already used by `fuzz-ivm.test.ts`).

_Existing infrastructure covers all phase requirements except the new fuzz file noted above._

---

## Manual-Only Verifications

| Behavior                                                                    | Requirement                      | Why Manual                                      | Test Instructions                                                  |
| --------------------------------------------------------------------------- | -------------------------------- | ----------------------------------------------- | ------------------------------------------------------------------ |
| Memory footprint reduction (`O(total_changes)` → `O(max_pipeline_changes)`) | None directly (Phase 33 PERF-02) | Requires production-scale benchmark             | Skip in Phase 31; Phase 33 PERF-02 covers via `rust-ivm-bench.ts`. |
| Time-to-first-byte improvement under multi-pipeline workload                | None directly (Phase 33 PERF-01) | Requires microbenchmark with slow tail pipeline | Skip in Phase 31; Phase 33 PERF-01.                                |

_Phase 31 has no production consumers, so no live system manual verification is required. Phase 32's view-syncer migration UAT will validate end-to-end behavior._

---

## Signal-Rate Analysis (Nyquist Dimension 8)

Per `31-RESEARCH.md` § Validation Architecture, the streaming surface introduces 5 new signal classes that must be sampled at appropriate rates:

| Signal Class                         | Production Behavior                                                                                                      | Test Sampling Rate                                                                                                                                     | Detection Cost                                                        |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------- |
| **Chunk arrival ordering**           | Per-pipeline rayon completion order (non-deterministic across pipelines, deterministic within)                           | Fuzz (1k iter) parity vs buffered Vec                                                                                                                  | Low: multiset compare via `compareChanges` from `dual-executor.ts`    |
| **Cancellation observable boundary** | `Arc<AtomicBool>` checked between operator pushes inside `advance_persistent_pipeline`                                   | Deterministic TEST-01: queue 50 pipelines with explicit `Mutex<Barrier>` for ordering, cancel after 5, assert ≤ 6 chunks delivered                     | Low: counted via channel receiver                                     |
| **Reset signal mid-stream**          | Companion scalar check after pipeline join → `StreamItem::ResetSignal` + channel close                                   | Deterministic TEST-02: companion table seeded with values that cascade to scalar change after push                                                     | Low: enum match on next StreamItem                                    |
| **Panic isolation**                  | `panic::catch_unwind` per `scope.spawn`; sibling pipelines complete; panic surfaces as `StreamItem::Error('panic', ...)` | Deterministic TEST-03: pipeline that panics on first push (use `assert!(false)` in test-only operator)                                                 | Low: enum match + sibling chunk count                                 |
| **`return_()` cancel propagation**   | TS `for await ... break` calls `stream.return_()` → Rust `cancel.store(true)` → tasks exit at next push boundary         | Deterministic TEST-05: `for await of changes { if (n++ === 3) break; }` then poll a `static AtomicUsize` counter the test operator increments per push | Low: assert counter does not exceed `expected_pushes_after_break + 1` |

**Sampling rate adequacy:** All 5 signals have deterministic sampling at 100% (every test run exercises the boundary) plus the parity fuzz at 1000 iterations covers the cross-pipeline ordering sub-space. No signal goes uncovered.

---

## Validation Gaps (resolved during Wave 0)

None expected — all 17 requirements (STREAM-01..06, WRAP-01..04, COMPAT-01..03, TEST-01..05) map cleanly to one of: deterministic unit test, integration test, or the new parity fuzz. Planner should fill the per-task verification map above with exact test names once tasks are decomposed.

---

_Phase 31 — Validation Strategy_
_Drafted: 2026-04-29_
