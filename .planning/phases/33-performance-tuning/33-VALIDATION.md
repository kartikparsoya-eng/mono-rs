---
phase: 33
slug: performance-tuning
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-29
---

# Phase 33 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution. Sourced from `33-RESEARCH.md` §6 Validation Architecture and `33-CONTEXT.md` decisions D-25..D-30.

---

## Test Infrastructure

| Property                            | Value                                                                                                                                                                                         |
| ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Frameworks**                      | `cargo test` (Rust unit) + `vitest` 4.1.3 (TS unit/integration/bench)                                                                                                                         |
| **Quick run (33-01 HARDEN-01)**     | `cd packages/zero-cache && npx vitest run src/services/view-syncer/parity-check.test.ts` (new file)                                                                                           |
| **Quick run (33-02 HARDEN-02)**     | `cd packages/zero-ivm-rs && cargo test --release --lib or_exists_op::tests`                                                                                                                   |
| **Quick run (33-03 PERF-01/02/03)** | `cd packages/zqlite-rs && cargo test --release --lib streaming_channel_bounded_blocks` + `cd packages/zero-cache && npx vitest run src/services/view-syncer/rust-ivm-streaming-bench.test.ts` |
| **Full Rust suite**                 | `cargo test --release -p zero-ivm-rs && cargo test --release -p zqlite-rs --lib -- --test-threads=1`                                                                                          |
| **Full TS dual-mode**               | TS suite under `ZQLITE_RS_PARITY_CHECK=off`, `=sample`, `=strict` (3 invocations)                                                                                                             |
| **Estimated runtime**               | 33-01: ~30s · 33-02: ~10s · 33-03: ~60s · Full Rust: ~30s · Full TS dual-mode: ~3 min                                                                                                         |

---

## Sampling Rate

- **After every task commit:** Run quick command for the touched plan.
- **After every plan wave:** Run full suite for the touched language(s).
- **Before `/gsd-verify-work`:** Full Rust + Full TS dual-mode + benchmarks must be green; broader `pipeline-driver.*.test.ts` glob must show zero failed test files (Phase 30-05 blocking gate); zero parity divergences in sample mode; bench results recorded to `.planning/milestones/v5.0-bench-results.md`.
- **Max feedback latency:** 30s for unit tests; ~3 min for full TS dual-mode.

---

## Per-Task Verification Map

(Planner refines.)

| Task ID  | Plan | Wave | Requirement                                  | Test Type   | Automated Command                                                                                         | Status     |
| -------- | ---- | ---- | -------------------------------------------- | ----------- | --------------------------------------------------------------------------------------------------------- | ---------- |
| 33-01-01 | 01   | 1    | HARDEN-01 — TS oracle                        | unit        | `npx tsc --noEmit packages/zero-cache/src/services/view-syncer/pipeline-driver-ts-oracle.ts`              | ⬜ pending |
| 33-01-02 | 01   | 1    | HARDEN-01 — sampling shim                    | unit        | `npx vitest run src/services/view-syncer/parity-check.test.ts -t "sample mode"`                           | ⬜ pending |
| 33-01-03 | 01   | 1    | HARDEN-01 — strict mode                      | unit        | `ZQLITE_RS_PARITY_CHECK=strict npx vitest run src/services/view-syncer/parity-check.test.ts -t "strict"`  | ⬜ pending |
| 33-01-04 | 01   | 1    | HARDEN-01 — divergence counter               | unit        | `getParityDivergenceCount()` exported and callable from tests                                             | ⬜ pending |
| 33-01-05 | 01   | 1    | HARDEN-01 — broader sample-mode parity check | integration | `ZQLITE_RS_PARITY_CHECK=sample npx vitest run "src/services/view-syncer/*.test.ts"` returns 0 divergences | ⬜ pending |
| 33-02-01 | 02   | 1    | HARDEN-02 — fetch tests                      | unit        | `cargo test --release -p zero-ivm-rs --lib or_exists_op::tests::test_or_exists_fetch`                     | ⬜ pending |
| 33-02-02 | 02   | 1    | HARDEN-02 — push tests                       | unit        | `cargo test --release -p zero-ivm-rs --lib or_exists_op::tests::test_or_exists_push`                      | ⬜ pending |
| 33-02-03 | 02   | 1    | HARDEN-02 — child push + transitions         | unit        | `cargo test --release -p zero-ivm-rs --lib or_exists_op::tests::test_or_exists_child`                     | ⬜ pending |
| 33-02-04 | 02   | 1    | HARDEN-02 — final count check                | meta        | `cargo test --release -p zero-ivm-rs --lib -- --list or_exists_op`                                        | ⬜ pending |
| 33-03-01 | 03   | 1    | PERF-01 channel block                        | unit (Rust) | `cargo test --release -p zqlite-rs --lib streaming_channel_bounded_blocks_when_full`                      | ⬜ pending |
| 33-03-02 | 03   | 1    | PERF-02 TTFB bench                           | bench (TS)  | `npx vitest run src/services/view-syncer/rust-ivm-streaming-bench.test.ts -t "TTFB"`                      | ⬜ pending |
| 33-03-03 | 03   | 1    | PERF-03 memory bench                         | bench (TS)  | `npx vitest run src/services/view-syncer/rust-ivm-streaming-bench.test.ts -t "memory"`                    | ⬜ pending |
| 33-03-04 | 03   | 1    | bench results recorded                       | meta        | `wc -l .planning/milestones/v5.0-bench-results.md > 5`                                                    | ⬜ pending |

_Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky_

---

## Wave 0 Requirements

- [ ] `packages/zero-cache/src/services/view-syncer/pipeline-driver-ts-oracle.ts` — new file (lifted/adapted from `~/Documents/xy-repo/mono/packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` lines 450 + 701-810)
- [ ] `packages/zero-cache/src/services/view-syncer/parity-check.test.ts` — new file
- [ ] `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts` — new file
- [ ] `.planning/milestones/v5.0-bench-results.md` — new file (created by 33-03 with append-only header)
- [ ] No new framework install
- [ ] No new env vars beyond `ZQLITE_RS_PARITY_CHECK` and `ZQLITE_RS_PARITY_CHECK_RATE`

---

## Manual-Only Verifications

| Behavior                                                            | Requirement                     | Why Manual                              | Test Instructions                                                                                |
| ------------------------------------------------------------------- | ------------------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Production observability dashboards (`zero_sync_*` divergence rate) | None directly (deferred to ops) | Requires deployment infra; out of scope | Skip; ops decision post-milestone.                                                               |
| Long-running parity-check soak test in prod                         | None directly (deferred to ops) | Requires production traffic             | Operators enable `ZQLITE_RS_PARITY_CHECK=sample` post-deploy and watch divergence-count metrics. |

---

## Signal-Rate Analysis (Nyquist Dimension 8)

| Signal Class                         | Production Behavior                                 | Test Sampling Rate                                                                                        | Detection Cost                                           |
| ------------------------------------ | --------------------------------------------------- | --------------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| **Parity-check divergence**          | Logged in CI sample mode; thrown in strict mode     | Every existing TS test under sample mode (1-in-10 invocations) + dedicated strict run on a curated subset | Low: counter assertion                                   |
| **OrExists test coverage**           | Compile-time + cargo test count                     | One-shot meta check via `cargo test -- --list` returns ≥22                                                | Low: list count                                          |
| **Channel-block timing**             | Producer blocks when channel full                   | Deterministic Rust unit test with explicit Mutex/Barrier                                                  | Low: thread join                                         |
| **TTFB regression detection**        | Microbenchmark records time-to-first-chunk          | Vitest `it` block with mocked staggered pipelines; assert ratio ≤ 1.5×                                    | Medium: timing-sensitive, threshold loose to avoid flake |
| **Memory-peak regression detection** | Microbenchmark records peak `process.memoryUsage()` | Vitest `it` block with hydrate workload; assert streaming×N ≤ buffered×1.2                                | Medium: memory measurement noisy, threshold loose        |

**Sampling adequacy:** All 5 signals have deterministic boundaries that matter for correctness/perf. Production-scale exposure is operator's responsibility (deferred).

---

## Validation Gaps (resolved during Wave 0)

None expected. Planner refines per-task verification map above with exact test names once tasks are decomposed.

---

_Phase 33 — Validation Strategy_
_Drafted: 2026-04-29_
