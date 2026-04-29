# Phase 32: View-Syncer Streaming Migration - Context

**Gathered:** 2026-04-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Flip the production consumer (`view-syncer.ts`) from the buffered API to the streaming API so `pokePart` fires as fast pipelines complete instead of only at end-of-batch. CVR commit semantics, `pokers.end`, and `pokers.cancel` on reset are preserved exactly. This is the FIRST production consumer of the streaming surface shipped in Phase 31.

**In scope (4 requirements):**

- MIGRATE-01: `#advancePipelines` → `advanceStreaming`; `#processChanges` → `for await of`
- MIGRATE-02: `#hydrateUnchangedQueries` → `addQueriesStreaming`
- MIGRATE-03: `pokers.pokePart` fires mid-batch as fast pipelines complete (instrumented test)
- MIGRATE-04: `pokers.cancel()` fires correctly when `ResetPipelinesSignal` thrown mid-stream

**Plus one carry-forward gap from Phase 31 review:**

- CR-01 (i64 decoder threshold check happens after lossy arithmetic in `readJsonValue`) — pre-existing bug, but Phase 32 IS the first prod exposure of the chunk decoder. Address as plan 32-01 BEFORE the migration.

**Out of scope:**

- Performance microbenchmarks / TTFB measurement (Phase 33 PERF-01)
- Memory footprint benchmarks (Phase 33 PERF-02)
- Removing the `advanceAsync` / `addQueriesAsync` buffered methods (NOT in this milestone — kept indefinitely for tests/benches per IVM-STREAMING-PLAN.md §8)
- Removing the `ZQLITE_RS_USE_STREAMING_CONSUMER` feature flag (post-32 follow-up after one production cycle validates streaming)

</domain>

<decisions>
## Implementation Decisions

### Plan Granularity

- **D-01:** Two plans, sequential waves.
  - **32-01 (Wave 1, autonomous):** CR-01 fix in `decode-advance-buf.ts` — gate on `hi` magnitude before constructing the JS Number; add regression tests pinning `Number.MAX_SAFE_INTEGER ± 1`, `i64::MAX`, `i64::MIN`. Small standalone plan, ~3 tasks.
  - **32-02 (Wave 2, autonomous, depends_on [32-01]):** Full view-syncer migration — `#advancePipelines` + `#hydrateUnchangedQueries`, `RustStreamError` kind-aware error handling, `ZQLITE_RS_USE_STREAMING_CONSUMER` feature flag, MIGRATE-03 mid-batch pokePart test, MIGRATE-04 reset signal test. ~6-7 tasks.
- **D-02:** Why 32-01 separately: CR-01 is independent of view-syncer; isolating it keeps the migration's diff focused on view-syncer changes and reviewable as one unit. CR-01 fix is required first because both `decodeAdvanceResultBuf` (buffered) and `decodeAdvanceChunkBuf` (streaming) share the buggy `readJsonValue` helper — fixing it before the migration ensures both prod paths ship clean decoders.
- **D-03:** Why both view-syncer callsites in one plan: they touch the same file, same trace span structure, same error handling, same poker protocol. Separating them would create file-overlap conflicts in worktree mode and add ceremony without rollback granularity benefit.

### Rollback / Safety Strategy (D-04..D-07)

- **D-04:** Ship 32-02 with a feature flag `ZQLITE_RS_USE_STREAMING_CONSUMER`. Read once at view-syncer boot from `process.env.ZQLITE_RS_USE_STREAMING_CONSUMER`. Default after deploy validation: `true` (streaming).
- **D-05:** When flag is `false`, view-syncer calls the existing `advanceAsync` / `addQueriesAsync` buffered methods. When `true`, calls `advanceStreaming` / `addQueriesStreaming`. The flag is read at boot, not per-request — flipping requires a service restart, which is the safest semantics for "emergency rollback."
- **D-06:** Plan 32-02 must include both code paths and test both under the flag (two `describe.each([true, false])` blocks for the existing tests, plus the new mid-batch test runs only when flag is `true`).
- **D-07:** Removing the flag is OUT OF SCOPE for Phase 32. A follow-up phase (32.1 or part of milestone wrap-up) deletes the flag, the buffered fallback in view-syncer, and the dual-mode test wrapping. Document this deferral explicitly in `<deferred>` so it doesn't get forgotten.

