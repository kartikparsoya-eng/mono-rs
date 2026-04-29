# Phase 32: View-Syncer Streaming Migration - Research

**Researched:** 2026-04-29
**Domain:** TypeScript view-syncer migration to async streaming consumer
**Confidence:** HIGH

## Summary

The migration is mechanically small but touches two integration boundaries:
(1) `#processChanges` flips from `Iterable` to `AsyncIterable` consumption (a single-line `for of` → `for await of` change); (2) `#advancePipelines` and `#hydrateUnchangedQueries` swap `advanceAsync`/`addQueriesAsync` for the new streaming variants. The streaming wrappers (Phase 31) are already async and already do the bulk of the work — error throw shape (`ResetPipelinesSignal`, `RustStreamError`), `try/finally { stream.return() }` cancel propagation, and `case 'reset'` dispatch are all Phase 31's responsibility. View-syncer just consumes them.

The two surprises uncovered by this research:

1. **`#processChanges` is called from TWO sites**, not one (line 1929 hydration + line 2198 advance). Both must accept `AsyncIterable`. The hydration callsite already awaits `addQueriesAsync` (returns `Promise<Iterable>`), so flipping to `addQueriesStreaming` (returns `Promise<AsyncIterable>`) requires changing both the hydration callsite AND the `#processChanges` signature once.
2. **The view-syncer test suite has NO direct `pokers.pokePart` mock** — all PG integration tests observe pokes via `Queue<Downstream>` and `nextPokeParts(client)`. The MIGRATE-03 mid-batch test must either (a) instrument the queue with timestamp recording, or (b) introduce an explicit unit-style test with a mocked `PokeHandler`. Option (b) requires inventing a new test pattern that doesn't currently exist in this directory.

**Primary recommendation:** Make the `#processChanges` signature change atomically with both callsite migrations. Use the `Queue<Downstream>` instrumentation approach for MIGRATE-03 (record timestamps as poke messages enqueue) — keeps the test in the existing PG integration style.

## Architectural Responsibility Map

| Capability                               | Primary Tier                          | Secondary Tier | Rationale                                                 |
| ---------------------------------------- | ------------------------------------- | -------------- | --------------------------------------------------------- |
| Per-pipeline parallelism + cancel        | Rust IVM (Phase 31)                   | —              | Already shipped; view-syncer is just consumer             |
| Chunk decode + error/reset throw mapping | TS PipelineDriver (Phase 31)          | —              | `#streamChanges` async generator already does this        |
| `pokers.pokePart` cadence                | TS view-syncer (Phase 32)             | —              | This is what we're changing — pokes fire as chunks arrive |
| CVR commit atomicity                     | TS view-syncer (existing)             | —              | Single commit at end of stream — UNCHANGED                |
| Feature flag read                        | TS view-syncer module init (Phase 32) | —              | Boot-time read, per D-04..D-07                            |

## Standard Stack

### Core (already in use, no additions)

| Library            | Version     | Purpose                        | Why Standard                                               |
| ------------------ | ----------- | ------------------------------ | ---------------------------------------------------------- |
| `vitest`           | 4.1.3       | Test framework                 | Repo-wide standard [VERIFIED: package.json]                |
| `fast-check`       | ^3.18.0     | Property-based testing         | Used by parity fuzz [VERIFIED: 31-02 SUMMARY]              |
| `@rocicorp/logger` | (workspace) | LogContext for structured logs | Used everywhere in zero-cache [VERIFIED: existing imports] |

### Already-Built Streaming Wrappers (consumed by Phase 32)

| API                                                  | Returns                                                                        | Defined In              | Notes                                                                     |
| ---------------------------------------------------- | ------------------------------------------------------------------------------ | ----------------------- | ------------------------------------------------------------------------- |
| `pipelineDriver.advanceStreaming(timer, vsId?)`      | `Promise<{version, numChanges, changes: AsyncIterable<RowChange \| 'yield'>}>` | pipeline-driver.ts:1851 | Mirrors `advanceAsync` shape                                              |
| `pipelineDriver.addQueriesStreaming(queries, timer)` | `Promise<AsyncIterable<RowChange \| 'yield'>>`                                 | pipeline-driver.ts:1113 | Mirrors `addQueriesAsync` shape                                           |
| `RustStreamError`                                    | exported class                                                                 | pipeline-driver.ts:133  | `kind: 'panic'\|'rayon_error'\|'channel_closed'`, `source: 'rust-stream'` |
| `ResetPipelinesSignal`                               | existing class                                                                 | snapshotter.ts          | Streaming throws with reason `'scalar-subquery'`, same as today           |

**Installation:** None needed — all consumers already in `pipeline-driver.ts`.

## User Constraints (from CONTEXT.md)

### Locked Decisions

(All 24 decisions D-01..D-24 are in scope — see 32-CONTEXT.md. Highlights:)

- **D-01:** Two plans, sequential. 32-01 = CR-01 fix (~3 tasks). 32-02 = full migration (~6-7 tasks).
- **D-04..D-07:** `ZQLITE_RS_USE_STREAMING_CONSUMER` env var, read once at boot. Default `true`. Feature-flag removal is a follow-up phase.
- **D-08..D-11:** `RustStreamError` kind in structured log payload. All three kinds bubble + crash the connection (no per-kind recovery in view-syncer). `ResetPipelinesSignal` semantics unchanged from today.
- **D-12..D-14:** CR-01 fix gates on `hi` magnitude before lossy `n = hi * 0x100000000 + lo` construction. Threshold `>= 0x200000` for positive (mirrored for negative). Regression tests pin `MAX_SAFE_INTEGER ± 1`, `i64::MAX`, `i64::MIN`.
- **D-15..D-17:** MIGRATE-03 test = synthetic mock pipelines on different timers, assert FIRST pokePart fires BEFORE slowest pipeline completes. Test runs only when `ZQLITE_RS_USE_STREAMING_CONSUMER=true`.
- **D-18..D-19:** MIGRATE-04 reset test asserts `pokers.cancel()` × 1, `pokers.end()` × 0, no CVR commit, client-side rolled back.
- **D-20..D-22:** File modifications: `decode-advance-buf.{ts,test.ts}` (32-01); `view-syncer.ts` + existing tests dual-mode + 2 new test files (32-02). Do NOT touch `pipeline-driver.ts` or Rust files.
- **D-23..D-24:** Verifier runs vitest twice — under `ZQLITE_RS_USE_STREAMING_CONSUMER=true` AND `=false`. `assertNapiBinaryFreshness` covers Phase 32 automatically.

