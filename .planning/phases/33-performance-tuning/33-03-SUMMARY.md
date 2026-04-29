---
phase: 33-performance-tuning
plan: 03
subsystem: streaming-ivm-bench
tags: [perf, benchmarks, channel-bound, ttfb, memory-peak, streaming]
requires:
  - packages/zqlite-rs/src/pipeline_manager.rs (existing streaming infrastructure from Phase 31)
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.ts (advanceAsync + advanceStreaming methods from Phase 31/32)
provides:
  - PERF-01 channel-block test confirming bounded mpsc::sync_channel rejects sends when full
  - PERF-02 TTFB bench proving streaming first-chunk ≈ min(pipeline_time)
  - PERF-03 memory-peak bench proving streaming peak ≈ O(max_pipeline_changes)
  - .planning/milestones/v5.0-bench-results.md append-only regression log
affects:
  - packages/zqlite-rs/src/pipeline_manager.rs (test addition only — production code unchanged)
  - packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts (new file)
  - .planning/milestones/v5.0-bench-results.md (new file)
tech-stack:
  added: []
  patterns:
    - Proxy-on-prototype delay injection for streaming benchmarks
    - process.memoryUsage delta-from-baseline pattern for accurate per-run peak measurement
    - Append-only markdown bench log with ISO timestamps
key-files:
  created:
    - packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts
    - .planning/milestones/v5.0-bench-results.md
  modified:
    - packages/zqlite-rs/src/pipeline_manager.rs (added streaming_channel_bounded_blocks_when_full test)
decisions:
  - 'PERF-03 memory measurement uses peak DELTA from a per-run baseline (not absolute rss+heapUsed). Node RSS rarely shrinks back to OS, so absolute peaks across sequential runs are monotonically increasing — second run always looks bigger regardless of workload. Captured baseline at poller start, peak() returns max(0, peakAbs - baseline).'
  - 'PERF-02 fixture uses 50ms minimum delay (raised from CONTEXT D-15 baseline of 10ms per RESEARCH P-04) to avoid CI scheduler jitter at sub-50ms granularity.'
  - 'Both bench tests use vitest test() (not bench()) per RESEARCH §2.3 — assertion + log-to-file requires explicit per-iteration control that bench() does not give cleanly.'
  - 'PERF-03 uses categories filtering (4 distinct queries on widgets table by category column) so the 4 pipelines are genuinely independent and the streaming/buffered memory delta is observable.'
metrics:
  duration_seconds: 422
  duration_human: 7m 2s
  completed: 2026-04-30
  tasks_completed: 5
  files_created: 2
  files_modified: 1
  commits: 5
---

# Phase 33 Plan 03: Performance Tuning + Benchmarks Summary

**One-liner:** Three streaming-perf benchmarks (PERF-01 Rust channel-bound + PERF-02 TTFB + PERF-03 memory-peak) deferred from Phases 31/32, recorded to an append-only markdown log.

## Path Summary

- **1 Rust unit test** added to `packages/zqlite-rs/src/pipeline_manager.rs::streaming_tests` (50 lines).
- **1 TS bench file** created at `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts` containing 2 vitest `test()` blocks (PERF-02 TTFB + PERF-03 memory peak), 552 lines total.
- **1 markdown log file** created at `.planning/milestones/v5.0-bench-results.md` with explanatory header (29 lines) plus 2 result lines per bench run.

## Bench Results (final canonical run)

```
2026-04-29T18:28:36.613Z TTFB-streaming first_chunk_ms=51.51 min_pipeline_ms=50 ratio=1.03x threshold=1.5x PASS
2026-04-29T18:28:36.921Z MemPeak-streaming buffered_mb=93.75 streaming_mb=0.88 ratio=106.59x expected=4x PASS
```

### PERF-01 (Rust) Verdict

`streaming_channel_bounded_blocks_when_full` **PASSES**. Confirmed:

- Production capacity formula `pipeline_count.max(1) + 1` yields 5 for `pipeline_count=4`.
- Filling to capacity succeeds; the 6th `try_send` returns `Err(TrySendError::Full(99))`.
- After draining one item, a previously-blocked sender unblocks (`try_send(99)` succeeds).
- Queue ordering preserved (`0,1,2,3,4,99` drained in order).
- Test is fully deterministic (no thread sleeps, no timing flake) — runs in 0.00s.

### PERF-02 (TTFB) Verdict — PASS

- 4 pipelines staggered at 50/100/250/500ms via Proxy-on-prototype patch on `RustPipelineManager.prototype.advanceStreaming`.
- First-RowChange arrived 51.51ms after kickoff, 1.03× the fastest pipeline's 50ms delay.
- Threshold: 1.5×. Margin: ample.

### PERF-03 (Memory Peak) Verdict — PASS