### Error Handling — RustStreamError per kind (D-08..D-11)

- **D-08:** View-syncer wraps `#processChanges` invocations in `try { ... } catch (err) { if (err instanceof RustStreamError) { lc.error('rust streaming error', { kind: err.kind, message: err.message, source: err.source }); throw err; } throw err; }`. The `kind` discriminator goes into the structured log payload.
- **D-09:** All three kinds (`panic`, `rayon_error`, `channel_closed`) log + bubble to crash the view-syncer instance for that connection. Same semantics as today's buffered `advanceAsync` errors — client reconnects, which spawns a fresh view-syncer.
- **D-10:** Do NOT add per-kind recovery logic in view-syncer. The streaming surface is too new (first consumer) to second-guess panic causes mid-flight. Recovery via reconnect handles transient issues; persistent issues become observable via the structured `kind` field in logs.
- **D-11:** `ResetPipelinesSignal` is unchanged from today. Existing reset/recovery path applies — when thrown mid-stream, view-syncer rolls back the advance, calls `pokers.cancel()`, and the next iteration retries. The streaming wrapper's `try { for await ... } finally { stream.return_() }` (Phase 31 D-15) ensures Rust-side cancel fires even when ResetPipelinesSignal aborts the loop.

### CR-01 i64 Decoder Fix (D-12..D-14)

- **D-12:** Root cause: in `decode-advance-buf.ts:266-275`, the `i64` decoder constructs `n = hi * 0x100000000 + lo` BEFORE the safe-integer threshold check `n >= MAX_SAFE_INTEGER || n <= MIN_SAFE_INTEGER`. When `hi` is large (e.g., `0x80000000` for `i64::MIN`), the JS Number construction is already lossy — the threshold check then runs on a lossy value, defeating the guard.
- **D-13:** Fix: gate on `hi` magnitude before number construction. If `hi >= 0x200000` (the smallest `hi` such that the resulting Number could exceed `MAX_SAFE_INTEGER`), throw the existing-style error WITHOUT constructing the lossy Number. Otherwise proceed with `n = hi * 0x100000000 + lo` as today.
- **D-14:** Add regression tests in `decode-advance-buf.test.ts`:
  - `Number.MAX_SAFE_INTEGER` (boundary, should decode)
  - `Number.MAX_SAFE_INTEGER + 1` (just over, should throw)
  - `i64::MAX` (`0x7FFFFFFFFFFFFFFF` → should throw, NOT silently lossy-decode)
  - `i64::MIN` (`0x8000000000000000` → should throw, NOT silently lossy-decode)
  - Negative just under `MIN_SAFE_INTEGER` (should throw)

### MIGRATE-03 Test Strategy (D-15..D-17)

- **D-15:** Create `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-mid-batch.test.ts`. Mock pipelines that block on different timers (e.g., 10ms, 50ms, 200ms). Assert `pokePart` call timestamps interleave with per-pipeline completion: specifically, the FIRST `pokePart` fires BEFORE the slowest pipeline (200ms) completes.
- **D-16:** Synthetic test only — real-world workload fixture is Phase 33 (PERF-01) territory. Keep MIGRATE-03 verification atomic.
- **D-17:** Test runs only when `ZQLITE_RS_USE_STREAMING_CONSUMER=true`. Under buffered mode (`false`), `pokePart` mid-batch is impossible by construction, so the test is conditionally `it.runIf(useStreaming)` rather than expected to fail.

### MIGRATE-04 Reset-Cancel Test (D-18..D-19)

- **D-18:** Add an integration test that injects a companion-scalar change mid-batch (e.g., between pipeline 1 completing and pipeline 2 completing). Assert:
  - `pokers.cancel()` was called exactly once
  - `pokers.end()` was NOT called
  - No CVR commit happened
  - Client-side state is rolled back
- **D-19:** This test exercises both the streaming path's `try/finally { stream.return_() }` AND view-syncer's existing `ResetPipelinesSignal` recovery. Run under `ZQLITE_RS_USE_STREAMING_CONSUMER=true` only; buffered mode is covered by the existing reset tests.

### File Modifications (D-20..D-22)

- **D-20:** 32-01 modifies:
  - `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` — fix `readJsonValue` i64 path
  - `packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts` — add regression tests