### Claude's Discretion (per CONTEXT.md)

- Specific instrumentation mechanism for the mid-batch pokePart test (timestamp array, mock spy, async barrier).
- Exact threshold value for `hi` magnitude check in CR-01 fix (`>= 0x200000` vs `>= 0x1FFFFF`).
- How to wrap existing view-syncer tests in `describe.each([true, false])` (single shared wrapper module vs per-test adoption).
- Exact env var read site (boot vs per-instance).

### Deferred Ideas (OUT OF SCOPE)

- Removing `ZQLITE_RS_USE_STREAMING_CONSUMER` flag and buffered fallback → follow-up phase 32.1.
- Performance microbenchmarks (TTFB, memory) → Phase 33.
- Removing `advanceAsync` / `addQueriesAsync` / `decodeAdvanceResultBuf` → NOT in this milestone.
- WR-02..WR-05 from 31-REVIEW.md → assessed non-blocking; revisit if production issues surface.

## Phase Requirements

| ID         | Description                                                                     | Research Support                                                                                                                     |
| ---------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| MIGRATE-01 | `#advancePipelines`→`advanceStreaming`; `#processChanges` accepts AsyncIterable | view-syncer.ts:2175 + 2198 — single-callsite swap; `for of` → `for await of` at line 2103                                            |
| MIGRATE-02 | `#hydrateUnchangedQueries`→`addQueriesStreaming`                                | view-syncer.ts:1898 — second `#processChanges` consumer at line 1929 must also accept AsyncIterable                                  |
| MIGRATE-03 | `pokers.pokePart` fires mid-batch                                               | New test file, mock pipeline-driver to emit chunks on staggered timers; record poke timestamps via Queue<Downstream> instrumentation |
| MIGRATE-04 | `pokers.cancel()` on `ResetPipelinesSignal` mid-stream                          | view-syncer.ts:2206-2209 catch block already does this — verify the streaming throw path triggers it correctly                       |

---

## 1. Existing Code Reference Points

### `view-syncer.ts` callsites

| Line      | What                                                                                                                                         | Migration impact                                                                                                                                                                  |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 481       | `await this.#advancePipelines(lc, cvr)` — outer caller in `#runInLockWithCVR`                                                                | UNCHANGED — `#advancePipelines` returns same `Promise<'success' \| ResetPipelinesSignal>`                                                                                         |
| 503       | `await this.#hydrateUnchangedQueries(lc, cvr)` — outer caller                                                                                | UNCHANGED                                                                                                                                                                         |
| 1321      | `async #hydrateUnchangedQueries(lc, cvr)` def                                                                                                | Body unchanged except line 1898                                                                                                                                                   |
| 1898      | `const rowChanges = await pipelines.addQueriesAsync(...)`                                                                                    | Change to `addQueriesStreaming(...)`. Return type flips from `Iterable` to `AsyncIterable`. Local `rowChanges` is just passed to `#processChanges` at line 1929 — no other reads. |
| 1929      | `await this.#processChanges(lc, timer, rowChanges, updater, pokers)`                                                                         | UNCHANGED at callsite — but signature of `rowChanges` flips to `AsyncIterable`                                                                                                    |
| 2067      | `#processChanges(lc, timer, changes: Iterable<RowChange \| 'yield'>, updater, pokers)` def                                                   | Change parameter type to `AsyncIterable<RowChange \| 'yield'>`                                                                                                                    |
| 2103      | `for (const change of changes)` inside `loopingChanges` span                                                                                 | Change to `for await (const change of changes)`                                                                                                                                   |
| 2162      | `#advancePipelines(lc, cvr): Promise<'success' \| ResetPipelinesSignal>` def                                                                 | Body changes inside                                                                                                                                                               |
| 2174      | `const timer = new TimeSliceTimer(lc)`                                                                                                       | UNCHANGED                                                                                                                                                                         |
| 2175      | `const {version, numChanges, changes} = await this.#pipelines.advanceAsync(timer, this.id)`                                                  | Change to `await this.#pipelines.advanceStreaming(timer, this.id)`. Return shape is identical except `changes` is `AsyncIterable` not `Iterable`.                                 |
| 2197-2211 | `try { await this.#processChanges(...) } catch (e) { if (e instanceof ResetPipelinesSignal) { await pokers.cancel(); return e; } throw e; }` | EXTEND catch with `RustStreamError` branch per D-08. Logging via `lc.error('rust streaming error', { kind, message, source })`. Then `throw err`.                                 |
| 2218      | `pokers.end(finalVersion)` inside `vs.#advancePipelines.pokeEnd` span                                                                        | UNCHANGED — fires once after CVR commit                                                                                                                                           |

### `pipeline-driver.ts` — Phase 31 streaming surface (consumed, NOT modified)

| Line | What                                                                                                 | Notes                                                                                                |
| ---- | ---------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| 133  | `export class RustStreamError`                                                                       | `extends Error`; `source = 'rust-stream'`; `kind` field                                              |
| 1113 | `async addQueriesStreaming(queries, timer): Promise<AsyncIterable<RowChange \| 'yield'>>`            | Phase 3a (TS-hydrate first) → 3b (Rust streaming) ordering                                           |
| 1276 | `async *#streamAddQueries(prepared, timer)`                                                          | The async generator returned by `addQueriesStreaming`                                                |
| 1851 | `async advanceStreaming(timer, _vsId?): Promise<{version, numChanges, changes: AsyncIterable<...>}>` | Same shape as `advanceAsync` modulo `Iterable` → `AsyncIterable`                                     |
| 1926 | `async *#streamChanges(stream, timer, numChanges)`                                                   | Decodes chunks; throws `ResetPipelinesSignal` / `RustStreamError`; `try/finally { stream.return() }` |

### `decode-advance-buf.ts` — CR-01 target (Plan 32-01 only)