- Workload: 4 pipelines × 2500 rows = 10K total rows, on a 5-column `widgets` table.
- Buffered (`advanceAsync`) peak delta: 93.75 MB.
- Streaming (`advanceStreaming`) peak delta: 0.88 MB.
- Ratio: 106.59× — far better than the 4× expected lower bound.
- Streaming reduces peak from `O(total_changes)` to ~`O(max_pipeline_changes)` as designed.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 — Bug] Memory poller used absolute peak instead of delta-from-baseline**

- **Found during:** Task 4 (PERF-03 implementation)
- **Issue:** Initial implementation sampled `process.memoryUsage().rss + heapUsed` and tracked the absolute peak. Because Node's RSS rarely shrinks back to the OS, sequential runs (buffered → streaming) always saw monotonically increasing peaks — the streaming run always inherited all of the buffered run's residual RSS plus its own. First run produced `streaming_mb=247.45 buffered_mb=229.29 ratio=0.93x FAIL` even though streaming should massively beat buffered.
- **Fix:** Captured a baseline `m0.rss + m0.heapUsed` at poller construction, returned `max(0, peakAbs - baseline)` as the peak. Each run measures its own incremental allocation footprint, isolating it from prior-run residual.
- **Files modified:** `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts` (`startPeakPoller` function)
- **Commit:** ff37ce2ce (the fix and the bench file are in the same commit; the FAIL/PASS sequence is recorded in commit messages and bench-results.md history).
- **Verification after fix:** Streaming peak delta drops to 0.88 MB (from 247.45 MB absolute), buffered peak delta is 93.75 MB. Ratio: 106.59× (PASS).

### Non-deviations

- All thresholds, fixtures, channel-sizing literal, Proxy-on-prototype pattern, and bench-results format match the plan exactly.
- All test-task verification commands pass on the first invocation (after the inline Rule 1 fix above).

## Threat Mitigations Applied

Per the plan's threat model:

- **T-33-03-01** (Proxy-on-prototype patch leaks across tests if restore fails): Both bench tests use `try { ... } finally { restore(); }`. Verified by running `pipeline-driver.streaming.test.ts` after the bench file — all 6 streaming tests pass cleanly. Restore is effective.
- **T-33-03-02** (CI-flaky timing assertions): TTFB threshold loose at 1.5× with 50ms minimum (raised from CONTEXT-suggested 10ms per RESEARCH P-04). Memory threshold loose at 1.2× with WARN-instead-of-throw for small margins (1.2-2.0×); only severe (>2× or >3×) failures throw.
- **T-33-03-04** (Memory measurement noise from V8 GC): Mitigated via per-run baseline subtraction (Rule 1 fix above) and optional `--expose-gc` invocation guarded by `typeof gc === 'function'`.

## Proxy-on-prototype Restore Verified

After running `rust-ivm-streaming-bench.test.ts` (which patches `RustPipelineManager.prototype.advanceStreaming`), executed:

```
npx vitest run --no-coverage src/services/view-syncer/pipeline-driver.streaming.test.ts
```

Result: 6 tests passed in 1.02s. The Proxy-on-prototype patch is properly restored in the bench's `finally` block, leaving no pollution.

## Regression Suite Results

| Suite | Result | Baseline | New |
|-------|--------|----------|-----|
| `cargo test --release -p zqlite-rs --lib` | 129 passed, 0 failed, 1 ignored | 128 + 1 ignored (Phase 31) | +1 (PERF-01) |
| `cargo test --release -p zero-ivm-rs --lib` | 170 passed, 0 failed | 170 (Phase 31) | unchanged |
| `pipeline-driver.streaming.test.ts` | 6 passed | 6 (Phase 31) | unchanged |
| `rust-ivm-streaming-bench.test.ts` | 2 passed | n/a (new) | +2 |

All baselines preserved. No regressions.

## Recommendation for CI Integration (out of scope per CONTEXT deferred-ideas)

Observations the operator may use for ops decisions:

- TTFB bench (PERF-02) total runtime: ~1s. Safe to run on every PR.
- Memory bench (PERF-03) total runtime: ~0.3s after fixture warmup. Safe to run on every PR.
- Both bench thresholds are loose enough (1.5× TTFB / 1.2× memory) that they should not flake under typical CI scheduler jitter on shared runners.
- The append-only bench-results log preserves history for trend analysis. Operators reviewing PRs should compare the 2 newest lines to the prior baseline; a sudden FAIL or sustained WARN trend is a regression signal.

CI wiring is explicitly out of scope per CONTEXT deferred-ideas; the benches are runnable today via `npx vitest run src/services/view-syncer/rust-ivm-streaming-bench.test.ts`.

## Self-Check: PASSED

- File `packages/zqlite-rs/src/pipeline_manager.rs` (modified): FOUND
- File `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts` (created): FOUND
- File `.planning/milestones/v5.0-bench-results.md` (created): FOUND
- Commit `0b8e3f5b5` (Task 1): FOUND
- Commit `f18eb12e8` (Task 2): FOUND
- Commit `ff37ce2ce` (Task 3+4): FOUND
- Commit `328dd10e5` (Task 5): FOUND
