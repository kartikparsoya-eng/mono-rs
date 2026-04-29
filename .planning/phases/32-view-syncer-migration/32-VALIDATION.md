---
phase: 32
slug: view-syncer-migration
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-29
---

# Phase 32 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution. Sourced from `32-RESEARCH.md` §6 Validation Architecture and `32-CONTEXT.md` decisions D-14 through D-19.

---

## Test Infrastructure

| Property                      | Value                                                                                                                                                                    |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Frameworks**                | `vitest` 4.1.3 (TS unit + integration) + Postgres testcontainers (`vitest.config.pg-16.ts` for view-syncer.pg.test.ts)                                                   |
| **Config files**              | `packages/zero-cache/vitest.config.no-pg.ts`, `packages/zero-cache/vitest.config.pg-16.ts`                                                                               |
| **Quick run command (32-01)** | `cd packages/zero-cache && npx vitest run src/services/view-syncer/decode-advance-buf.test.ts`                                                                           |
| **Quick run command (32-02)** | `cd packages/zero-cache && npx vitest run src/services/view-syncer/view-syncer-streaming-mid-batch.test.ts src/services/view-syncer/view-syncer-streaming-reset.test.ts` |
| **Full TS suite**             | `cd packages/zero-cache && npx vitest run "src/services/view-syncer/*.test.ts"` (under both flag values)                                                                 |
| **Full PG suite**             | `cd packages/zero-cache && npx vitest run --config vitest.config.pg-16.ts "src/services/view-syncer/*.pg.test.ts"`                                                       |
| **Full Rust suite**           | `cargo test --release -p zero-ivm-rs && cargo test --release -p zqlite-rs` (regression check — should remain unchanged)                                                  |
| **Estimated runtime**         | 32-01 quick: ~5s · 32-02 quick: ~15s · Full TS dual-mode: ~2-3 min · Full PG: ~3-5 min                                                                                   |

---

## Sampling Rate

- **After every task commit:** Run quick command for the touched plan (32-01 → decoder tests; 32-02 → streaming integration tests).
- **After every plan wave:** Run the full TS suite for the touched plan + Rust regression suite (unchanged baseline).
- **Before `/gsd-verify-work`:** Full TS dual-mode + Full PG suite + Rust regression must all be green; broader `pipeline-driver.*.test.ts` glob must show zero failed test files (Phase 30-05 blocking gate).
- **Max feedback latency:** 5s for unit tests; 3 min for full dual-mode TS suite.

---

## Per-Task Verification Map

(Planner refines specifics; placeholder shape here.)

| Task ID  | Plan | Wave | Requirement                | Test Type         | Automated Command                                                                                                                | Status     |
| -------- | ---- | ---- | -------------------------- | ----------------- | -------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| 32-01-01 | 01   | 1    | (CR-01 RED)                | unit              | `cd packages/zero-cache && npx vitest run src/services/view-syncer/decode-advance-buf.test.ts -t i64.boundary` exits 1           | ⬜ pending |
| 32-01-02 | 01   | 1    | (CR-01 GREEN)              | unit              | `cd packages/zero-cache && npx vitest run src/services/view-syncer/decode-advance-buf.test.ts` exits 0                           | ⬜ pending |
| 32-02-01 | 02   | 2    | MIGRATE-01                 | integration       | view-syncer.test.ts dual-mode tests pass under both flag values                                                                  | ⬜ pending |
| 32-02-02 | 02   | 2    | MIGRATE-02                 | integration       | `addQueriesStreaming` callsite test passes; existing `#hydrateUnchangedQueries` tests pass                                       | ⬜ pending |
| 32-02-03 | 02   | 2    | MIGRATE-03                 | integration (NEW) | `view-syncer-streaming-mid-batch.test.ts` asserts first pokePart timestamp < slowest pipeline completion                         | ⬜ pending |
| 32-02-04 | 02   | 2    | MIGRATE-04                 | integration (NEW) | `view-syncer-streaming-reset.test.ts` asserts pokers.cancel() called once + no CVR commit when companion-scalar change mid-batch | ⬜ pending |
| 32-02-05 | 02   | 2    | (RustStreamError handling) | integration       | `view-syncer.ts` catches `instanceof RustStreamError`, logs with `kind` field, throws                                            | ⬜ pending |
| 32-02-06 | 02   | 2    | (Feature flag)             | integration       | `ZQLITE_RS_USE_STREAMING_CONSUMER=false` round-trips identical to today's buffered behavior; `=true` uses streaming              | ⬜ pending |

_Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky_

---

## Wave 0 Requirements