| Line    | What                                                                                                                                                                         |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 257-309 | `function readJsonValue(buf, view, offset)`                                                                                                                                  |
| 266-275 | `case 1` — i64 decoder. **THIS IS CR-01.**                                                                                                                                   |
| 270     | `const n = hi * 0x100000000 + lo` — lossy when `\|hi\| >= 2^21`                                                                                                              |
| 272-273 | `if (n >= MAX_SAFE_INTEGER \|\| n <= MIN_SAFE_INTEGER) { return [BigInt(hi) * BigInt(0x100000000) + BigInt(lo >>> 0), offset]; }` — comparison-after-loss, defeats the guard |

### Existing test files this phase modifies

| File                                          | Reason                                                                                                                                                                                                                 |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `view-syncer.pg.test.ts` (3000+ lines)        | Wrap streaming-eligible tests in `describe.each([{useStreaming: true}, {useStreaming: false}])`. The file already uses `describe.each` at line 3084 for a different parameterization (failed query re-transformation). |
| `view-syncer-permissions.pg.test.ts`          | Same wrapping if it exercises advance/hydrate paths                                                                                                                                                                    |
| `view-syncer-ttl.pg.test.ts`                  | Same wrapping if it exercises advance/hydrate                                                                                                                                                                          |
| `view-syncer-auth-maintenance.pg.test.ts`     | Same wrapping                                                                                                                                                                                                          |
| `view-syncer.yield-during-advance.pg.test.ts` | Same wrapping                                                                                                                                                                                                          |
| `decode-advance-buf.test.ts`                  | 32-01: add CR-01 boundary regression tests                                                                                                                                                                             |

---

## 2. Library / Pattern Specifics

### Env var idiom (D-04 read site)

The canonical pattern in this codebase is **read at module init**, not per-instance:

```typescript
// pipeline-driver.ts:165-166 (assertNapiBinaryFreshness gate)
if (process.env['NODE_ENV'] === 'production') return;
if (process.env['ZQLITE_RS_SKIP_FRESHNESS_CHECK'] === '1') return;
```

The `ZERO_DISABLE_RUST_IVM` env var is read in 6 different test files via `process.env.ZERO_DISABLE_RUST_IVM` and toggled with `process.env.ZERO_DISABLE_RUST_IVM = '1'` / `delete process.env.ZERO_DISABLE_RUST_IVM` for test setup [VERIFIED: grep across packages/zero-cache/src/services/view-syncer/]. **Note:** Despite the test toggling, no production code currently reads `ZERO_DISABLE_RUST_IVM` — the env var name is reserved but unused in pipeline-driver.ts (manager is gated by `RustPipelineManagerClass` import success only).

**Recommended pattern for `ZQLITE_RS_USE_STREAMING_CONSUMER`:** Read once at view-syncer **module top-level** (matches `assertNapiBinaryFreshness` pattern), expose as a `const`:

```typescript
// view-syncer.ts top of file
const USE_STREAMING_CONSUMER =
  process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'] !== 'false';
// Default true per D-04. Set to 'false' for emergency rollback.
// Removed in follow-up phase 32.1 per D-07.
```

Use `!== 'false'` (string comparison, default-true) rather than `=== 'true'` so unset/undefined yields `true` per D-04. Bracket-access `process.env['X']` is preferred for `noUncheckedIndexedAccess`-safe TS [VERIFIED: pattern in pipeline-driver.ts:165-166].

### `describe.each([true, false])` precedent

**EXISTS in this codebase.** [VERIFIED: grep]

- `view-syncer.pg.test.ts:3084` uses `describe.each([{name, hash1, hash2, ast1, ast2, ...}])(...)` for parameterized scenarios.
- `process-mutations.test.ts:1013` uses `describe.each(mutatorInvokers)(...)` for invoker variants.

For Phase 32, use this shape for dual-mode wrapping:

```typescript
describe.each([
  {useStreaming: true, label: 'streaming'},
  {useStreaming: false, label: 'buffered'},
])('view-syncer ($label)', ({useStreaming, label}) => {
  // ... wrap process.env around setup/teardown
  beforeEach(() => {
    process.env.ZQLITE_RS_USE_STREAMING_CONSUMER = useStreaming
      ? 'true'
      : 'false';
  });
  afterEach(() => {
    delete process.env.ZQLITE_RS_USE_STREAMING_CONSUMER;
  });
  // ...existing tests
});
```

**Caveat:** Because the env var is read at MODULE INIT, toggling it in `beforeEach` won't affect the already-imported view-syncer module. Two options:

1. **(Recommended)** Read the env var inside `ViewSyncerService` constructor — converts module-level constant to per-instance field. Slightly violates "read once at boot" simplicity, but tests can flip it without re-importing.
2. Use `vi.resetModules()` + dynamic re-import in `beforeEach`. More invasive — likely breaks the existing PG integration tests' shared `setup()` helper.

**Recommend Option 1** — read `process.env` in the `ViewSyncerService` constructor, store as a private `#useStreamingConsumer: boolean` field. "Read once at boot" still holds at the granularity of a view-syncer instance (one per client group).

### Pokers mock pattern

**There is NO existing direct mock of `PokeHandler.startPoke / pokePart / end / cancel`.** [VERIFIED: grep across view-syncer.* test files]

Existing PG integration tests observe pokes by reading from a `Queue<Downstream>` populated by the real client connection stream:

```typescript
// view-syncer-test-util.ts:469
export async function nextPokeParts(
  client: Queue<Downstream>,
): Promise<PokePartBody[]> {
  const pokes = await nextPoke(client);
  return pokes
    .filter((msg: Downstream) => msg[0] === 'pokePart')
    .map(([, body]) => body);
}
```

`nextPoke` blocks until it sees a `'pokeEnd'` message. The Downstream stream IS the observable — there is no intermediate `pokers` mock layer.

**For MIGRATE-03 (mid-batch timestamp recording):** Wrap the existing `Queue<Downstream>` with a recording shim:

