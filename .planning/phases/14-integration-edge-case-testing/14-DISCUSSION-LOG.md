# Phase 14: Integration + Edge Case Testing - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 14-integration-edge-case-testing
**Areas discussed:** Benchmark suite, Edge case coverage, Documentation & tagging

---

## Benchmark Suite

| Option | Description | Selected |
|--------|-------------|----------|
| Throughput microbench | Synthetic: fixed N pipelines, fixed diff size, measure throughput | |
| Realistic workload replay | Multi-table, mixed query types, wall-clock per advance cycle | |
| Both | Microbench for CI regression + realistic for headline numbers | ✓ |

**User's choice:** Both

| Option | Description | Selected |
|--------|-------------|----------|
| Pipeline count sweep | 1, 2, 4, 8, 16, 32, 64 pipelines | ✓ |
| Two fixed points | 10 and 100 pipelines | |
| Full matrix | Pipelines x diff size | |

**User's choice:** Pipeline count sweep

| Option | Description | Selected |
|--------|-------------|----------|
| Any improvement | Any measurable improvement | |
| 2x minimum | Minimum 2x throughput | |
| 5x target | Target 5x+ | |
| Custom | User-defined two-number bar | ✓ |

**User's choice:** Two-number bar — 4x hard minimum (below = broken parallelism, no ship), 6x success target (matches Amdahl prediction). Non-negotiable correctness gate: byte-identical output to TS path.

**Notes:** User rejected all presets with detailed analysis. 4x derived from sequential floor ~4ms + parallel portion ~500ms -> ~80ms on 8 cores. Below 4x means WAL/serialization issues. Correctness is non-negotiable regardless of speed.

---

## Edge Case Coverage

| Option | Description | Selected |
|--------|-------------|----------|
| Diff size extremes | 1000+ changes, empty, single-row, all-no-op | ✓ |
| Pipeline topology changes | Mixed eligible/ineligible, hot-swap, add/remove during advance | ✓ |
| Error & fallback paths | Panic recovery, SQLite busy, version mismatch, TS fallback | ✓ |
| Data type edge cases | NULL, empty string, unicode, long values, all SQLite types | ✓ |

**User's choice:** All four categories

| Option | Description | Selected |
|--------|-------------|----------|
| Rust unit tests only | cargo test in zqlite-rs | |
| TS integration harness | Compare Rust vs TS output end-to-end | |
| Both | Rust unit tests + TS integration harness | ✓ |

**User's choice:** Both

---

## Documentation & Tagging

| Option | Description | Selected |
|--------|-------------|----------|
| Architecture overview | Rust/TS integration, data flow, operator boundary, fallback | ✓ |
| Performance results | Benchmark results table, scaling curves | |
| Ops/migration guide | Enable/disable, env vars, build prereqs, troubleshooting | ✓ |

**User's choice:** Architecture overview + Ops/migration guide (no separate perf doc)

| Option | Description | Selected |
|--------|-------------|----------|
| Tag after bench + tests | Tag when benchmarks pass 4x + edge tests green | ✓ |
| Tag after docs complete | Tag after all docs written | |

**User's choice:** Tag after bench + tests

---

## Claude's Discretion

- Benchmark framework choice
- Specific diff sizes in sweep
- Documentation format/location

## Deferred Ideas

None
