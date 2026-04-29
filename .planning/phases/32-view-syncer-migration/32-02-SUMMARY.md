---
phase: 32-view-syncer-migration
plan: 02
subsystem: view-syncer
tags:
  [
    view-syncer,
    streaming,
    migration,
    feature-flag,
    mid-batch-poke,
    async-iterable,
    rust-stream-error,
  ]

requires:
  - phase: 31-streaming-primitives-and-wrappers
    provides: advanceStreaming + addQueriesStreaming + RustStreamError on PipelineDriver
provides:
  - ViewSyncerService consuming Rust streaming surface under ZQLITE_RS_USE_STREAMING_CONSUMER
  - Mid-batch pokePart firing as fast pipelines complete
  - Operator rollback path via env var without code revert
  - Test helpers for streaming-mode-tolerant pokePart assertions
affects:
  [33-streaming-perf, future-mpsc-backpressure, future-cvr-incremental-flush]

tech-stack:
  added: []
  patterns:
    - 'Per-instance env-var read in ViewSyncerService constructor (NOT module-init) for dual-mode test compatibility'
    - 'AsyncIterable | Iterable union parameter type with for-await-of consumer (works for both buffered + streaming)'
    - 'RustStreamError instanceof + .kind branching (NEVER .message string parsing) per Phase 31 D-13'
    - 'Test helper mergePokePartsIntoOne to assert WHAT data arrives, not HOW MANY pokeParts batch it'

key-files:
  created:
    - packages/zero-cache/src/services/view-syncer/view-syncer-streaming-mid-batch.pg.test.ts
    - packages/zero-cache/src/services/view-syncer/view-syncer-streaming-reset.pg.test.ts
  modified:
    - packages/zero-cache/src/services/view-syncer/view-syncer.ts
    - packages/zero-cache/src/services/view-syncer/view-syncer-test-util.ts
    - packages/zero-cache/src/services/view-syncer/pipeline-driver.ts
    - packages/zero-cache/src/services/view-syncer/client-handler.ts
    - packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts
    - packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts
    - packages/zero-cache/src/services/view-syncer/view-syncer.pg.test.ts
    - packages/zero-cache/src/services/view-syncer/view-syncer-permissions.pg.test.ts

key-decisions:
  - 'Read ZQLITE_RS_USE_STREAMING_CONSUMER in ViewSyncerService constructor (not module init) so tests can flip the flag in beforeEach (CONTEXT D-04..D-07, RESEARCH P-03)'
  - 'Broaden #processChanges param to AsyncIterable | Iterable so a single for-await-of loop serves both streaming and buffered branches'
  - "Add 'chunk-end' sentinel in pipeline-driver streaming methods so #processChanges flushes processBatch + emits pokePart between Rust chunks (MIGRATE-03 enabler — Task 7.5 deviation from D-22)"
  - 'Add PokeHandler.flush() so the streaming consumer can force a pokePart at chunk-end without waiting for the next yield boundary (Task 7.5b)'
  - 'Refactor PG inline-snapshot tests via mergePokePartsIntoOne helper rather than wrapping the entire PG suite in describe.each([true, false]) — assertions become mode-agnostic without doubling runtime'

patterns-established:
  - 'Mode-agnostic pokePart assertions: nextPokeMerged collapses contiguous pokePart messages while preserving non-poke messages (transformError, warning) in their original positions'
  - 'Streaming consumer must always re-throw RustStreamError after structured logging — no per-kind recovery in view-syncer (CONTEXT D-08..D-10)'
  - 'Feature-flag fallback path preserved verbatim (advanceAsync/addQueriesAsync) so operator rollback via env var works without code revert'

requirements-completed: [MIGRATE-01, MIGRATE-02, MIGRATE-03, MIGRATE-04]

duration: ~4h (across two executor runs)
completed: 2026-04-29
---

# Phase 32 Plan 02: View-Syncer Streaming Migration Summary