```typescript
// In the new test file
const pokeTimestamps: Array<{type: string; t: number; pokeID?: string}> = [];

void (async function recordPokes() {
  for await (const msg of source) {
    if (msg[0] === 'pokePart') {
      pokeTimestamps.push({
        type: 'pokePart',
        t: performance.now(),
        pokeID: msg[1].pokeID,
      });
    } else if (msg[0] === 'pokeEnd') {
      pokeTimestamps.push({
        type: 'pokeEnd',
        t: performance.now(),
        pokeID: msg[1].pokeID,
      });
    } else if (msg[0] === 'pokeStart') {
      pokeTimestamps.push({
        type: 'pokeStart',
        t: performance.now(),
        pokeID: msg[1].pokeID,
      });
    }
    queue.enqueue(msg);
  }
})();
```

Adapt `connectWithQueueAndSource` (view-syncer-test-util.ts:834) — its existing for-await loop is the natural injection point. Either fork the helper for the new test, or extend it with an optional `onMessage` callback.

### Mock pipelines with timers

**The Phase 31 streaming test does NOT mock pipelines on different timers.** It uses real pipelines on a real DB and asserts parity. [VERIFIED: pipeline-driver.streaming.test.ts:168-211]

**For MIGRATE-03, you must build new infrastructure.** The cleanest approach:

```typescript
// Spy on the real PipelineDriver.advanceStreaming.
// Wrap the returned AsyncIterable to inject staggered awaits between
// chunk groups so each "pipeline" appears to complete on a different timer.
//
// Async generator with explicit setTimeout sleeps between yielded chunks
// gives deterministic, reproducible mid-batch fan-out timing.

import {sleep} from '../../../../shared/src/sleep.ts'; // confirm helper exists

const realAdvanceStreaming = pipelines.advanceStreaming.bind(pipelines);
vi.spyOn(pipelines, 'advanceStreaming').mockImplementation(async timer => {
  const real = await realAdvanceStreaming(timer);
  return {
    version: real.version,
    numChanges: real.numChanges,
    changes: (async function* staggered() {
      const buckets: RowChange[][] = [[], [], []]; // 3 fake "pipelines"
      for await (const c of real.changes) {
        if (c === 'yield') continue;
        buckets[parseInt(c.queryID.slice(-1)) % 3].push(c);
      }
      const delays = [10, 50, 200];
      for (let i = 0; i < buckets.length; i++) {
        await sleep(delays[i]);
        for (const c of buckets[i]) yield c;
      }
    })(),
  };
});
```

This pattern is **new** to this directory but follows established `vi.spyOn(...).mockImplementation` conventions used elsewhere in zero-cache tests [VERIFIED: vi.spyOn in view-syncer.pg.test.ts:3108].

**Alternative (simpler) approach:** Mock `RustPipelineManager.advanceStreaming` directly via Proxy on its prototype — same pattern Phase 31 used for TEST-05 [VERIFIED: pipeline-driver.streaming.test.ts:265-299]. Inject staggered `setTimeout` between calls to the real `next()` method.

---

## 3. Pitfalls & Gotchas

### P-01: `#processChanges` is called from TWO sites, not one

**The trap:** Reading only the `#advancePipelines` flow (line 2198) and forgetting `#syncQueryPipelineSet` also calls `#processChanges` at line 1929.

**Why it matters:** When you flip `#processChanges` from `Iterable` to `AsyncIterable`, BOTH callers' producers must change. If you migrate `addQueriesAsync` → `addQueriesStreaming` at line 1898 but forget the line 2175 (`advanceAsync` → `advanceStreaming`), or vice versa, TypeScript will catch one but the production behavior of the un-migrated path will silently regress (still runs through the buffered code path that materializes everything before yielding).

**Prevention:** Migrate both callsites + the signature change atomically in a single task. Verify by grepping `\.#processChanges(` returns exactly 2 callsites both passing AsyncIterable producers.

### P-02: `#processChanges` body is already async — no internal sync assumptions

**Inspected:** Lines 2074-2151. The body is `startAsyncSpan(tracer, 'vs.#processChanges', async () => { ... })`. Inside, `processBatch` is async, every `pokers.addPatch` is awaited, the `for (const change of changes)` loop body uses `await timer.yieldProcess(...)` and `await processBatch()`. **There are NO sync-iteration assumptions** — the only line change is `for (const change of changes)` → `for await (const change of changes)` at line 2103.

**Confidence:** HIGH — the body is structurally async-friendly today.

### P-03: Env var read timing — module-init vs per-instance

If you read `process.env.ZQLITE_RS_USE_STREAMING_CONSUMER` at module top-level (matching `assertNapiBinaryFreshness`), test code that toggles `process.env` in `beforeEach` will have NO effect on the already-imported module. The first test that runs locks in the value forever for that vitest worker.

**Resolution:** Read the env var in the `ViewSyncerService` constructor, store as `#useStreamingConsumer: boolean`. Each test's `setup()` creates a fresh `ViewSyncerService`, so the env value is captured at the right time. (See "Env var idiom" above.)

### P-04: `ResetPipelinesSignal` interaction with `try/finally { stream.return_() }`

**The risk:** When `#streamChanges` (pipeline-driver.ts:1926) throws `ResetPipelinesSignal` from inside the `for await` loop, control jumps to the `finally` block which calls `stream.return()`. The Rust side flips the cancel flag and drops the stream. Then the throw propagates up to view-syncer's `#processChanges` `for await`, which propagates to `#advancePipelines`'s `try/catch` at line 2206-2209, where `pokers.cancel()` fires.

**Verified by:** pipeline-driver.ts:1982-1991 — the `finally` block wraps `stream.return()` in try/catch and swallows errors ("Drop handles the GC path"). The throw from line 1945 propagates after `finally` runs. Standard JS semantics.

**Watch for:** If `pokers.cancel()` were ever to throw or hang, the chain breaks. Verify in MIGRATE-04 test that `pokers.cancel()` is called BEFORE the test asserts state (not just "eventually").

### P-05: `addQueriesStreaming` calls `addQuery` synchronously in Phase 2 (WR-03)

**Pre-existing risk** flagged in 31-REVIEW.md WR-03: `addQueriesStreaming` registers Rust queries on the manager (line 1257-1262) BEFORE the async generator returned at line 1264 starts draining. If `#hydrateUnchangedQueries` throws between awaiting `addQueriesStreaming` and starting `for await`, the Rust manager has registered queries with no consumer.

