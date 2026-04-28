# Phase 7: Benchmarks & Validation - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 07-benchmarks
**Areas discussed:** Workload profiles, Regression testing, GC pause measurement, Results documentation

---

## Workload Profiles

| Option                      | Description                                                 | Selected |
| --------------------------- | ----------------------------------------------------------- | -------- |
| View-syncer diff simulation | Simulate changesSinceBuf -> getRowsMultiBuf -> decode loop  | ✓        |
| Concurrent read pressure    | Multiple queryAll + getRowsBuf interleaved, WAL concurrency |          |
| Mixed read/write            | INSERTs interleaved with allBuf reads                       |          |
| All of the above            | Comprehensive coverage                                      |          |

**User's choice:** View-syncer diff simulation
**Notes:** Focus on the actual hot path, not synthetic concurrent scenarios.

---

## Regression Testing

| Option                          | Description                                           | Selected |
| ------------------------------- | ----------------------------------------------------- | -------- |
| Assert gates (existing pattern) | Extend --assert mode with gates on all key operations |          |
| Timed vitest comparison         | Run vitest suites with timing wrappers                |          |
| Both approaches                 | Assert gates + timed snapshotter.test.ts comparison   | ✓        |

**User's choice:** Both approaches
**Notes:** Two-pronged: micro-op assert gates + end-to-end timed integration test.

---

## GC Pause Measurement

| Option                      | Description                                                    | Selected |
| --------------------------- | -------------------------------------------------------------- | -------- |
| Forced GC + p99 latency     | --expose-gc, force GC every N iterations, measure spikes       |          |
| Trace-gc analysis           | V8 --trace-gc flag, parse pause durations                      |          |
| Skip (document theory only) | Buffer protocol reduces allocation; document without measuring | ✓        |

**User's choice:** Skip (document theory only)
**Notes:** The buffer protocol's reduced object allocation is self-evident from the 1.7-2x speedup.

---

## Results Documentation

| Option                          | Description                                        | Selected |
| ------------------------------- | -------------------------------------------------- | -------- |
| Markdown report in planning dir | Table + summary in .planning/phases/07-benchmarks/ | ✓        |
| Planning dir + package README   | Same + README section in packages/zqlite-rs/       |          |
| JSON + generated markdown       | Machine-readable output + generated summary        |          |

**User's choice:** Markdown report in planning dir
**Notes:** Formal documentation of benchmark results alongside planning artifacts.

---

## Claude's Discretion

- Exact benchmark parameters (row counts, iteration counts, warmup)
- Timed snapshotter comparison structure
- Additional assert gates beyond existing thresholds

## Deferred Ideas

None
