---
phase: 260430-gqe
plan: 01
type: execute
wave: 1
depends_on: []
files_modified:
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.ts
  - packages/zero-cache/src/services/view-syncer/parity-check.test.ts
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts
  - packages/zqlite-rs/src/hydrate.rs
  - packages/zqlite-rs/index.d.ts
autonomous: true
requirements: []
---

<objective>
Close two production-vs-test divergences in the Rust IVM:

1. **Wire `#maybeRunParityCheck` into the streaming hydrate + advance paths** so the
   default production code path (`useStreamingConsumer=true`) actually runs the
   TS oracle comparison instead of silently bypassing it.
2. **Re-point `pipeline-driver.bench.ts` from the bench-only `RustPipeline` NAPI
   class to the production `RustPipelineManager`**, then delete the dead
   `RustPipeline` class and any genuinely-orphaned helpers in `hydrate.rs` so
   bench numbers reflect production behaviour and the surface area shrinks.

Each item ships as a separate atomic commit — Item 1 first (purely additive,
lower risk), Item 2 second (deletion-heavy). Bisectability is non-negotiable.

Purpose: production `advanceStreaming` / `addQueriesStreaming` callers are the
ones running today (view-syncer.ts:361-365 defaults `useStreamingConsumer=true`),
and they currently never hit the parity oracle. The bench file's `RustPipeline`
path likewise tests a code path no production caller exercises. Both are
silent regressions in our correctness story.

Output: parity-check fires on streaming hydrate + advance with regression
coverage; bench file uses production NAPI surface; dead `RustPipeline` napi
class removed; `index.d.ts` regenerated without `RustPipeline`.
</objective>

<execution_context>
@/Users/kartik.parsoya/Documents/Zero/mono-rs/.claude/get-shit-done/workflows/execute-plan.md
@/Users/kartik.parsoya/Documents/Zero/mono-rs/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@./CLAUDE.md
@./AGENTS.md
@packages/zero-cache/src/services/view-syncer/pipeline-driver.ts
@packages/zero-cache/src/services/view-syncer/dual-executor.ts
@packages/zero-cache/src/services/view-syncer/pipeline-driver-ts-oracle.ts
@packages/zero-cache/src/services/view-syncer/parity-check.test.ts
@packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts
@packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts
@packages/zqlite-rs/src/hydrate.rs
@packages/zqlite-rs/src/advance.rs
@packages/zqlite-rs/src/lib.rs
@packages/zqlite-rs/index.d.ts

<interfaces>
<!-- Key contracts the executor needs. Extracted from codebase. -->

# pipeline-driver.ts — parity-check shim (already exists, reused verbatim)

```typescript
// pipeline-driver.ts:519
async #maybeRunParityCheck(
  label: 'advance' | 'hydrate',
  rustChanges: RowChange[],
  runTsExpensive: () => Promise<RowChange[]>,
): Promise<void>

// pipeline-driver.ts:549
#oracleCtx(): TsOracleContext

// Mode env var read in constructor:
//   ZQLITE_RS_PARITY_CHECK = 'off' | 'sample' | 'strict'  (default: 'off')
//   ZQLITE_RS_PARITY_CHECK_RATE = number (default: 10)
```

# Existing wired sites (REFERENCE — DO NOT CHANGE):

```typescript
// addQueriesAsync (~line 1244): Rust hydrate batch parity
if (this.#parityCheckMode !== 'off') {
  const rustHydrateChanges: RowChange[] = [];
  for (const c of allChanges) {
    if (c !== 'yield') rustHydrateChanges.push(c);
  }
  await this.#maybeRunParityCheck('hydrate', rustHydrateChanges, async () => {
    const out: RowChange[] = [];
    const ctx = this.#oracleCtx();
    const eligibleQueries = prepared
      .filter(p => p.rustEligible)
      .map(p => ({transformationHash, queryID, resolvedQuery}));
    this.#hydrateContext = {timer};
    try {
      for (const c of tsAddQueryAll(ctx, eligibleQueries, timer)) {
        if (c !== 'yield') out.push(c);
      }
    } finally {
      this.#hydrateContext = null;
    }
    return out;
  });
}

// #rustAdvanceAsync (~line 2347): Rust advance parity
await this.#maybeRunParityCheck('advance', changes, async () =>
  materializeChanges(tsAdvance(this.#oracleCtx(), diff, timer, numChanges)),
);
```

# Streaming sites needing the wire-up (Item 1):

```typescript
// advanceStreaming returns AsyncIterable from #streamChanges (line 2207).
// #streamChanges is an async generator — the natural place to accumulate
// is INSIDE #streamChanges (yield to caller as today AND push to a local
// RowChange[]; fire #maybeRunParityCheck in the finally block once the
// stream completes successfully).

// addQueriesStreaming is itself an async generator (line 1307+):
// Phase 3a yields TS-hydrate companion rows; Phase 3b drains Rust streaming
// hydrate and buckets per queryID; Phase 3c yields rust-eligible rows.
// Accumulate yielded rust-eligible RowChange values across Phase 3c and fire
// #maybeRunParityCheck at end-of-generator.

// Both: skip 'yield' / 'chunk-end' sentinels when accumulating.
// Both: only run when this.#parityCheckMode !== 'off' (avoid the per-row
// branch overhead — the if-guard mirrors the existing addQueriesAsync wire).
```