**Phase 32 mitigation:** Don't introduce new throw points between `await pipelines.addQueriesStreaming(...)` and the `for await` consumption. The pattern at view-syncer.ts:1898 already passes the result directly to `#processChanges` — preserve that immediacy.

### P-06: CR-01 — comparison-after-loss

**The trap:** `n = hi * 0x100000000 + lo` produces a JS Number. For `|hi| >= 2^21`, the multiplication exceeds f64 53-bit precision. Subsequent `n >= MAX_SAFE_INTEGER` operates on an already-rounded value. The guard fails to fire when it should.

**Concrete failure example:** `i64 = 9007199254740993` (= 2^53 + 1):

- `lo = 0x00000001`, `hi = 0x00200000` (2^21)
- `hi * 0x100000000 = 9007199254740992` (= 2^53, the f64 round-down)
- `+ 1 = 9007199254740993`? Actually `9007199254740992 + 1 = 9007199254740992` in f64 (1 ULP at 2^53 is 2). So `n = 9007199254740992`.
- `n >= MAX_SAFE_INTEGER (= 2^53 - 1)` is true → BigInt branch fires.
- BigInt branch returns `BigInt(0x00200000) * BigInt(0x100000000) + BigInt(0x00000001) = 9007199254740993n` (correct).

This case actually works. The genuine breakage is for `i64 = 2^53` itself (`MAX_SAFE_INTEGER + 1`):

- `lo = 0x00000000`, `hi = 0x00200000`
- `n = 9007199254740992`
- `n >= MAX_SAFE_INTEGER (= 9007199254740991)` is true → BigInt branch fires → returns `9007199254740992n`. Correct.

The genuine breakage is at `i64::MAX = 0x7FFFFFFFFFFFFFFF`:

- `lo = 0xFFFFFFFF`, `hi = 0x7FFFFFFF`
- `hi * 0x100000000 = 0x7FFFFFFF00000000` ≈ `9.223372032559808e18` (already lossy — last ~10 bits are zero in f64)
- `+ lo (0xFFFFFFFF = 4294967295)` ≈ `9.223372036854776e18` (further lossy)
- `n >= MAX_SAFE_INTEGER` is true → BigInt branch. Returns `BigInt(0x7FFFFFFF) * BigInt(0x100000000) + BigInt(0xFFFFFFFF) = 9223372036854775807n` (correct i64::MAX as BigInt).

**Wait — the BigInt branch returns the right answer.** Where is the bug?

Re-reading 31-REVIEW.md CR-01 carefully: the bug is the THRESHOLD CHECK happens after lossy arithmetic. For values like `n = 2^53` exactly, the comparison `n >= MAX_SAFE_INTEGER` correctly evaluates true (since `2^53 > 2^53 - 1`). For `n = 2^53 - 1` (`MAX_SAFE_INTEGER`), comparison is `n >= MAX_SAFE_INTEGER` which is true (with `>=`) — taking BigInt branch when Number is correct. **The BigInt branch math `BigInt(hi) * 0x100000000n + BigInt(lo>>>0)` always produces the correct two's-complement i64.** So functionally, the BigInt fallback is fine even when the Number branch had precision loss.

The 31-REVIEW.md Critical concern is more subtle: the `>=` strict-greater-or-equal at MAX_SAFE_INTEGER causes type inconsistency (Number for `n = MAX_SAFE_INTEGER - 1`, BigInt for `n = MAX_SAFE_INTEGER`). Downstream code doing `if (typeof v === 'number')` may mis-route exactly at the boundary. **The CONTEXT.md fix (D-12..D-14) is the correct mitigation regardless** — gate on `hi` magnitude BEFORE the lossy Number construction:

```typescript
case 1: {
  const lo = view.getUint32(offset, true);
  const hi = view.getInt32(offset + 4, true);
  offset += 8;
  // Gate on hi magnitude BEFORE Number arithmetic (CR-01 fix per D-12..D-13).
  // 2^53 = 2^21 * 2^32, so |hi| >= 2^21 (= 0x200000) means the result
  // exceeds MAX_SAFE_INTEGER and Number arithmetic is lossy.
  if (hi >= 0x200000 || hi < -0x200000) {
    return [BigInt(hi) * 0x100000000n + BigInt(lo), offset];
  }
  return [hi * 0x100000000 + lo, offset];
}
```

**Note:** `BigInt(lo)` is sufficient since `lo` was read via `getUint32` — already a non-negative JS Number, `BigInt(lo)` does NOT need `lo >>> 0`. The original code used `BigInt(lo >>> 0)` defensively but it's redundant.

### P-07: `hi >= 0x200000` boundary precision

`MAX_SAFE_INTEGER = 2^53 - 1 = 9007199254740991`. The smallest positive i64 that exceeds this is `2^53 = 9007199254740992`, which encodes as `lo = 0, hi = 0x00200000`. So `hi == 0x200000 && lo == 0` IS the first unsafe value. With `hi >= 0x200000`, ANY value with `hi == 0x200000` triggers BigInt — including `lo = 0` which would actually be `2^53` (representable as f64 but not as MAX_SAFE_INTEGER). This is conservative but correct. CONTEXT.md D-13 calls out the alternative `>= 0x1FFFFF` as "safer-by-one" — both work; `>= 0x200000` is the cleaner power-of-two boundary.

For negatives: the smallest representable safe negative is `MIN_SAFE_INTEGER = -(2^53 - 1)`. The two's-complement encoding of `-(2^53)` is `lo = 0, hi = 0xFFE00000` (signed Int32 = `-0x200000`). So mirror the check: `hi < -0x200000` triggers BigInt.

**Edge cases for tests (per D-14):**