- **D-21:** 32-02 modifies:
  - `packages/zero-cache/src/services/view-syncer/view-syncer.ts` — `#advancePipelines`, `#hydrateUnchangedQueries`, `#processChanges`, error catch block, feature flag read at boot
  - Existing `view-syncer.*.test.ts` files — wrap streaming-eligible tests in `describe.each([true, false])` for both modes
  - `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-mid-batch.test.ts` (NEW) — MIGRATE-03 instrumented test
  - `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-reset.test.ts` (NEW) — MIGRATE-04 cancel test
- **D-22:** Do NOT modify `pipeline-driver.ts` (Phase 31 surface — frozen). Do NOT modify any Rust files. Do NOT modify `addQueriesAsync` / `advanceAsync` / `decodeAdvanceResultBuf` (preserved per COMPAT-02/03 in Phase 31 — view-syncer feature flag relies on them existing unchanged).

### Build & Verification

- **D-23:** `assertNapiBinaryFreshness` (Phase 30-05) gate covers Phase 32 automatically. No new gate needed.
- **D-24:** Verifier MUST run with both `ZQLITE_RS_USE_STREAMING_CONSUMER=true` AND `ZQLITE_RS_USE_STREAMING_CONSUMER=false` to confirm both paths green. The full vitest suite must pass under each value.

### Claude's Discretion

- Specific instrumentation mechanism for the mid-batch pokePart test (timestamp array, mock spy, async barrier).
- Exact threshold value for `hi` magnitude check in CR-01 fix (`>= 0x200000` is one option; `>= 0x1FFFFF` is the safer-by-one alternative — pick whichever maps cleanly to `MAX_SAFE_INTEGER` boundary math).
- How to wrap existing view-syncer tests in `describe.each([true, false])` — could be a single shared wrapper module or per-test adoption. Pick whichever requires fewer line edits.
- Exact env var read site (boot vs per-instance) — boot is preferred for simplicity, but if there's an existing per-instance config object, follow that pattern.

### Folded Todos

None — no pending todos relevant to this phase.

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase 31 carry-forward (the streaming surface this phase consumes)

- `.planning/phases/31-streaming-primitives-and-wrappers/31-CONTEXT.md` — D-10..D-15 govern `RustStreamError` shape and TS streaming wrapper invariants. D-13 explicitly says "Phase 32's view-syncer migration MUST use `error.kind === 'panic'` / `instanceof RustStreamError` for branching, not string-matching on `.message`." Honor.
- `.planning/phases/31-streaming-primitives-and-wrappers/31-01-SUMMARY.md` — Rust streaming primitives shipped (chunk_encoder, AdvanceStream, advance_streaming).
- `.planning/phases/31-streaming-primitives-and-wrappers/31-02-SUMMARY.md` — TS streaming wrappers shipped (decodeAdvanceChunkBuf, RustStreamError, advanceStreaming, addQueriesStreaming with companion fallback).
- `.planning/phases/31-streaming-primitives-and-wrappers/31-REVIEW.md` — code review findings; CR-01 (the bug 32-01 fixes), WR-02..WR-05 (advisory).

### Streaming design (primary source of truth)

- `.planning/IVM-STREAMING-PLAN.md §7 Phase C` — original migration design. Phase 32 implements this section.
- `.planning/IVM-STREAMING-PLAN.md §8` — "Things that intentionally don't change" — preserve all listed semantics.

### Existing view-syncer surface (must preserve)

- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:481` — `#advancePipelines` callsite (in advance loop)
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:503` — `#hydrateUnchangedQueries` callsite
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:1321` — `#hydrateUnchangedQueries` def
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:1898` — `addQueriesAsync` call inside `#hydrateUnchangedQueries`
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:2067` — `#processChanges` def (the `Iterable<RowChange | 'yield'>` consumer that becomes `AsyncIterable<RowChange | 'yield'>`)
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:2162` — `#advancePipelines` def
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:2175` — `advanceAsync` call inside `#advancePipelines`
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:2218` — pokeEnd span (must continue to fire exactly once after CVR commit)

### Project conventions

- `./CLAUDE.md` — ESM, kebab-case, oxlint/oxfmt, Disposable pattern, LogContext for structured logging
- `./AGENTS.md` — TS optional fields rule, no `mod.ts` imports, no `import()` in type expressions
- `.planning/PROJECT.md` — multi-core parallelism is the core value; streaming exposes it client-visibly via mid-batch poke

### Phase 30 carry-forward

- AUDIT-02 invariant (parent_field stable in Edit reaching Exists operators) — relevant for streaming Edit chunks the consumer receives. The Phase 30-05 build-freshness gate (`assertNapiBinaryFreshness`) covers Phase 32 automatically.

### Test patterns

- `packages/zero-cache/src/services/view-syncer/view-syncer.test.ts` — primary test surface. Pattern reference for the new mid-batch + reset tests.
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts` — Phase 31's streaming test pattern (timer mocking, async iterator polling).

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- **`pokers` interface** in `view-syncer.ts` already supports `pokePart` / `cancel` / `end`. Streaming migration changes only WHEN these fire, not the interface contract.
- **`startAsyncSpan(tracer, ...)` wrapping pattern** at lines 2074, 2166, 2218 — preserve, just change the body inside the span.
- **`#convertDispatchChanges`** (or the buffered equivalent that converts napi RowChange → internal RowChange) — the streaming wrapper in pipeline-driver.ts already calls this; view-syncer doesn't need to.
- **`ResetPipelinesSignal` class** (existing) — re-thrown from streaming path with same shape.
- **`describe.each([true, false])` vitest pattern** — standard way to dual-mode existing tests under the feature flag.

### Established Patterns

- **Async generator + `for await of` pattern** — view-syncer already uses this elsewhere; `#processChanges` shifting from `Iterable` to `AsyncIterable` is mechanical.
- **`lc.error(message, structuredFields)` for structured logs** — established LogContext pattern.
- **Env var feature flags** — there are existing examples in zero-cache (e.g., debug log paths, tunable timeouts). Follow the local idiom.
- **Mock pipelines for time-based tests** — `pipeline-driver.streaming.test.ts` already mocks pipelines with deterministic timers; reuse the helper.

### Integration Points

- `view-syncer.ts` is the ONLY consumer being migrated.
- Tests: `view-syncer.test.ts` and any related test files in the same dir need dual-mode wrapping.
- No changes to `pipeline-driver.ts`, `decode-advance-buf.ts` (except the CR-01 fix in 32-01), Rust crates, schema, protocol, CVR, or replicator.

</code_context>

<specifics>
## Specific Ideas

- Feature flag env var: `ZQLITE_RS_USE_STREAMING_CONSUMER`. Default `true`. Documented in code comment with reference to D-04..D-07.
- The CR-01 fix gates on `hi` magnitude — the precise threshold value needs to map cleanly to `MAX_SAFE_INTEGER` (`2^53 - 1`). For positive numbers: if `hi >= 0x200000` (≥ 2^21), the result will exceed `MAX_SAFE_INTEGER` (since `2^21 * 2^32 = 2^53`). For negatives (two's complement), mirror the check.
- Mid-batch test asserts FIRST `pokePart` fires BEFORE slowest pipeline completes — that's the falsifiable claim, not just "pokePart fires more than once."
- Reset test: companion-scalar change injection via the existing test mechanism (whatever `ResetPipelinesSignal` reset tests in view-syncer.test.ts use today).

</specifics>

<deferred>
## Deferred Ideas

- **Remove `ZQLITE_RS_USE_STREAMING_CONSUMER` feature flag and the buffered fallback in view-syncer** → follow-up phase (32.1 or milestone wrap-up) after one production cycle validates the streaming path.
- **Real-world workload fixture for performance characterization** → Phase 33 PERF-01.
- **Memory footprint benchmark** (`O(total_changes)` → `O(max_pipeline_changes)`) → Phase 33 PERF-02.
- **Time-to-first-byte microbenchmark** → Phase 33 PERF-01.
- **Removing `advanceAsync` / `addQueriesAsync` / `decodeAdvanceResultBuf` buffered methods** → NOT in this milestone per IVM-STREAMING-PLAN.md §8.
- **WR-02 / WR-03 / WR-04 / WR-05 from 31-REVIEW.md** → assessed by Phase 31 verifier as non-blocking; revisit if production exposure surfaces actual bugs.

### Reviewed Todos (not folded)

None — no pending todos relevant to this phase.

</deferred>

---

_Phase: 32-view-syncer-migration_
_Context gathered: 2026-04-29_