# dual-executor exports (already imported in pipeline-driver.ts):

```typescript
export function compareChanges(
  tsChanges: RowChange[],
  rustChanges: RowChange[],
): { match: boolean; tsCount: number; rustCount: number; mismatches: ... };

export function materializeChanges(
  iterable: Iterable<RowChange | 'yield'>,
): RowChange[];
```

# Counter inspection (already exported, used by tests):

```typescript
export function getParityCheckInvocationCountForTesting(): number;
export function getParityDivergenceCount(): number;
export function resetParityDivergenceCount(): void;
```

# RustPipelineManager NAPI surface (production — used by bench rewrite, Item 2):

From packages/zqlite-rs/index.d.ts (auto-generated):

```typescript
export declare class RustPipelineManager {
  constructor();
  static createInstance(dbPath: string, instanceId: string): void;
  addQuery(instanceId: string, queryJson: string): void;
  removeQuery(instanceId: string, queryId: string): void;
  hydrate(instanceId: string): Buffer; // sync
  advance(instanceId: string, changesJson: string): Buffer;
  swapSnapshot(instanceId: string, newDbPath: string): void;
  setPrevSnapshot(instanceId: string, prevDbPath: string): void;
  setPermissionTables(instanceId: string, json: string): void;
  // ... advanceStreaming / hydrateStreaming for streaming variants
}
```

# hydrate.rs map (verified line numbers):

```
Line  Symbol                                       Status
------- ----------------------------------------- -------------------------------
18    LiveTableSource                             KEEP (used by build_operator_chain)
85    value_to_group_key                          KEEP unless verified unused
96    PreloadedSource                             KEEP if apply_child_operators kept
122   extract_child_source_info                   verify after Parallel* deleted
147   apply_child_operators                       called by Parallel* ONLY (verify)
168   batch_fetch_children                        called by Parallel* ONLY (verify)
254   make_child_source                           CALLED by build_push_next_operator
                                                  (production!) — KEEP
291   ParallelJoinOperator                        DELETION CANDIDATE — but build_next_operator
                                                  at 853 references it; build_next_operator
                                                  is called by build_operator_with_live_source
                                                  (701) which is called by build_push_next_operator
                                                  (1095/1127/1157) which is called by
                                                  build_operator_chain (production line 989).
                                                  REQUIRES RUNTIME VERIFICATION.
393   ParallelExistsOperator                      Same conditional status as above.
518   ParallelOrExistsOperator                    Same conditional status as above.
657   HydratePipelineConfig                       Used by hydrate_pipelines only — DELETE if
                                                  hydrate_pipelines deleted.
669   hydrate_pipelines                           DEAD via tests only (lines 1357/1416/1448/
                                                  1470/1643). DELETE candidate.
701   build_operator_with_live_source             USED BY build_push_next_operator (production)
                                                  — KEEP.
812   build_next_operator                         USED BY build_operator_with_live_source — KEEP.
949   OperatorChain                               KEEP (production).
964   build_operator_chain                        KEEP (production, primary entry).
1001  SourceBridgeOperator                        USED BY build_operator_chain at line 984 —
                                                  KEEP (contradicts task description).
1051  build_push_next_operator                    KEEP (production).
```

**Critical correction to task description:**

- `SourceBridgeOperator` IS used by `build_operator_chain` (production) — MUST KEEP.
- `build_operator_with_live_source` IS used (transitively) by `build_operator_chain`
  via `build_push_next_operator` — MUST KEEP.
- `ParallelJoinOperator` / `ParallelExistsOperator` / `ParallelOrExistsOperator`
  ARE referenced by `build_next_operator` (which IS production-reachable). The
  question is whether the production AST shape ever produces an OperatorConfig
  whose nested-child branch hits these — runtime verification required.

The deletion target is genuinely:

- `RustPipeline` class in `advance.rs` (~lines 1075-end of impl) — confirmed only
  consumed by the bench file.
- `hydrate_pipelines` (line 669) + `HydratePipelineConfig` (line 657) — only
  referenced by hydrate.rs's own tests.
- The hydrate.rs `#[cfg(test)] mod tests` blocks that exercise the deleted
  symbols.

Whether the Parallel\* operators can also be deleted is conditional on
`build_next_operator`'s production reachability — investigate during execution
before deleting; the safe default is KEEP.

# Bench rewrite shape (Item 2 task 2):

```typescript
// Replace:
let RustPipelineClass = bindings?.RustPipeline;
// with:
let RustPipelineManagerClass = bindings?.RustPipelineManager;

// Per bench:
//   const instanceId = `bench-${Date.now()}-${i++}`;
//   RustPipelineManagerClass.createInstance(dbFile.path, instanceId);
//   const mgr = new RustPipelineManagerClass();
//   mgr.addQuery(instanceId, JSON.stringify(buildHydrateQuery('q1', AST)));
//   const buf = mgr.hydrate(instanceId);
//   ... or for advance: mgr.swapSnapshot + mgr.advance
//   ... cleanup: mgr.removeQuery(instanceId, 'q1') if needed
//
// Bench labels: 'Rust (Manager)' (so future bench output diffs are obvious).
```

