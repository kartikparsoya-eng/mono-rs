# Phase 18: Concurrency Tests - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 18-concurrency-tests
**Areas discussed:** Concurrency Model, Determinism Assertion, Pipeline Mutation Timing, Test Scope Boundaries

---

## Concurrency Model

| Option                        | Description                                                                                   | Selected |
| ----------------------------- | --------------------------------------------------------------------------------------------- | -------- |
| Rayon-internal (realistic)    | Test Rayon internal parallelism — multiple pipelines reading shared Arc<changes> concurrently | ✓        |
| Multi-thread (worker_threads) | Use worker_threads from TS to call rust_advance() from multiple threads simultaneously        |          |
| Both layers                   | Test both Rayon fan-out + TS worker_threads for SQLite contention                             |          |

**User's choice:** Rayon-internal (realistic)
**Notes:** rust_advance is synchronous — no true call-level overlap from Node. Real concurrency is Rayon par_iter over pipelines.

---

## Determinism Assertion

| Option                           | Description                                                         | Selected |
| -------------------------------- | ------------------------------------------------------------------- | -------- |
| Set equality (order-independent) | Sort output by (query_id, pk), compare as sets                      |          |
| Repeated runs (stability)        | Run N times, assert all identical                                   |          |
| Both (set + stability)           | Set equality for correctness + 50x repeated runs for race detection | ✓        |

**User's choice:** Both (set + stability)
**Notes:** Two-pronged approach catches both wrong results and flaky ordering from thread scheduling.

---

## Pipeline Mutation Timing

| Option                 | Description                                                   | Selected |
| ---------------------- | ------------------------------------------------------------- | -------- |
| TS-level between calls | Add/remove pipelines between consecutive rust_advance() calls | ✓        |
| Both TS + Rust unit    | Also add Rust tests proving statelessness                     |          |
| Skip (stateless)       | Function is stateless, nothing to corrupt                     |          |

**User's choice:** TS-level between calls
**Notes:** Since rust_advance is purely functional (no persistent state), mid-advance mutation is impossible. Testing between-calls scenario at TS level.

---

## Test Scope Boundaries

| Option                         | Description                                                        | Selected |
| ------------------------------ | ------------------------------------------------------------------ | -------- |
| Rust unit + vitest integration | Rust tests for Rayon determinism + vitest for TS pipeline mutation | ✓        |
| Vitest only                    | All tests in vitest, simpler setup                                 |          |
| Rust only                      | Test where concurrency lives (Rayon)                               |          |

**User's choice:** Rust unit + vitest integration
**Notes:** Keep iteration count moderate (50x) so tests stay under 5s.

---

## Claude's Discretion

- Number of pipelines per test
- Specific filter/operator configurations
- Timeout assertion for deadlock detection

## Deferred Ideas

None