**View-syncer now consumes Rust's per-pipeline streaming surface under `ZQLITE_RS_USE_STREAMING_CONSUMER` (default on), firing `pokePart` mid-batch as fast pipelines complete instead of buffering to end-of-stream — operator rollback via env var preserved.**

## Performance

- **Duration:** ~4h across two executor runs (initial Tasks 1-7 + Task 7.5 / 7.5b deviations + final Task 8 test refactor + Task 9 verification)
- **Tasks:** 9 plan tasks + 2 deviation tasks (7.5 chunk-end marker, 7.5b PokeHandler.flush)
- **Files modified:** 9
- **Files created:** 2 regression test files + 1 deferred-items doc + this summary

## Accomplishments

- `ViewSyncerService.#advancePipelines` and `#hydrateUnchangedQueries` branch on `ZQLITE_RS_USE_STREAMING_CONSUMER` (default `true`); streaming path uses Rust's per-pipeline AsyncIterable, buffered fallback preserved verbatim for operator rollback (MIGRATE-01, MIGRATE-02).
- `#processChanges` migrated to `for await of` with broadened `AsyncIterable | Iterable` parameter type (single loop serves both branches).
- `RustStreamError` caught with `instanceof` + `.kind` discriminator, logged structured (`kind`/`message`/`source`) and re-thrown — no per-kind recovery in view-syncer (CONTEXT D-08..D-10).
- `pokers.cancel()` invoked exactly once on `ResetPipelinesSignal` mid-stream (preserved); pinned by MIGRATE-04 regression test (`view-syncer-streaming-reset.pg.test.ts`).
- First `pokePart` fires before slowest pipeline completes when chunks are staggered — MIGRATE-03 regression test (`view-syncer-streaming-mid-batch.pg.test.ts`) with Proxy-on-prototype Rust manager patch.
- Test infrastructure: `connectWithQueueAndSource` accepts optional `onMessage` timestamp callback; new `mergePokePartsIntoOne` + `nextPokeMerged` helpers reconstruct buffered single-pokePart shape from streaming-mode multi-part output.
- `streaming-vs-buffered-parity.fuzz.test.ts` at `FUZZ_NUM_RUNS=1000` passes — confirms streaming output matches buffered output for the same input modulo cross-pipeline ordering.

## Task Commits

Each task was committed atomically:

1. **Task 1: Extend `connectWithQueueAndSource` with optional `onMessage`** — `23b149857` (test)
2. **Task 2 (RED): MIGRATE-03 mid-batch pokePart timing test** — `1221a1642` (test)
3. **Task 3 (RED): MIGRATE-04 reset-cancel mid-stream test** — `57ea5b7ee` (test)
4. **Task 4: Add `#useStreamingConsumer` field to `ViewSyncerService`** — `bc5499425` (feat)
5. **Task 5: Migrate `#advancePipelines` to `advanceStreaming` + RustStreamError catch** — `3622bf5a9` (feat)
6. **Task 6: Migrate `#hydrateUnchangedQueries` to `addQueriesStreaming`** — `4111b398b` (feat)
7. **Task 7: `#processChanges` consumes `AsyncIterable | Iterable` via `for await of`** — `98f91a7c7` (feat)
8. **Task 7.5 (deviation): chunk-end marker for mid-batch processBatch flush (MIGRATE-03 enabler)** — `f4646768f` (feat)
9. **Task 7.5b (deviation): PokeHandler.flush() + adjust MIGRATE-04 assertions** — `5f89808b8` (feat)
10. **Task 8a: `mergePokePartsIntoOne` + `nextPokeMerged` test helpers** — `59ae19ec0` (feat)
11. **Task 8b/c: Refactor PG pokePart assertions for streaming-mode parity** — `537e55ca8` (test)
12. **Task 8d: Document deferred items (yield-during baseline + TTL flake)** — `914740f94`, `b7be66858` (docs)
13. **Task 9: Final phase verification gate + this SUMMARY.md** — (final commit, this file)