</interfaces>
</context>

<tasks>

<task type="auto" tdd="true">
  <name>Task 1 (ITEM 1): Wire #maybeRunParityCheck into advanceStreaming + addQueriesStreaming with regression test</name>
  <files>
    packages/zero-cache/src/services/view-syncer/pipeline-driver.ts,
    packages/zero-cache/src/services/view-syncer/parity-check.test.ts
  </files>
  <behavior>
    Tests to add to parity-check.test.ts (new `describe('streaming parity')` block):

    - Test S1: `mode=sample, rate=1, advanceStreaming` → drives an advance
      against `advanceStreaming`, fully consumes the AsyncIterable via
      `for await`, asserts `getParityCheckInvocationCountForTesting()` strictly
      increments past pre-stream baseline. (Mirrors the existing rate=1
      addQueriesAsync test but for the streaming path.)

    - Test S2: `mode=sample, rate=1, addQueriesStreaming` → drives a hydrate
      via `addQueriesStreaming`, fully consumes the AsyncIterable, asserts
      invocation counter incremented.

    - Test S3 (divergence detection on streaming advance):
        * `mode=strict`
        * Use vi.spyOn on `tsAdvance` (imported from
          pipeline-driver-ts-oracle.ts) — or the equivalent
          `pipeline-driver-ts-oracle` module export — to return one extra
          synthetic `RowChange` so divergence is guaranteed.
        * If module-level spy is awkward, fall back to: bypass the
          mock-spy approach and instead use a divergence-injection
          escape hatch already present in the codebase (search for one in
          parity-check.test.ts; if none exists, prefer `vi.mock(...)` of
          `./pipeline-driver-ts-oracle.ts` with a partial impl that wraps
          `tsAdvance` and pushes a fake change).
        * Drive `advanceStreaming`, fully consume.
        * Assert: stream consumption either throws (strict mode) OR
          `getParityDivergenceCount() > 0` after consumption. Both outcomes
          prove the comparison fired against streaming output.

    - Test S4 (symmetric divergence detection on streaming hydrate):
        * Same shape as S3 but mocks `tsAddQueryAll` to inject a divergence,
          drives `addQueriesStreaming`, asserts strict-mode throw OR
          divergence count incremented.

    Reuse `makeDriver()` setup pattern from the existing parity-check.test.ts
    cadence-behavior block (DbFile + Snapshotter + InspectorDelegate +
    initReplicationState + driver.init). Each test sets/restores
    `process.env['ZQLITE_RS_PARITY_CHECK']` and
    `process.env['ZQLITE_RS_PARITY_CHECK_RATE']` exactly like the existing
    tests do. Each test calls `resetParityDivergenceCount()` in a beforeEach.

    Tests MUST fail RED before implementation (mode=off baseline confirms the
    counter does NOT increment on streaming today; flipping mode=sample/strict
    today produces 0 increment because the streaming paths bypass the shim).

  </behavior>
  <action>
    GREEN-phase implementation in pipeline-driver.ts (additive only — never
    substitute TS output for production output, mirror the P-07 anti-pattern
    avoidance comment from existing call sites):

    1. **Modify `#streamChanges` (line 2207, the advance streaming generator):**
       - Add a parity-collection accumulator alongside the existing yields:
         declare `const rustAdvanceChangesForParity: RowChange[] = []` at the
         top of the function, gated behind `if (this.#parityCheckMode !== 'off')`
         (assign the empty array conditionally, leave undefined otherwise — keeps
         the hot path branch-predictable).
       - In the `case 'chunk':` branch, after `for (const change of this.#convertDispatchChanges(decoded))`,
         when `change !== 'yield'`, also push the converted change into
         `rustAdvanceChangesForParity` if accumulator is defined. (Push BEFORE
         the yield — accumulator is local to this call, so post-yield mutation
         is safe but pre-yield is clearer.)
       - In the existing `finally` block (after `stream.return()` + advance
         context cleanup), AFTER setting `this.#advanceContext = null`, fire
         the parity check:
         ```ts
         if (rustAdvanceChangesForParity !== undefined) {
           // Reuse the diff captured in advanceStreaming's caller frame.
           // #streamChanges does NOT have access to `diff` — restructure:
           // pass `diff` (or a closure that reproduces the TS-side oracle
           // call) into #streamChanges as a new parameter, or run the
           // parity check from advanceStreaming's caller frame after the
           // returned AsyncIterable is fully consumed.
         }
         ```

       **Re-architecture decision (D-1 carried forward at execution):** rather
       than pass `diff` into `#streamChanges`, restructure so `advanceStreaming`
       wraps the AsyncIterable returned to the consumer with a teeing async
       generator that:
         a. yields each item from `#streamChanges` to the caller verbatim
         b. accumulates non-`'yield'`/non-`'chunk-end'` items into a local
            `RowChange[]` (only when `parityCheckMode !== 'off'`)
         c. on the consuming generator's natural completion (StopAsyncIteration),
            calls `await this.#maybeRunParityCheck('advance', accumulated,
            async () => materializeChanges(tsAdvance(this.#oracleCtx(), diff, timer, numChanges)))`

       The teeing wrapper must propagate errors and `return()` cancellation
       verbatim (do NOT swallow ResetPipelinesSignal / RustStreamError; do NOT
       run the parity check on errored streams — only on successful
       completion). Implement using a private async generator method
       `#streamChangesWithParity(stream, timer, numChanges, diff)` that
       internally awaits `#streamChanges(...)` and tees.

    2. **Modify `addQueriesStreaming` (line 1307, the hydrate streaming
       generator):**
       - Since this is an async generator that already yields directly, the
         tee can happen inside the existing function: declare
         `const rustHydrateChangesForParity: RowChange[] | undefined =
            this.#parityCheckMode !== 'off' ? [] : undefined;`
         at the top, after assertions.
       - Inside Phase 3c (~line 1677, the rust-eligible iteration that yields
         converted changes), accumulate into `rustHydrateChangesForParity`
         when defined, EXCLUDING `'yield'` and `'chunk-end'` sentinels.
       - At the very end of the generator (after the Phase 3c for-loop completes
         successfully — NOT inside any yield path; place after the closing
         brace of the rust-eligible loop), fire:
         ```ts
         if (rustHydrateChangesForParity) {
           await this.#maybeRunParityCheck(
             'hydrate',
             rustHydrateChangesForParity,
             async () => {
               const out: RowChange[] = [];
               const ctx = this.#oracleCtx();
               const eligibleQueries = prepared
                 .filter(p => p.rustEligible)
                 .map(p => ({
                   transformationHash: p.transformationHash,
                   queryID: p.queryID,
                   resolvedQuery: p.resolvedQuery,
                 }));
               this.#hydrateContext = {timer};
               try {
                 for (const c of tsAddQueryAll(ctx, eligibleQueries, timer)) {
                   if (c !== 'yield') out.push(c);
                 }
               } finally {
                 this.#hydrateContext = null;
               }
               return out;
             },
           );
         }
         ```
         This block is byte-for-byte the same TS-oracle invocation as the
         existing `addQueriesAsync` wire — DRY by extraction is tempting but
         adds risk; prefer copy-paste with a comment cross-referencing the
         buffered site. (If the file already has a private helper for this,
         reuse it; otherwise leave the duplication and note it for a
         follow-up.)

       **Caveat:** because `addQueriesStreaming` is itself an async generator,
       the parity-check call happens inside the generator body and runs only
       when the consumer fully drains the iterable. If the consumer breaks
       early (via `return`), the parity check is skipped — this is correct
       behaviour (consumer didn't observe the full stream, comparing partials
       would falsely diverge).

    3. **Comment additions** at each new wire site that mirror the existing
       sites (~line 1235 and ~line 2347 for tone):
       - "HARDEN-01 streaming wire: parity-check on the streaming hot path.
         Cost paid only when shouldRunParityCheck() returns true."
       - "Production output is the streamed Rust array; #maybeRunParityCheck
         returns void by design (P-07 anti-pattern avoided — never substitute
         TS for production)."
       - "Parity check fires AFTER consumer drains the AsyncIterable so
         streaming benefit is preserved (consumer sees first chunk before
         oracle runs)."

    Run lint + format + check-types (per AGENTS.md: every change). Run the
    new streaming-parity tests until GREEN. Run the full
    `pipeline-driver.streaming.test.ts` suite to confirm no regression.
    Run the full `parity-check.test.ts` suite to confirm pre-existing
    cadence/strict-mode tests still pass.

    **Commit (atomic for ITEM 1):**
    Subject: `feat(zero-cache): wire parity check into streaming hydrate + advance`
    Body: explain the production-vs-test divergence (view-syncer.ts:361-365
    defaults useStreamingConsumer=true, the existing parity wire only ran on
    addQueriesAsync / advanceAsync, so production never hit the oracle), and
    note that the wire is purely additive — production output unchanged.

  </action>
  <verify>
    <automated>cd /Users/kartik.parsoya/Documents/Zero/mono-rs && npm --workspace=zero-cache run check-types && npm --workspace=zero-cache run lint && npm --workspace=zero-cache run test -- src/services/view-syncer/parity-check.test.ts src/services/view-syncer/pipeline-driver.streaming.test.ts</automated>
  </verify>
  <done>
    - 4 new tests in parity-check.test.ts (or a new
      parity-check.streaming.test.ts) pass green.
    - Existing parity-check.test.ts tests still pass unchanged.
    - Existing pipeline-driver.streaming.test.ts tests still pass unchanged.
    - `git log -1 --oneline` shows the atomic ITEM 1 commit.
    - The diff in pipeline-driver.ts is purely additive (no production
      output paths changed; no existing yield order changed; streaming
      consumers see byte-identical output to before this commit when
      parityCheckMode === 'off' — verify by reading the diff).
  </done>