- [ ] `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-mid-batch.test.ts` — new file, requires mock-pipeline timer helper (research recommends Proxy-on-prototype pattern from `pipeline-driver.streaming.test.ts:265-299`)
- [ ] `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-reset.test.ts` — new file, integration with companion-scalar change injection (use existing test mechanism from view-syncer.test.ts reset coverage)
- [ ] Optional: extend `connectWithQueueAndSource` in `view-syncer-test-util.ts` with `onMessage` callback for timestamp-recording (per RESEARCH.md §5)
- [ ] No new framework install — vitest, fast-check, postgres testcontainers all in deps
- [ ] No new env vars required for tests (the `ZQLITE_RS_USE_STREAMING_CONSUMER` flag is set per-`describe.each` block, not via env file)

_Existing infrastructure covers all phase requirements except the 2 new test files noted above._

---

## Manual-Only Verifications

| Behavior                                                     | Requirement                         | Why Manual                                                                         | Test Instructions                                                                                                                                                     |
| ------------------------------------------------------------ | ----------------------------------- | ---------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Time-to-first-byte improvement under multi-pipeline workload | None directly (Phase 33 PERF-01)    | Requires production-scale benchmark; Phase 32 ships behind feature flag for safety | Skip in Phase 32; Phase 33 PERF-01 benchmarks.                                                                                                                        |
| End-to-end production validation under streaming             | (Implicit via feature flag rollout) | Requires production deploy with flag toggled on                                    | Deploy with `ZQLITE_RS_USE_STREAMING_CONSUMER=true`, observe pokes mid-batch, monitor error logs for `kind` field. Operators have rollback via flag toggle + restart. |
| Memory footprint reduction                                   | None directly (Phase 33 PERF-02)    | Requires production-scale benchmark                                                | Skip in Phase 32; Phase 33 PERF-02.                                                                                                                                   |

_Phase 32 ships with a feature flag default-on after deploy validation. The "manual production validation" item is the operator's deploy-and-observe step, NOT Claude's direct UAT._

---

## Signal-Rate Analysis (Nyquist Dimension 8)

Per `32-RESEARCH.md` §6 Validation Architecture, this migration introduces 5 production signal classes:

| Signal Class                          | Production Behavior                                                                 | Test Sampling Rate                                                                                                                                                                               | Detection Cost                     |
| ------------------------------------- | ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------- |
| **`pokePart` cadence**                | Mid-batch when streaming flag is on; end-of-batch when off                          | Deterministic mid-batch test (`view-syncer-streaming-mid-batch.test.ts`) — mock pipelines with staggered timers (10/50/200ms); assert first pokePart fires < 200ms (slowest pipeline completion) | Low: timestamp array on poker mock |
| **`RustStreamError` kind frequency**  | Logged with structured `kind` field; bubbled to crash view-syncer                   | Deterministic unit test injects each kind via mocked `advanceStreaming`; asserts log payload includes `kind` and error rethrows                                                                  | Low: log capture + assertion       |
| **`ResetPipelinesSignal` mid-stream** | Existing reset-and-retry path; adds `try/finally { stream.return_() }` cleanup      | Deterministic integration test (`view-syncer-streaming-reset.test.ts`) injects companion-scalar change mid-batch; asserts cancel() once + no CVR commit                                          | Low: pokers spy + CVR assertion    |
| **CVR commit atomicity**              | Exactly once at end-of-stream after `pokers.end(finalVersion)`                      | Existing view-syncer.test.ts CVR coverage runs under both flag values via `describe.each`                                                                                                        | Low: unchanged from today          |
| **CR-01 i64 boundary cases**          | Decoder throws on values exceeding `MAX_SAFE_INTEGER`; never silently lossy-decodes | 6 deterministic boundary tests in 32-01 (per D-14)                                                                                                                                               | Low: parametrized test cases       |

**Sampling rate adequacy:** All 5 signals have deterministic 100% sampling at boundaries that matter for correctness. Production-scale sampling (TTFB distribution, panic frequency under real load) is Phase 33's domain and is intentionally deferred — Phase 32's contract is "the feature flag-gated migration is correct and the new tests pin the load-bearing properties."

---

## Validation Gaps (resolved during Wave 0)

None expected — all 4 requirements (MIGRATE-01..04) plus CR-01 map cleanly to one of: deterministic unit test, deterministic integration test, or existing test wrapped under `describe.each([true, false])`. Planner should refine the per-task verification map above with exact test names once tasks are decomposed.

---

_Phase 32 — Validation Strategy_
_Drafted: 2026-04-29_