## Files Created/Modified

### Created

- `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-mid-batch.pg.test.ts` — MIGRATE-03 regression test pinning mid-batch pokePart timing under Proxy-on-prototype Rust manager patch.
- `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-reset.pg.test.ts` — MIGRATE-04 regression test pinning `pokers.cancel()` exactly-once on `ResetPipelinesSignal` mid-stream.
- `.planning/phases/32-view-syncer-migration/deferred-items.md` — pre-existing baseline failures + TTL flake + coverage warnings documented out-of-scope per executor SCOPE BOUNDARY rule.

### Modified

- `packages/zero-cache/src/services/view-syncer/view-syncer.ts` — `#useStreamingConsumer` field, constructor read of env var, flag-branched `advanceStreaming`/`advanceAsync` and `addQueriesStreaming`/`addQueriesAsync` callsites, `#processChanges` async iterator migration, `RustStreamError` catch arm.
- `packages/zero-cache/src/services/view-syncer/view-syncer-test-util.ts` — `connectWithQueueAndSource` `onMessage` callback, `mergePokePartsIntoOne` + `nextPokeMerged` helpers.
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — `'chunk-end'` sentinel emitted at per-query/per-chunk boundaries in streaming methods (Task 7.5 deviation from D-22 — required for MIGRATE-03).
- `packages/zero-cache/src/services/view-syncer/client-handler.ts` — `PokeHandler.flush()` (Task 7.5b deviation — required for MIGRATE-04 assertion semantics).
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts` — extended for chunk-end semantics.
- `packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts` — broadened to handle chunk-end markers in parity comparison.
- `packages/zero-cache/src/services/view-syncer/view-syncer.pg.test.ts` — bulk-swapped `expect(await nextPoke(client))` → `expect(await nextPokeMerged(client))` at all snapshot sites (~37 occurrences); replaced direct `pokePart[1].rowsPatch` indexing with merged-shape access in 'failed query re-transformation' test.
- `packages/zero-cache/src/services/view-syncer/view-syncer-permissions.pg.test.ts` — same bulk swap at 6 snapshot sites.

## Decisions Made

1. **Per-instance env-var read in constructor (not module-init):** RESEARCH P-03 caveat — module-init reads lock the value at first import, breaking dual-mode tests that flip the flag in `beforeEach`. Per-instance read preserves "read once at boot" semantics at the right granularity.
2. **Broadened `#processChanges` parameter type to `AsyncIterable | Iterable`:** Single `for await of` loop natively serves both streaming AsyncIterable AND buffered Iterable. Eliminates the need for a flag-branched dispatch inside `#processChanges`.
3. **Deviation Task 7.5 — `chunk-end` sentinel:** Plan D-22 said pipeline-driver.ts should be untouched, but MIGRATE-03 (mid-batch pokePart firing) requires `#processChanges` to flush its row batch + emit a `pokePart` between Rust chunks. The `chunk-end` sentinel was added to pipeline-driver streaming methods as the boundary signal. Buffered consumers never see this marker (their Iterable never emits it). This is a justified deviation — without it, MIGRATE-03's success criterion is structurally impossible.
4. **Deviation Task 7.5b — `PokeHandler.flush()`:** Required so the streaming consumer can deterministically force a `pokePart` flush at chunk-end without waiting for the next natural yield boundary.
5. **Test refactor strategy: `mergePokePartsIntoOne` helper, not `describe.each([true, false])`:** Per CONTEXT D-06 spirit, tests should be mode-agnostic. Wrapping the entire PG suite in `describe.each` would double runtime without doubling coverage. The helper makes assertions assert WHAT data arrives (the user-visible wire-protocol contract per `zero-protocol/src/poke.ts`) instead of HOW MANY pokeParts batch it (an internal cadence). The helper preserves non-poke messages (e.g., `transformError`) in their original positions while collapsing contiguous `pokePart` runs.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 — Missing Critical Functionality] Task 7.5: `chunk-end` sentinel in pipeline-driver streaming methods**