</task>

<task type="auto">
  <name>Task 2 (ITEM 2 — Step 1): Rewrite pipeline-driver.bench.ts to use RustPipelineManager (production NAPI surface)</name>
  <files>packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts</files>
  <action>
    Rewrite the bench file to use `RustPipelineManager` instead of `RustPipeline`.
    Bench numbers must reflect production behaviour.

    1. **Replace the binding lookup at the top of the file:**
       ```typescript
       // Before:
       let RustPipelineClass: ... | undefined;
       try {
         const bindings = esmRequire('zqlite-rs');
         RustPipelineClass = bindings?.RustPipeline;
       } catch { /* skip */ }

       // After:
       let RustPipelineManagerClass:
         | (new () => {
             addQuery(instanceId: string, queryJson: string): void;
             removeQuery(instanceId: string, queryId: string): void;
             hydrate(instanceId: string): Buffer;
             advance(instanceId: string, changesJson: string): Buffer;
             swapSnapshot(instanceId: string, newDbPath: string): void;
             setPrevSnapshot(instanceId: string, prevDbPath: string): void;
             setPermissionTables(instanceId: string, json: string): void;
           })
         | undefined;
       let RustPipelineManagerCreateInstance:
         | ((dbPath: string, instanceId: string) => void)
         | undefined;

       try {
         const bindings = esmRequire('zqlite-rs');
         RustPipelineManagerClass = bindings?.RustPipelineManager;
         // createInstance is a static method on the class:
         RustPipelineManagerCreateInstance =
           bindings?.RustPipelineManager?.createInstance?.bind(
             bindings.RustPipelineManager,
           );
       } catch { /* Rust bindings not available; benches skipped */ }
       ```

       **Verify before coding the rewrite:** open
       `packages/zqlite-rs/index.d.ts` and confirm the exact `RustPipelineManager`
       method signatures (createInstance vs new-then-setup, return types,
       static-vs-instance dichotomy). The pseudo-typing above is best-effort
       from the production pipeline-driver.ts call sites — adjust to match the
       actual `.d.ts` shape.

    2. **Update the file header comment block** (lines 1-13) to describe the
       new strategy:
       ```
       /**
        * Benchmark: Rust IVM (RustPipelineManager — production NAPI surface)
        *            vs TypeScript IVM (PipelineDriver with TS operator trees)
        *
        * Uses RustPipelineManager (the same class production view-syncer uses
        * via PipelineDriver.advanceAsync / addQueriesAsync) so bench numbers
        * reflect production behaviour — sequential operator chain via
        * build_operator_chain, not the deprecated Parallel* operator path.
        */
       ```

    3. **Rewrite each `bench('Rust (RustPipeline)', ...)` block** to use the
       Manager. Pattern:
       ```typescript
       if (RustPipelineManagerClass && RustPipelineManagerCreateInstance) {
         const Mgr = RustPipelineManagerClass;
         const createInstance = RustPipelineManagerCreateInstance;
         let instanceCounter = 0;
         bench('Rust (Manager)', () => {
           const instanceId = `bench-${Date.now()}-${instanceCounter++}`;
           createInstance(dbFile.path, instanceId);
           const mgr = new Mgr();
           const cfg = buildHydrateQuery('q1', SIMPLE_ISSUES);
           mgr.addQuery(instanceId, JSON.stringify(cfg));
           const buf = mgr.hydrate(instanceId);
           const d = decodeAdvanceResultBuf(buf);
           if (d.changes.length === 0) throw new Error('no results');
         });
       }
       ```

       Apply the same pattern to:
       - `hydrate: simple issues (3 rows)` block
       - `hydrate: issues + comments join (7 rows)` block
       - `hydrate: issues with EXISTS filter` block
       - `hydrate: bulk 1000 rows` block
       - The two `hydrate: N pipelines (Rayon par_iter)` describe blocks (use
         a single Manager instance with multiple `addQuery` calls — that's
         the production multi-pipeline shape)
       - `advance: single insert (small DB)` block
       - `advance: single insert (1000-row DB)` block
       - The PROFILE-advance-breakdown block (also rename inner `rustPipeline`
         locals to `mgr` and `instanceId`)
       - The multi-pipeline advance describe block

       **For advance benches**, the production sequence per pipeline-driver.ts
       (#rustAdvanceAsync, line 2284+) is:
       ```typescript
       mgr.setPrevSnapshot(instanceId, prevDbPath);
       mgr.swapSnapshot(instanceId, currDbPath);
       const buf = mgr.advance(instanceId, changesJson);
       ```
       — but in the bench's simplified setup there's only one DB file and
       changes are simulated; setPrevSnapshot can be omitted or set to the
       same path (mirror what the bench was doing before with
       `rustPipeline.swapSnapshot(dbFile.path)`). Document the simplification
       in a code comment.

    4. **Bench label rule:** every Rust bench label changes from
       `'Rust (RustPipeline)'` to `'Rust (Manager)'` so any subsequent bench
       output is unambiguous about which surface was tested.

    5. **Local sanity:** run `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts --no-bench`
       to confirm the file at minimum type-checks and any non-bench tests in
       it (if any) still pass. (Per task: bench run is optional.)

    6. **Commit** (deferred — combined commit with Task 3 below for the ITEM 2
       atomic commit per task constraints).

  </action>
  <verify>
    <automated>cd /Users/kartik.parsoya/Documents/Zero/mono-rs && npm --workspace=zero-cache run check-types && npm --workspace=zero-cache run lint && npx vitest run --root /Users/kartik.parsoya/Documents/Zero/mono-rs packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts --no-bench</automated>
  </verify>
  <done>
    - bench file no longer references `RustPipeline` (grep confirms zero
      matches in the bench file).
    - bench file uses `RustPipelineManager` + `createInstance` + `addQuery` +
      `hydrate` / `advance` for every Rust bench.
    - All bench labels use `'Rust (Manager)'`.
    - check-types + lint clean.
    - The bench file's vitest invocation (with `--no-bench`) loads without
      runtime error (proves construction paths work).
    - NO commit yet — combined with Task 3 for ITEM 2 atomic commit.
  </done>
</task>

<task type="auto">
  <name>Task 3 (ITEM 2 — Step 2): Verify dead code in hydrate.rs, delete what is genuinely unreferenced, delete RustPipeline class, regenerate index.d.ts; commit ITEM 2 atomically</name>
  <files>
    packages/zqlite-rs/src/hydrate.rs,
    packages/zqlite-rs/src/advance.rs,
    packages/zqlite-rs/index.d.ts,
    packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts
  </files>
  <action>
    **Order of operations is critical — Task 2's bench rewrite MUST land in the
    working tree before this task runs (NOT yet committed; this task creates
    the single ITEM 2 atomic commit covering both Task 2 and Task 3).**

    1. **Verification phase (MUST run first; gate later steps on its output):**

       Run these greps from repo root and DOCUMENT results in a working note
       (not committed) so the deletion list is evidence-based, not assumed:

       ```bash
       # Confirm RustPipeline (in advance.rs) has zero non-test consumers:
       grep -rn "RustPipeline\b" packages/ \
         --include="*.rs" --include="*.ts" --include="*.tsx" \
         | grep -v "RustPipelineManager" \
         | grep -v "/target/" \
         | grep -v "src/services/view-syncer/pipeline-driver.bench.ts"
       # Expected: only the definition site in advance.rs and any internal
       # tests/comments. If any production .ts file still references
       # RustPipeline (other than the bench, which Task 2 already cleaned),
       # ABORT and surface the consumer.

       # Confirm hydrate_pipelines has zero non-test consumers:
       grep -rn "hydrate_pipelines\b" packages/ \
         --include="*.rs" \
         | grep -v "/target/"
       # Expected: only the definition in hydrate.rs and the test bodies
       # at lines ~1357/1416/1448/1470/1643. If anything else references
       # it, ABORT.

       # Confirm Parallel* operators reachability (the disputed deletion target):
       # Are they referenced ONLY from build_next_operator? Yes per the line
       # map. But is build_next_operator only reached from
       # build_operator_with_live_source -> build_push_next_operator? If yes
       # AND if the production AST shape never produces nested-child
       # OperatorConfigs whose build_push_next_operator path constructs them,
       # they ARE dead. Otherwise, KEEP.
       grep -n "ParallelJoinOperator\|ParallelExistsOperator\|ParallelOrExistsOperator" \
         packages/zqlite-rs/src/hydrate.rs
       # Then trace each call site upward.
       ```

       **Decision matrix output (record in commit message):**

       | Symbol                          | Verified status                | Action |
       |---------------------------------|--------------------------------|--------|
       | RustPipeline (advance.rs)       | only consumer was bench (now removed in Task 2) | DELETE |
       | hydrate_pipelines               | only consumed by hydrate.rs internal tests       | DELETE |
       | HydratePipelineConfig           | only used by hydrate_pipelines                    | DELETE |
       | hydrate.rs internal tests using above | DELETE alongside (or rewrite to use build_operator_chain — pick whichever shrinks the diff) |
       | SourceBridgeOperator            | USED BY build_operator_chain @ line 984          | KEEP — task description was incorrect |
       | build_operator_with_live_source | USED BY build_push_next_operator (production)    | KEEP — task description was incorrect |
       | build_next_operator             | USED BY build_operator_with_live_source          | KEEP |
       | ParallelJoinOperator            | USED BY build_next_operator (production-reachable) | **KEEP unless runtime trace proves dead** — default KEEP |
       | ParallelExistsOperator          | Same as above                                    | KEEP unless proven dead — default KEEP |
       | ParallelOrExistsOperator        | Same as above                                    | KEEP unless proven dead — default KEEP |
       | apply_child_operators           | called by Parallel* only                          | DELETE iff Parallel* deleted |
       | batch_fetch_children            | called by Parallel* only                          | DELETE iff Parallel* deleted |
       | value_to_group_key              | check usage; KEEP if any retained code uses it    | conditional |

       **Default policy when verification is ambiguous: KEEP.** This task is a
       cleanup pass, not a correctness investigation. We can delete in a
       follow-up commit once usage is conclusively traced. Atomic-bisect
       safety > maximal deletion.

    2. **Deletion phase (only the verified-dead set):**

       Apply edits to:

       a. `packages/zqlite-rs/src/advance.rs`:
          - Find the `#[napi]` block declaring `pub struct RustPipeline` (~line
            1075) and its `impl RustPipeline { ... }` block (~line 1692).
          - Delete both. Also delete the `unsafe impl Send/Sync for
            RustPipeline` impls (~lines 1088-1089).
          - Search for `RustPipeline` in the same file's tests (the file has
            tests around lines 2558+, 2601+ that mention `RustPipelineManager`
            but use `RustPipeline` in comments/strings — leave the comments,
            delete only the actual class).

       b. `packages/zqlite-rs/src/hydrate.rs`:
          - Delete `pub fn hydrate_pipelines` (~lines 669-691).
          - Delete `pub struct HydratePipelineConfig` (~line 657-...).
          - Delete `pub struct HydrateResult` ONLY if it's not used elsewhere
            (verify with grep — if `advance.rs` uses it, keep it).
          - Delete or migrate the `#[cfg(test)] mod tests` blocks that
            exclusively exercise `hydrate_pipelines` (lines around 1357, 1416,
            1448, 1470, 1643). Prefer DELETION over migration — the `build_operator_chain`
            production path already has its own test coverage in advance.rs and
            pipeline_manager.rs. Note in commit body: "removed N hydrate.rs
            tests that exercised the dead hydrate_pipelines path; production
            coverage lives in pipeline_manager.rs and the view-syncer integration
            tests."

       c. `packages/zqlite-rs/index.d.ts`:
          - Run `npm --workspace=zqlite-rs run build` (the napi build target —
            check `packages/zqlite-rs/package.json` scripts; common name is
            `build` or `build:debug`). The build regenerates `index.d.ts`.
          - After regeneration, grep `index.d.ts` for `RustPipeline\b` and
            confirm `export declare class RustPipeline` is gone (only
            `RustPipelineManager` remains).
          - If the file is hand-edited (some napi-rs setups commit a hand-curated
            `.d.ts`), edit it manually to remove the `RustPipeline` class
            declaration.

    3. **Build + test verification gate (MUST all pass before commit):**

       ```bash
       # Rust workspace builds clean:
       cd /Users/kartik.parsoya/Documents/Zero/mono-rs/packages/zqlite-rs
       cargo build --release

       # Rust unit tests in zqlite-rs:
       cargo test --release -- --test-threads=1

       # Rust unit tests in zero-ivm-rs (fan-out check):
       cd /Users/kartik.parsoya/Documents/Zero/mono-rs/packages/zero-ivm-rs
       cargo test --release

       # Napi build (regenerates index.d.ts):
       cd /Users/kartik.parsoya/Documents/Zero/mono-rs/packages/zqlite-rs
       npm run build

       # Confirm the .d.ts no longer declares RustPipeline:
       grep "RustPipeline\b" /Users/kartik.parsoya/Documents/Zero/mono-rs/packages/zqlite-rs/index.d.ts \
         | grep -v "RustPipelineManager"
       # Expected: zero matches.

       # TS-side regression suite (ITEM 1 + ITEM 2 combined):
       cd /Users/kartik.parsoya/Documents/Zero/mono-rs
       npm --workspace=zero-cache run check-types
       npm --workspace=zero-cache run lint
       npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts
       npx vitest run packages/zero-cache/src/services/view-syncer/parity-check.test.ts
       npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts
       npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts --no-bench
       ```

       Any failure → debug + fix → re-run. Do NOT skip hooks. Do NOT commit
       on failure.

    4. **Atomic commit for ITEM 2** (covers Task 2 + Task 3):

       ```
       refactor(zqlite-rs): drop RustPipeline class + dead hydrate code; switch bench to RustPipelineManager

       Removes the `RustPipeline` napi class (advance.rs) and `hydrate_pipelines`
       + `HydratePipelineConfig` (hydrate.rs); these were only consumed by
       pipeline-driver.bench.ts (the `RustPipeline` class) and hydrate.rs's
       own internal tests (the `hydrate_pipelines` function). Production uses
       `RustPipelineManager` -> `build_operator_chain` -> sequential operator
       trees. `index.d.ts` regenerated.

       Bench file rewritten to use `RustPipelineManager` so bench numbers
       reflect the production napi surface. Bench labels updated from
       'Rust (RustPipeline)' to 'Rust (Manager)' for clarity in any future
       bench output.

       Verified KEEP (contradicts the original task description):
       - SourceBridgeOperator: used by build_operator_chain (production)
       - build_operator_with_live_source / build_next_operator: used by
         build_push_next_operator (production) for nested child operator
         trees during push
       - ParallelJoinOperator / ParallelExistsOperator / ParallelOrExistsOperator:
         referenced by build_next_operator and reachable via
         build_push_next_operator's nested-child path; conservative KEEP
         pending a focused production-reachability audit (out of scope for
         this commit).

       Tests removed from hydrate.rs covered the deleted hydrate_pipelines
       path; production behaviour stays covered by pipeline_manager.rs unit
       tests and the view-syncer integration suite.
       ```

       Stage exactly the files in `files_modified`:
       ```bash
       git add \
         packages/zqlite-rs/src/hydrate.rs \
         packages/zqlite-rs/src/advance.rs \
         packages/zqlite-rs/index.d.ts \
         packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts
       ```
       Confirm `git status` shows only those four. Do NOT include
       `packages/zero-ivm-rs/target/` artifacts (the working tree may have
       dirty fingerprint .json files from cargo — leave them unstaged; if
       any get accidentally added, `git restore --staged` them).

       Commit. Run `git log -1 --stat` to confirm.

  </action>
  <verify>
    <automated>cd /Users/kartik.parsoya/Documents/Zero/mono-rs && cargo test --release -p zqlite-rs -- --test-threads=1 && cargo test --release -p zero-ivm-rs && npm --workspace=zero-cache run check-types && npm --workspace=zero-cache run lint && npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts packages/zero-cache/src/services/view-syncer/parity-check.test.ts packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts && npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts --no-bench && (! grep -E "RustPipeline\b" packages/zqlite-rs/index.d.ts | grep -v "RustPipelineManager")</automated>
  </verify>
  <done>
    - `packages/zqlite-rs/index.d.ts` has zero matches for `RustPipeline\b`
      that are not part of `RustPipelineManager`.
    - `cargo build --release -p zqlite-rs` clean.
    - `cargo test --release -p zqlite-rs` clean.
    - `cargo test --release -p zero-ivm-rs` clean.
    - All four named TS test suites green.
    - `git log --oneline -2` shows: ITEM 1 commit (subject:
      `feat(zero-cache): wire parity check into streaming hydrate + advance`)
      followed by ITEM 2 commit (subject:
      `refactor(zqlite-rs): drop RustPipeline class + dead hydrate code; switch bench to RustPipelineManager`).
      Bisect order is correct: ITEM 1 first, ITEM 2 second.
    - The KEEP list in the verification phase is documented in the ITEM 2
      commit body so a future audit can re-evaluate the Parallel* operators
      with full context.
  </done>
