---
status: issues_found
phase: 33-performance-tuning
reviewed: 2026-04-29
depth: standard
files_reviewed: 6
findings:
  critical: 0
  warning: 6
  info: 5
  total: 11
---

# Phase 33: Code Review Report

**Reviewed:** 2026-04-29
**Depth:** standard
**Files Reviewed:** 6
**Status:** issues_found (no blockers, advisory)

## Summary

Reviewed Phase 33 (Performance Tuning + Benchmarks) deliverables across HARDEN-01, HARDEN-02, and PERF.

Verified the four phase-context constraints:

- **Hard constraint (no signature changes):** PASS. New exports added (`Streamer`, `getRowKey`, `mustGetPrimaryKey`) but no existing buffered-method signatures changed.
- **D-28 anti-hack guardrail (or_exists_op.rs lines 1-446 byte-identical):** PASS. Verified via `git diff` — only test code appended.
- **PERF-03 peak-delta-from-baseline rationale:** Implementation matches documented rationale.
- **33-01 deviation (`tsAdvance` empty in mono-rs):** Documented and matches behavior.

## Critical Issues

_None._

(Reviewer initially flagged CR-01 about a wrong PipelineDriver constructor signature in `parity-check.test.ts:136-147`. Manually verified — false positive: slot 5 is `new DatabaseStorage(storage).createClientGroupStorage('parity-check-cg')` which returns the `ClientGroupStorage`, the string is internal to that helper, and slots 6-8 correctly map to clientGroupID/inspectorDelegate/yieldThresholdMs. Test compiled and ran 15/15 pass during agent verification.)

## Warnings

### WR-01: Strict-mode parity check throws on legitimate empty TS oracle (advance path)

**File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:503-527`

`#maybeRunParityCheck` skips when `tsChanges.length === 0` only in non-strict mode. In strict mode it falls through to `compareChanges([], rustChanges)` and throws if Rust produced any output. Since `tsAdvance` always returns empty in mono-rs (documented mono-rs limitation), enabling `ZQLITE_RS_PARITY_CHECK=strict` in production immediately throws on first advance with any Rust output, regardless of correctness. Strict mode is currently unusable for the advance path.

**Fix:** Either (a) document this loudly in env-var help and oracle file header, (b) treat empty-TS as "no comparison performed" in strict mode for `advance` path, or (c) gate strict mode to only run on `hydrate` path.

### WR-02: Cadence test for `mode=sample, rate=10` does not exercise the divergence path

**File:** `packages/zero-cache/src/services/view-syncer/parity-check.test.ts:242-277`

With `rate=10` and only 1 invocation, the comparison is short-circuited (`1 % 10 !== 0`). Test passes trivially because `divergenceCount === 0` from `beforeEach`, not because comparison ran and found no divergence. A typo in `#shouldRunParityCheck` would still pass this test.

**Fix:** Either add an assertion that divergence count remains 0 after enough invocations to fire the comparison, or assert the gating explicitly via `getParityCheckInvocationCountForTesting() % 10 !== 0`.

### WR-03: PERF-02 TTFB measurement model + tautological assertion

**File:** `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts:319-403`

`patchAdvanceStreamingWithDelays` injects delay AFTER awaiting `target.next()`, so each chunk arrives at `(rust_chunk_time + delay)` not `(delay)`. Threshold `1.5 * minDelay = 75ms` may be too tight on slow CI. Also: `expect(firstChunkT!).toBeLessThan(minDelay * threshold)` (line 397) is identical to the `verdict === 'PASS'` precondition (line 382) — tautological, always passes if verdict is PASS.

**Fix:** Move delay BEFORE awaiting `target.next()`. Drop redundant `expect()` after PASS branch.

### WR-04: PERF-03 memory-peak log line shows misleading `expected=4x`

**File:** `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts:531-549`

The log line `expected=${N_PIPELINES}x` shows `expected=4x` but the actual PASS threshold ratio is `N_PIPELINES / 1.2 ≈ 3.33x`. An on-call engineer reading "ratio=2.5x expected=4x" might think 2.5 < 4 means PASS but actual threshold corresponds to ratio ≥ 3.33x.

**Fix:** Log `pass_threshold=${(N_PIPELINES / 1.2).toFixed(2)}x` instead.

### WR-05: `messagesBuf` shared across two independent fixtures

**File:** `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts:501-512`

Line 453 constructs `messagesBuf` for buffered run; line 503 reuses same instance for streaming-run transaction (different replicator). If `ReplicationMessages` is per-replicator-stateful (mutationID/LSN counters), streaming-run inserts may use wrong sequence numbers.

**Fix:** Construct separate `messagesStr = new ReplicationMessages({...})` for streaming fixture.

### WR-06: `test_or_exists_in_push_flag_cleared_after_push` does not actually verify flag was cleared

**File:** `packages/zero-ivm-rs/src/or_exists_op.rs:1034-1058`

Test asserts second push panics. If `in_push` was NOT cleared after first push, then `force_in_push_for_test()` is a no-op and second `push(...)` still panics — same outcome, satisfies `#[should_panic]`. Test does not distinguish the two cases.

**Fix:** Add explicit accessor `pub(crate) fn is_in_push_for_test(&self) -> bool` and assert false after first push completes.

## Info

### IN-01: Unused `eslint-disable` in test file (project uses oxlint)

**File:** `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts:224-268`

`/* eslint-disable @typescript-eslint/no-explicit-any */` wraps `patchAdvanceStreamingWithDelays` but body uses no `any`. Repo is on oxlint, not eslint. Remove both lines.

### IN-02: `tsAdvance` stub doc rationale at body, not just file header

**File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver-ts-oracle.ts:88-98`

Add a one-line rationale comment at the function body so future readers don't need to scroll to file header.

### IN-03: TS oracle `tsAddQuery` skips `Debug` delegate

**File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver-ts-oracle.ts:124-152`

Production `addQueriesAsync` constructs a Debug delegate when `runtimeDebugFlags.trackRowsVended` is true; oracle does not. Vended-row counts may differ. Low-risk (debug-only path) but worth documenting in oracle header's "Differences from upstream reference".

### IN-04: Magic number `200` for yield threshold duplicated across test files

**File:** `parity-check.test.ts:147`, `rust-ivm-streaming-bench.test.ts:153`

Extract to shared `TEST_YIELD_THRESHOLD_MS` constant. Low priority.

### IN-05: `parityDivergenceCount += result.mismatches.length` semantics ambiguous

**File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:518`

Counter increments by mismatch count per call. Unclear if it represents "events" or "row-level mismatches". Document semantics or expose two counters (`divergenceEventCount` / `rowMismatchCount`).

---

_Reviewed: 2026-04-29_
_Depth: standard_
_All findings advisory — no blockers for phase completion._