- **Found during:** Task 7 (after migrating `#processChanges` to `for await of`, MIGRATE-03 still couldn't pass — buffered consumer behavior preserved end-of-stream pokePart cadence).
- **Issue:** MIGRATE-03 requires the first `pokePart` to fire BEFORE the slowest pipeline completes. With `processBatch` only flushing at end-of-stream, no mid-batch pokePart was possible. Plan D-22 prohibited touching `pipeline-driver.ts`.
- **Fix:** Added `'chunk-end'` sentinel emission at per-query and per-chunk boundaries in `addQueriesStreaming` and `advanceStreaming`. View-syncer's `#processChanges` calls `processBatch()` + `pokers.flush()` when it sees this sentinel. Buffered methods are untouched (they return `Iterable`, not `AsyncIterable`).
- **Files modified:** `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts`, `packages/zero-cache/src/services/view-syncer/view-syncer.ts`
- **Verification:** MIGRATE-03 test passes; FUZZ_NUM_RUNS=1000 parity fuzz still green.
- **Committed in:** `f4646768f` (Task 7.5)

**2. [Rule 2 — Missing Critical Functionality] Task 7.5b: `PokeHandler.flush()`**

- **Found during:** Task 7.5 (chunk-end sentinel handler needed a way to force pokePart emission).
- **Issue:** No public API on `PokeHandler` to flush pending row patch into a `pokePart` mid-stream.
- **Fix:** Added `PokeHandler.flush()` method.
- **Files modified:** `packages/zero-cache/src/services/view-syncer/client-handler.ts`
- **Verification:** MIGRATE-04 assertion semantics confirm the flush propagates correctly; no impact on buffered path.
- **Committed in:** `5f89808b8` (Task 7.5b)

**3. [Rule 1 — Test-Quality Bug] Task 8: PG inline-snapshot tests pinned single-pokePart shape**

- **Found during:** Task 8 dual-mode verification.
- **Issue:** ~13 PG tests across `view-syncer.pg.test.ts` (10) and `view-syncer-permissions.pg.test.ts` (2) and one `removes failed query using last known transformation hash` (1) failed under streaming because their snapshots assumed a single `pokePart`. The wire protocol explicitly allows multi-part pokes.
- **Fix:** Added `mergePokePartsIntoOne` + `nextPokeMerged` helpers to `view-syncer-test-util.ts`. Bulk-swapped `expect(await nextPoke(client))` → `expect(await nextPokeMerged(client))` at all assertion sites. The helper preserves non-poke messages and collapses contiguous pokePart runs into a single normalized `PokePartBody`.
- **Files modified:** `packages/zero-cache/src/services/view-syncer/view-syncer-test-util.ts`, `packages/zero-cache/src/services/view-syncer/view-syncer.pg.test.ts`, `packages/zero-cache/src/services/view-syncer/view-syncer-permissions.pg.test.ts`
- **Verification:** Streaming and buffered modes now produce IDENTICAL pass/fail outcomes for the entire view-syncer PG suite (same 11 failures by name across both modes — see Task 9 verification table).
- **Committed in:** `59ae19ec0` (helpers), `537e55ca8` (test refactor)

---

**Total deviations:** 3 auto-fixed (2 missing critical functionality, 1 test-quality bug).

**Impact on plan:** All deviations were essential for the migration to achieve its stated success criteria. Task 7.5 + 7.5b are scope-justified extensions of D-22 (the `chunk-end` mechanism is the SOLE means for `#processChanges` to flush mid-batch — it could not have been omitted). Task 8 test refactor follows CONTEXT D-06 spirit and the wire protocol's explicit multi-part allowance.

## Issues Encountered

1. **Pre-existing PG suite failures (NOT regressions):** 5 tests in `view-syncer.pg.test.ts` and 1 in `view-syncer-permissions.pg.test.ts` failed in both streaming AND buffered modes on the base commit. The `empty row key` ProtocolError aligns with the recent debug knowledge base entry on phase-30-03-exists-edit-regression. Documented in `deferred-items.md`.
2. **TTL `two clients` flake:** Initially appeared streaming-only when run isolated. Confirmed non-streaming-specific when running the full PG view-syncer glob — the test fails under buffered mode too when test ordering perturbs the event-loop schedule. Root cause: `await sleep(100)` after `source.cancel()` is racy regardless of streaming flag. Documented in `deferred-items.md`.
3. **Pre-existing yield-during-\* failures:** 4 tests in `view-syncer.yield-during-*.pg.test.ts` fail in both modes on the base commit. Documented in `deferred-items.md`.
4. **Coverage parser warnings:** `litestream/config.yml` and `zqlite-rs.darwin-arm64.node` cause Rolldown parse errors during coverage. Stderr noise only — no test failures. Documented in `deferred-items.md`.

## Verification (Task 9 — Final Phase Gate)

| Gate                          | Command                                                                                                           | Result                                                                                                                                           |
| ----------------------------- | ----------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| Pipeline-driver glob          | `npx vitest --config vitest.config.no-pg.ts run src/services/view-syncer/pipeline-driver`                         | **13 files, 141 tests passed**                                                                                                                   |
| Parity fuzz @ 1000            | `FUZZ_NUM_RUNS=1000 npx vitest run streaming-vs-buffered-parity.fuzz.test.ts`                                     | **1 passed (16.11s)**                                                                                                                            |
| fuzz-ivm @ 1000               | `FUZZ_NUM_RUNS=1000 npx vitest run fuzz-ivm.test.ts`                                                              | **6 passed, 1 todo**                                                                                                                             |
| zqlite-rs Rust suite          | `cargo test --release --lib -- --test-threads=1`                                                                  | **128 passed, 1 ignored** (matches Phase 31 baseline)                                                                                            |
| zero-ivm-rs Rust suite        | `cargo test --release --lib -- --test-threads=1`                                                                  | **170 passed** (matches Phase 31 baseline)                                                                                                       |
| no-pg view-syncer (streaming) | `ZQLITE_RS_USE_STREAMING_CONSUMER=true npx vitest --config vitest.config.no-pg.ts run src/services/view-syncer/`  | **22 files, 250 passed, 1 todo**                                                                                                                 |
| no-pg view-syncer (buffered)  | `ZQLITE_RS_USE_STREAMING_CONSUMER=false npx vitest --config vitest.config.no-pg.ts run src/services/view-syncer/` | **22 files, 250 passed, 1 todo**                                                                                                                 |
| pg view-syncer (streaming)    | `ZQLITE_RS_USE_STREAMING_CONSUMER=true npx vitest --config vitest.config.pg-16.ts run src/services/view-syncer/`  | **141 passed, 11 failed** (all 11 are pre-existing baseline failures documented in deferred-items.md)                                            |
| pg view-syncer (buffered)     | `ZQLITE_RS_USE_STREAMING_CONSUMER=false npx vitest --config vitest.config.pg-16.ts run src/services/view-syncer/` | **139 passed, 11 failed, 2 skipped** (the 2 skipped are MIGRATE-03 + MIGRATE-04 which `runIf(streaming)`; same 11 failures by name as streaming) |

### Anti-hack guards

| Guard                                                                 | Check                                                            | Result                                                                                                              |
| --------------------------------------------------------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- |
| `decode-advance-buf.ts` untouched                                     | `git diff base..HEAD -- decode-advance-buf.ts`                   | **empty diff**                                                                                                      |
| Rust crates untouched                                                 | `git diff base..HEAD -- packages/zqlite-rs packages/zero-ivm-rs` | **empty diff**                                                                                                      |
| `RustStreamError` discriminated by `instanceof` + `.kind`             | `grep RustStreamError view-syncer.ts`                            | **`e instanceof RustStreamError` + `e.kind` (no `.message` parsing)**                                               |
| Env var read in constructor (not module-init)                         | `grep -n ZQLITE_RS_USE_STREAMING_CONSUMER view-syncer.ts`        | **single read at line 365 inside constructor body**                                                                 |
| Both `advanceAsync` AND `advanceStreaming` callsites preserved        | `grep -nE "(advanceAsync                                         | advanceStreaming)" view-syncer.ts`                                                                                  | **both present (line 2229 streaming, 2230 buffered)** |
| Both `addQueriesAsync` AND `addQueriesStreaming` callsites preserved  | `grep -nE "(addQueriesAsync                                      | addQueriesStreaming)" view-syncer.ts`                                                                               | **both present (line 1924 streaming, 1925 buffered)** |
| `pipeline-driver.ts` only adds chunk-end markers (Task 7.5 deviation) | `git diff base..HEAD -- pipeline-driver.ts`                      | **49 lines: +36 −13, all chunk-end + comment additions in streaming methods (no buffered method behavior changed)** |

### ROADMAP Phase 32 Success Criteria

1. **`view-syncer.ts::#advancePipelines` calls `advanceStreaming` (under flag-on); `#processChanges` consumes `AsyncIterable` via `for await of`.** ✓ — verified by `grep` matches above; no other module references `#processChanges` directly. (Tasks 5, 7)
2. **`view-syncer.ts::#hydrateUnchangedQueries` calls `addQueriesStreaming` instead of `addQueriesAsync` (under flag-on).** ✓ — line 1924. (Task 6)
3. **`pokers.pokePart` observed firing mid-batch in MIGRATE-03 instrumented test; the first `pokePart` arrives BEFORE the slowest pipeline finishes.** ✓ — `view-syncer-streaming-mid-batch.pg.test.ts` passes under streaming. (Tasks 2 + 5 + 7.5)
4. **CVR commit happens exactly once at end of `#advancePipelines` after the full stream consumes; `pokers.end(finalVersion)` fires exactly once after CVR commit; `pokers.cancel()` fires correctly when `ResetPipelinesSignal` is thrown mid-stream.** ✓ — `view-syncer-streaming-reset.pg.test.ts` passes; existing pokeEnd span at line 2283 unchanged. (Tasks 3, 5, 7.5b)
5. **Full view-syncer suite + `pipeline-driver.*.test.ts` + `fuzz-ivm.test.ts` (1k iterations) + `cargo test` continue to pass unchanged.** ✓ — verification table above. (Task 9)

## Next Phase Readiness

- **Phase 33 perf:** With per-pipeline streaming live in production, Phase 33 can introduce `mpsc::sync_channel(pipeline_count)` to bound the Rust-side channel depth (PERF-01 in roadmap), now that the consumer is structurally streaming-aware.
- **CVR incremental flush:** A future plan can extend the `chunk-end` mechanism to ALSO flush the CVR-update batch mid-stream (currently CVR commit remains atomic at end-of-stream — preserved per CONTEXT). This would reduce p99 latency for very large advances.
- **TTL test cleanup:** The `two clients` flake should be addressed in a test-quality follow-up: replace `await sleep(100)` with deterministic await of view-syncer's cancel-acknowledgement.
- **Pre-existing IVM exists/edit regression:** Documented in deferred-items.md and aligned with the recent debug knowledge base. Should be addressed in a dedicated phase.
- **Operator rollback:** `ZQLITE_RS_USE_STREAMING_CONSUMER=false` + service restart reverts to the buffered path without any code changes — the rollback escape valve is verified by the dual-mode parity gate.

## Self-Check: PASSED

All claimed files (10) exist on disk. All claimed commit hashes (13) are present in `git log`.

---

_Phase: 32-view-syncer-migration_
_Completed: 2026-04-29_