</task>

</tasks>

<verification>
End-of-phase aggregate verification (single command for the orchestrator):

```bash
cd /Users/kartik.parsoya/Documents/Zero/mono-rs && \
  cargo test --release -p zqlite-rs -- --test-threads=1 && \
  cargo test --release -p zero-ivm-rs && \
  npm --workspace=zero-cache run check-types && \
  npm --workspace=zero-cache run lint && \
  npx vitest run \
    packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts \
    packages/zero-cache/src/services/view-syncer/parity-check.test.ts \
    packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts && \
  npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts --no-bench && \
  (! grep -E "RustPipeline\b" packages/zqlite-rs/index.d.ts | grep -v RustPipelineManager) && \
  git log --oneline -2
```

</verification>

<success_criteria>

1. **Streaming parity wire (ITEM 1)** lands as a single atomic commit that:
   - Adds parity-check accumulators to `advanceStreaming` (via a teeing wrapper
     around `#streamChanges`) and `addQueriesStreaming` (inline accumulator +
     end-of-generator parity call).
   - Adds 4 regression tests: 2 cadence-style (sample/rate=1 → invocationCount
     increments) and 2 divergence-style (strict mode + injected divergence →
     throw or divergence count > 0).
   - Leaves production output paths byte-identical when
     `parityCheckMode === 'off'` (the production default).
   - Subject: `feat(zero-cache): wire parity check into streaming hydrate + advance`.