- `MAX_SAFE_INTEGER = 9007199254740991` → `lo=0xFFFFFFFF, hi=0x001FFFFF` → safe Number branch
- `MAX_SAFE_INTEGER + 1 = 9007199254740992` → `lo=0, hi=0x00200000` → BigInt branch (currently lossy in Number branch but BigInt path was correct anyway; with fix, BigInt fires before lossy arithmetic)
- `i64::MAX = 0x7FFFFFFFFFFFFFFF` → `lo=0xFFFFFFFF, hi=0x7FFFFFFF` → BigInt branch
- `i64::MIN = 0x8000000000000000 = -9223372036854775808` → `lo=0, hi=0x80000000` (signed = `-2147483648`) → `hi < -0x200000` triggers BigInt
- `MIN_SAFE_INTEGER = -9007199254740991` → `lo=1, hi=0xFFE00001` (signed = `-2097151`) → `-2097151 >= -2097152` (= `-0x200000`)? Yes, so safe Number branch.
- `MIN_SAFE_INTEGER - 1 = -9007199254740992` → `lo=0, hi=0xFFE00000` (signed = `-2097152` = `-0x200000`) → `hi < -0x200000` is FALSE (strict less-than). Borderline — would fall to Number branch which gives `-2097152 * 4294967296 + 0 = -9007199254740992` (representable in f64). With strict `<`, this is fine.

**Recommend the assertion in 32-01 use `<= -0x200000` instead of `< -0x200000`** to be symmetric with the positive check (`>= 0x200000`). Then `MIN_SAFE_INTEGER - 1` lands in BigInt branch — slightly more conservative but symmetric.

---

## 4. CR-01 Fix Details

**Current code** (decode-advance-buf.ts:266-275):

```typescript
case 1: {
  // i64
  const lo = view.getUint32(offset, true);
  const hi = view.getInt32(offset + 4, true);
  const n = hi * 0x100000000 + lo;
  offset += 8;
  if (n >= MAX_SAFE_INTEGER || n <= MIN_SAFE_INTEGER) {
    return [BigInt(hi) * BigInt(0x100000000) + BigInt(lo >>> 0), offset];
  }
  return [n, offset];
}
```

**Fix shape (per D-12..D-13):**

```typescript
case 1: {
  // i64. Gate on hi magnitude BEFORE Number arithmetic (CR-01 fix).
  // 2^53 = 2^21 * 2^32, so |hi| >= 2^21 (0x200000) means the value
  // exceeds MAX_SAFE_INTEGER and the JS Number arithmetic loses
  // precision. Construct BigInt directly in that case.
  const lo = view.getUint32(offset, true);
  const hi = view.getInt32(offset + 4, true);
  offset += 8;
  if (hi >= 0x200000 || hi <= -0x200000) {
    return [BigInt(hi) * 0x100000000n + BigInt(lo), offset];
  }
  return [hi * 0x100000000 + lo, offset];
}
```

**Notes:**

- `BigInt(lo)` is sufficient (no `>>> 0` needed — `getUint32` already returns non-negative).
- Use BigInt literal `0x100000000n` instead of `BigInt(0x100000000)` for clarity (existing pattern uses the latter; either works).
- Keep `MAX_SAFE_INTEGER` / `MIN_SAFE_INTEGER` imports — they're still used elsewhere in `decode-advance-buf.ts` (verify with grep before removing).

**Regression test additions** (per D-14, in `decode-advance-buf.test.ts`):

Test inputs to feed via the existing chunk-byte hand-rolling pattern (lines 174-237 of the existing test file). Build a single-row chunk where the column value is an `i64` (tag=1) with the boundary value, then assert `decodeAdvanceChunkBuf(buf)[0].row.value === expected`.

Add tests for:

1. `MAX_SAFE_INTEGER (9007199254740991)` → returns `9007199254740991` as Number
2. `MAX_SAFE_INTEGER + 1 (9007199254740992)` → returns `9007199254740992n` as BigInt
3. `i64::MAX` (`hi=0x7FFFFFFF, lo=0xFFFFFFFF`) → returns `9223372036854775807n` as BigInt
4. `i64::MIN` (`hi=0x80000000, lo=0`) → returns `-9223372036854775808n` as BigInt
5. `MIN_SAFE_INTEGER (-9007199254740991)` → returns `-9007199254740991` as Number
6. `MIN_SAFE_INTEGER - 1 (-9007199254740992)` → returns `-9007199254740992n` as BigInt (symmetric with case 2)

Both `decodeAdvanceResultBuf` AND `decodeAdvanceChunkBuf` share the same `readJsonValue` helper, so a single fix benefits both decoders. Add at minimum one test in each existing `describe(...)` block to confirm parity.

---

## 5. Test Patterns

### Pokers timestamp recording (MIGRATE-03)

**No existing helper.** New pattern needed. Recommend extending `connectWithQueueAndSource` in `view-syncer-test-util.ts`:

```typescript
// Extend with optional onMessage callback
function connectWithQueueAndSource(
  ctx: SyncContext,
  desiredQueriesPatch: UpQueriesPatch,
  clientSchema?: ClientSchema | null,
  activeClients?: string[],
  onMessage?: (msg: Downstream, t: number) => void, // NEW
) {
  // ... existing setup ...
  void (async function () {
    try {
      for await (const msg of source) {
        onMessage?.(msg, performance.now()); // NEW: timestamp record
        queue.enqueue(msg);
      }
    } catch (e) {
      queue.enqueueRejection(e);
    }
  })();
  return {queue, source};
}
```

Test usage:

```typescript
const pokeTimestamps: Array<{type: string; t: number; pokeID?: string}> = [];
const {queue} = connectWithQueueAndSource(
  SYNC_CONTEXT,
  patches,
  null,
  undefined,
  (msg, t) => {
    if (msg[0] === 'pokeStart' || msg[0] === 'pokePart' || msg[0] === 'pokeEnd') {
      pokeTimestamps.push({type: msg[0], t, pokeID: msg[1].pokeID});
    }
  },
);

// ... trigger advance with mocked staggered pipelines ...

const slowestPipelineCompleteT = /* read from spy */;
const firstPokePart = pokeTimestamps.find(p => p.type === 'pokePart')!;
expect(firstPokePart.t).toBeLessThan(slowestPipelineCompleteT);
```

### Mock pipeline staggered timing helper

Reuse the **Proxy pattern from pipeline-driver.streaming.test.ts:265-299** (TEST-05). Same shape, but instead of recording return-call counts, inject sleeps between the real `next()` calls:

```typescript
const ManagerCls = (zqliteRs as any).RustPipelineManager as {
  prototype: {advanceStreaming: (id: string, changesJson: string) => unknown};
};
const original = ManagerCls.prototype.advanceStreaming;
const chunkDelaysMs = [10, 50, 200];
let nextChunkIdx = 0;

ManagerCls.prototype.advanceStreaming = function patched(id, changesJson) {
  const realStream = original.call(this, id, changesJson) as {
    next: () => Promise<{done: boolean; kind?: string; chunk?: Buffer}>;
    return: () => void;
  };
  return new Proxy(realStream, {
    get(target, prop) {
      if (prop === 'next') {
        return async () => {
          const item = await target.next();
          if (item.kind === 'chunk') {
            await new Promise(r =>
              setTimeout(r, chunkDelaysMs[nextChunkIdx % chunkDelaysMs.length]),
            );
            nextChunkIdx++;
          }
          return item;
        };
      }
      return Reflect.get(target, prop);
    },
  });
};
```

**Restore in `finally`:** `ManagerCls.prototype.advanceStreaming = original;`. **Critical** — leaking patched prototypes corrupts subsequent tests.

### Dual-mode wrapping example

```typescript
describe.each([
  {useStreaming: true, label: 'streaming'},
  {useStreaming: false, label: 'buffered'},
])('view-syncer ($label)', ({useStreaming, label}) => {
  beforeEach(() => {
    process.env.ZQLITE_RS_USE_STREAMING_CONSUMER = useStreaming
      ? 'true'
      : 'false';
  });
  afterEach(() => {
    delete process.env.ZQLITE_RS_USE_STREAMING_CONSUMER;
  });

  test.runIf(useStreaming)(
    'mid-batch pokePart fires before slowest pipeline (MIGRATE-03)',
    async () => {
      // ... only runs in streaming mode per D-17
    },
  );

  test('common test that runs in both modes', async () => {
    // ...
  });
});
```

**Critical:** This pattern only works if `ViewSyncerService` reads `process.env.ZQLITE_RS_USE_STREAMING_CONSUMER` in its constructor (not at module init). See P-03.

---

## 6. Validation Architecture (Nyquist)

> Nyquist validation: production signals continuously sampled to confirm the migration's runtime behavior matches its design intent.

### Test Framework

| Property                 | Value                                                                                                                          |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------ |
| Framework                | vitest 4.1.3 [VERIFIED: package.json]                                                                                          |
| Config files             | `vitest.config.ts` (default), `vitest.config.no-pg.ts` (for unit tests), `vitest.config.pg-16.ts`, `vitest.config.pg-17.ts`    |
| Quick run command (unit) | `cd packages/zero-cache && npx vitest --config vitest.config.no-pg.ts run src/services/view-syncer/decode-advance-buf.test.ts` |
| Quick run command (PG)   | `cd packages/zero-cache && npx vitest run src/services/view-syncer/view-syncer.pg.test.ts`                                     |
| Full PG suite            | `cd packages/zero-cache && npx vitest --config vitest.config.pg-16.ts run`                                                     |
| Full Rust suite          | `cd packages/zqlite-rs && cargo test --release --lib -- --test-threads=1` (per 31-01 IN-01)                                    |

### Phase Requirements → Test Map

| Req ID     | Behavior                                                                                      | Test Type                      | Automated Command                                                                                                | File Exists?                         |
| ---------- | --------------------------------------------------------------------------------------------- | ------------------------------ | ---------------------------------------------------------------------------------------------------------------- | ------------------------------------ |
| CR-01      | i64 boundary decode (`MAX_SAFE_INTEGER ± 1`, `i64::MAX`, `i64::MIN`)                          | unit                           | `npx vitest run src/services/view-syncer/decode-advance-buf.test.ts`                                             | ⚠️ Existing file, ADD tests in 32-01 |
| MIGRATE-01 | `#advancePipelines` consumes streaming AsyncIterable correctly                                | integration (PG)               | `npx vitest run src/services/view-syncer/view-syncer.pg.test.ts` (under `ZQLITE_RS_USE_STREAMING_CONSUMER=true`) | ✅ Existing tests, dual-mode wrap    |
| MIGRATE-02 | `#hydrateUnchangedQueries` consumes streaming AsyncIterable correctly                         | integration (PG)               | same as MIGRATE-01                                                                                               | ✅                                   |
| MIGRATE-03 | Mid-batch `pokePart` fires before slowest pipeline                                            | integration (PG, instrumented) | `npx vitest run src/services/view-syncer/view-syncer-streaming-mid-batch.test.ts`                                | ❌ NEW file (D-15)                   |
| MIGRATE-04 | `pokers.cancel()` × 1, `pokers.end()` × 0, no CVR commit on `ResetPipelinesSignal` mid-stream | integration (PG)               | `npx vitest run src/services/view-syncer/view-syncer-streaming-reset.test.ts`                                    | ❌ NEW file (D-21)                   |

### Production signals introduced

| Signal                                     | Where observed                                                                                             | Sampling rate     | Why                                                                                  |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------- | ----------------- | ------------------------------------------------------------------------------------ |
| `pokePart` arrival timing in client stream | `Queue<Downstream>` in test; OTel span `vs.#advancePipelines` in production                                | Per advance batch | Confirms streaming actually delivers mid-batch pokes (MIGRATE-03)                    |
| `RustStreamError.kind` frequency           | `lc.error('rust streaming error', { kind, message, source })` structured log                               | Per error event   | Per D-08; observability for `panic` vs `rayon_error` vs `channel_closed` post-deploy |
| `ResetPipelinesSignal` mid-stream rate     | Existing `vs.#advancePipelines` span + `pokers.cancel()` invocation (already metered as `#pipelineResets`) | Per reset         | Verify D-19 path works under streaming (was previously buffered-only)                |
| `vs.#advancePipelines.pokeEnd` span timing | Existing OTel span at view-syncer.ts:2218                                                                  | Per advance batch | Confirms CVR commit happens AFTER full stream consumes; no regression                |
| `transactionAdvanceTime` metric            | Existing histogram at view-syncer.ts:2227                                                                  | Per advance batch | Wall-time should be similar (or better via TTFB) under streaming                     |

### Sampling rate