2. **Bench retarget + dead code drop (ITEM 2)** lands as a single atomic commit
   AFTER ITEM 1 that:
   - Switches `pipeline-driver.bench.ts` to `RustPipelineManager` (production
     surface), with bench labels `'Rust (Manager)'`.
   - Deletes the `RustPipeline` napi class from `advance.rs`.
   - Deletes `hydrate_pipelines` + `HydratePipelineConfig` from `hydrate.rs`
     and the test bodies that exercised them.
   - Documents the verified-KEEP list (`SourceBridgeOperator`,
     `build_operator_with_live_source`, `build_next_operator`, Parallel\*
     operators) in the commit body.
   - Regenerates `packages/zqlite-rs/index.d.ts` without `RustPipeline`.
   - Subject: `refactor(zqlite-rs): drop RustPipeline class + dead hydrate code; switch bench to RustPipelineManager`.

3. The full verification gate passes (cargo tests, ts check-types, ts lint,
   the four named vitest suites, the .d.ts grep guard).

4. `git log --oneline -2` shows ITEM 1 then ITEM 2 (correct bisect order).
   No `--no-verify`. No skipped hooks. No `git config` mutation.
   </success_criteria>

<output>
After completion, create the standard quick-mode summary:

`.planning/quick/260430-gqe-wire-parity-check-into-streaming-drop-be/260430-gqe-SUMMARY.md`

Include:

- Verification gate output (cargo + vitest + grep guard).
- Both commit hashes + subjects.
- The verified-KEEP list (Parallel\* operators etc.) with reasoning, so the
  follow-up audit has a starting point.
- Any deviations from the plan (e.g., if `build_operator_with_live_source`
  turned out to need different handling, or if a Parallel\* operator was
  conclusively traced as dead and deleted).
  </output>