- **Per task commit:** `npx vitest --config vitest.config.no-pg.ts run src/services/view-syncer/{decode-advance-buf,pipeline-driver}*.test.ts` (~30s, 13 files / 141 tests per 31-02 baseline)
- **Per wave merge:** Full PG suite under both `ZQLITE_RS_USE_STREAMING_CONSUMER=true` AND `=false` per D-24
- **Phase gate (per D-23):** `assertNapiBinaryFreshness` covers Phase 32 automatically (no new gate)
- **Phase gate (full):** Both env-var modes green + Rust `cargo test --release --lib -- --test-threads=1` unchanged (128 passed, 1 ignored per 31-01 baseline)

### Wave 0 Gaps

For 32-01 (CR-01 fix):

- ❌ `packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts` — file exists, MUST add 6 boundary tests per D-14

For 32-02 (migration):

- ❌ `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-mid-batch.test.ts` — NEW file (MIGRATE-03)
- ❌ `packages/zero-cache/src/services/view-syncer/view-syncer-streaming-reset.test.ts` — NEW file (MIGRATE-04)
- ⚠️ `view-syncer-test-util.ts` — extend `connectWithQueueAndSource` with `onMessage` callback for timestamp recording
- ⚠️ Existing `view-syncer.pg.test.ts` and related — wrap in `describe.each([useStreaming: true/false])` blocks

---

## Assumptions Log

| #   | Claim                                                                                                                  | Section                                  | Risk if Wrong                                                                                                                                                  |
| --- | ---------------------------------------------------------------------------------------------------------------------- | ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1  | `ViewSyncerService` constructor is the right place to read `ZQLITE_RS_USE_STREAMING_CONSUMER` (vs module top-level)    | §2 Env var idiom + §3 P-03               | If module-init is required, dual-mode tests must use `vi.resetModules()` per `beforeEach` — substantially more invasive                                        |
| A2  | `connectWithQueueAndSource` is the right injection point for poke timestamp recording                                  | §5 Pokers timestamp recording            | Could alternatively spy on `PokeHandler` directly if pipeline-driver test patterns introduce a mock; would need to trace through `client-handler.ts` to verify |
| A3  | Mock pipelines via Proxy on `RustPipelineManager.prototype.advanceStreaming` is the cleanest staggered-timer mechanism | §5 Mock pipeline staggered timing helper | Alternative: spy on `PipelineDriver.advanceStreaming` itself (one level up). Both work.                                                                        |
| A4  | `BigInt(lo)` (without `>>> 0`) is sufficient in CR-01 fix because `getUint32` returns non-negative                     | §4 CR-01 Fix                             | Defensive `>>> 0` would not break anything; pre-existing code uses it.                                                                                         |
| A5  | `hi <= -0x200000` (symmetric) is preferred over `hi < -0x200000` (strict) for the negative branch                      | §3 P-07                                  | Strict `<` is closer to mathematically tight; symmetric `<=` is more conservative. Either passes the D-14 test cases.                                          |
| A6  | View-syncer tests have NO direct `pokers.pokePart` mock and observe via `Queue<Downstream>` only                       | §2 Pokers mock pattern                   | Verified by grep returning no `mock(.*pokers)`/`spyOn.*pokePart` matches across view-syncer test files.                                                        |

---

## Open Questions

1. **Is there a `sleep` helper in `shared/`?** Used in §2 staggered-timer mock. If not, inline `new Promise(r => setTimeout(r, ms))`. (Not blocking — trivial alternative.)
2. **Should the new `view-syncer-streaming-mid-batch.test.ts` use the PG `setup()` harness, or a lighter unit-test setup like `pipeline-driver.streaming.test.ts`?** PG harness is more realistic but slower; unit setup is faster but requires duplicating the client connection wiring. **Recommendation: use PG harness for parity with existing view-syncer test conventions.**
3. **Does `oxlint` flag any of the proposed Proxy patterns?** TEST-05 in `pipeline-driver.streaming.test.ts:265-299` already uses Proxy and passes lint, so likely safe.

---

## Sources

### Primary (HIGH confidence — direct file inspection)

- `packages/zero-cache/src/services/view-syncer/view-syncer.ts` (lines 470-535, 1300-1430, 1850-1965, 2050-2230) — production callsites
- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts:240-309` — CR-01 target code
- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts` — test pattern reference
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:100-260, 854-870, 1100-1440, 1845-1995, 2140-2220` — Phase 31 streaming surface
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts` — Phase 31 test patterns (Proxy on prototype, fixture setup)
- `packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts:1-120` — fast-check pattern
- `packages/zero-cache/src/services/view-syncer/view-syncer-test-util.ts` — PG integration test harness, `nextPokeParts`, `connectWithQueueAndSource`
- `packages/zero-cache/src/services/view-syncer/view-syncer.yield-during-advance.pg.test.ts:1-100` — example consumer of the harness
- `.planning/phases/31-streaming-primitives-and-wrappers/31-{01,02}-SUMMARY.md` — what's already shipped
- `.planning/phases/31-streaming-primitives-and-wrappers/31-REVIEW.md` — CR-01 details (lines 41-78)

### Secondary (HIGH confidence — verified by grep)

- `process.env.ZERO_DISABLE_RUST_IVM` usage across view-syncer test files (6 files match) — env var idiom precedent
- `describe.each` usage at `view-syncer.pg.test.ts:3084` and `process-mutations.test.ts:1013` — pattern precedent
- `#processChanges(` callsite count: 2 (lines 1929 and 2198) — verified via grep on view-syncer.ts

---

## Metadata

**Confidence breakdown:**

- Existing code reference points: HIGH — direct line inspection
- Library / pattern specifics: HIGH — env var, describe.each, Proxy patterns all exist in tree
- CR-01 fix details: HIGH — code inspected; threshold math verified
- Test patterns: HIGH for dual-mode + Proxy reuse; MEDIUM for the timestamp-recording approach (no precedent — new pattern)
- Validation architecture: HIGH — vitest configs verified

**Research date:** 2026-04-29
**Valid until:** 2026-05-29 (30 days — codebase is stable; Phase 31 just landed)

---

## RESEARCH COMPLETE
