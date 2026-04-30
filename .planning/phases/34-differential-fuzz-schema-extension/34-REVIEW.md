---
phase: 34-differential-fuzz-schema-extension
reviewed: 2026-04-29T00:00:00Z
depth: standard
files_reviewed: 14
files_reviewed_list:
  - packages/zqlite-rs/src/ast_to_config.rs
  - packages/zero-ivm-rs/src/exists_op.rs
  - packages/zero-ivm-rs/src/take_op.rs
  - packages/zqlite-rs/src/advance.rs
  - packages/zqlite-rs/src/pipeline_manager.rs
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.ts
  - tools/ivm-parity/arb-ast.ts
  - tools/ivm-parity/random-ast-fuzz.ts
  - tools/ivm-parity/harness-fuzz.ts
  - tools/ivm-parity/diff-tests-track2.ts
  - tools/ivm-parity/zero-schema.ts
  - tools/ivm-parity/schema.sql
  - tools/ivm-parity/seed-extras.sql
  - tools/ivm-parity/parity-allowlist.json
findings:
  critical: 0
  warning: 3
  info: 4
  total: 7
status: issues_found
---

# Phase 34: Code Review Report

**Reviewed:** 2026-04-29T00:00:00Z
**Depth:** standard
**Files Reviewed:** 14
**Status:** issues_found

## Summary

The Track 2 fixes (B1, B2, B3, B11) are well-implemented and well-cited:

- **B1 Skip-before-conditions** (`ast_to_config.rs:233-243`): Skip emission moved
  ahead of the where-condition block, mirroring TS `builder.ts:302-306`. The
  follow-up CSQ-vs-Filter-inside-And ordering caveat is correctly documented as
  `#[ignore]`d (`test_b1_csq_then_filter_inside_and`) per plan 34-02.
- **B2 `parent_sizes.max(1)` removal** (`exists_op.rs:177`): The cache now stores
  the real `children.len()`. A regression-guard test
  (`test_b2_parent_sizes_real_count`) asserts both the behavior AND a
  source-level grep that the `.max(1)` literal does not reappear.
- **B3 partition_key threading** (`ast_to_config.rs:206`, plus 4 recursion
  sites): the `partition_key: Option<Vec<String>>` parameter flows through
  `ast_to_operator_configs`; recursion sites at `:278-284`, `:410-416`,
  `:463-469`, `:594-601` all pass `Some(rel.correlation.child_field.clone())`,
  matching TS `builder.ts:626-632`. `take_op.rs:286-289` builds a Row from
  constraint and reuses `take_state_key` so fetch/push keys agree.
- **B11 additive `set_prev_snapshot`** (`pipeline_manager.rs:312-321`): correctly
  marked `#[napi]`, no removal of existing attributes; `prev_db_path` is
  threaded through the new `advance_persistent_pipeline(_with_cancel)`
  signatures with a back-compat `eprintln!` fallback when unset. TS wires
  `setPrevSnapshot` BEFORE `swapSnapshot` at all three call sites
  (`pipeline-driver.ts:2095, 2241, 2333`). CLAUDE.md gate #4 satisfied: no
  signature changes to `advance/advanceAsync/hydrate*/addQuery*/addQueries*`.

CLAUDE.md gate #4 (no signature change) and D-22 (no CI integration) hold. The
TS-spec citations for each fix are in place (D-17). The fuzz harness, allow
list, and FUZZ-02 schema/seed additions match `34-CONTEXT.md` D-09..D-12. The
schema typing concern flagged in 34-03 is correctly applied: TIMESTAMPTZ /
JSONB / NUMERIC / BIGINT replicated as `string()` per Zero conventions.

Findings below are non-blocking: two are TS fuzz-driver logic bugs that
under-cover the targeted shapes (so Track 1 coverage in CI may be lower than
intended), and one is a pre-existing unconditional `/tmp` debug log write that
remains live in the B11 fix path. None affect Track 2 correctness.

## Warnings

### WR-01: arb-ast `relatedDepth0` draws from a second random table — produces ASTs with mismatched parent table and related[]

**File:** `tools/ivm-parity/arb-ast.ts:540-544`

**Issue:** The `relatedDepth0` arbitrary chains a second `arbTable` draw `t2`
and then calls `arbRelated(t2 === table ? table : t2, adapted, 0)`. The ternary
returns `table` only when `t2 === table` — i.e. when the second draw happens to
match. In every other case (the common path) it uses `t2`'s relationships,
which are correlated to `t2`'s columns, NOT the AST's `table.name`. Result: an
AST emitted with `table: 'channels'` may carry `related: [<messages-relation>]`
whose `parentField` (e.g. `['conversationId']`) does not exist on `channels`.

This will not crash — Zero/Rust likely emit zero rows for the bad related — but
it dilutes B3 coverage (the very shape `arbB3Targeted` aims to exercise) and
silently degrades base-arbitrary signal. The comment at line 526 explicitly
states "column/rel arbitraries are local to the chosen table"; the code does
the opposite.

**Fix:**

```ts
relatedDepth0: (() => {
  const r = arbRelated(table, adapted, 0);
  if (!r) return fc.constant(undefined);
  return fc.option(r, {nil: undefined, freq: 3});
})(),
```

Drop the `arbTable.chain(t2 => …)` wrapper entirely. The local closure already
captured `table` from the outer chain; no second draw is needed.

### WR-02: BatchedRunner.enqueueAndMaybeFlush always flushes — BATCH_SIZE is dead code

**File:** `tools/ivm-parity/harness-fuzz.ts:662-676`

**Issue:** The method's name and the `if (queued.length >= batchSize)` branch
imply that ASTs accumulate up to `batchSize` before flushing. But after that
branch, the function FALLS THROUGH to an unconditional `const out = await
flush()` (line 674). Every enqueue therefore flushes a 1-element batch, making
`BATCH_SIZE`, the comment "Sub-batch case", and `finalFlush()`'s "queued

> 0" branch all unreachable in practice. This collapses Mitigation 1 from
> RESEARCH §"CI Budget Compliance" into a no-op.

The intent (per the inline comment) is to flush per-AST so fast-check's
property body resolves immediately. If that is correct, the `if` branch and
the BATCH_SIZE plumbing should be removed; otherwise the second `flush()`
call should be guarded by an `else` to honor the batch boundary.

**Fix:** If per-AST flush is intentional (matches fast-check semantics), remove
the dead branch:

```ts
async enqueueAndMaybeFlush(ast: AST): Promise<RunOneAstResult> {
  queued.push({ast});
  const out = await flush();
  return out[out.length - 1];
},
```

Alternatively, to honor BATCH_SIZE, gate the trailing flush:

```ts
async enqueueAndMaybeFlush(ast: AST): Promise<RunOneAstResult> {
  queued.push({ast});
  if (queued.length >= batchSize) {
    const out = await flush();
    return out[out.length - 1];
  }
  // Defer flush; caller receives a synthetic ok and must rely on finalFlush().
  return {status: 'ok', tsRows: {}, rsRows: {}, tsHash: '', rsHash: ''};
},
```

### WR-03: advance_persistent_pipeline writes /tmp/rust_ivm_debug.log unconditionally on every advance

**File:** `packages/zqlite-rs/src/advance.rs:1510-1518`

**Issue:** Inside the (B11-touched) `advance_persistent_pipeline` function,
each invocation opens `/tmp/rust_ivm_debug.log` in append mode and writes a
debug record. There is no env-gate, no `cfg!(debug_assertions)`, and no log
level. In production this:

1. Opens a syscall on every advance (latency on the hot path).
2. Eventually fills `/tmp` on long-running deployments.
3. Leaks pipeline metadata (query IDs, table names) to a world-writable
   directory in container/multi-tenant environments.

The companion `advance_persistent_pipeline_with_cancel` (the streaming hot
path) does NOT have this debug log — only the older buffered path does — so
the impact is bounded but real for any deployment still on the buffered
advance route.

This is pre-existing (introduced in commit `599a7b229`, well before phase 34),
but the function under review is one of the four B11 fix sites, so the debug
log lives in code that this phase touched.

**Fix:**

```rust
// Debug: write to /tmp for investigation
#[cfg(debug_assertions)]
if std::env::var("ZQLITE_RS_DEBUG_LOG").is_ok() {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open("/tmp/rust_ivm_debug.log")
    {
        use std::io::Write;
        let _ = writeln!(f, "[advance_persistent] query={} source_table={} child_to_op={:?} has_ops={} changes={:?}",
            pipeline.query_id, pipeline.source_table,
            pipeline.child_table_to_op_index.keys().collect::<Vec<_>>(),
            pipeline.has_operators,
            changes.iter().map(|c| c.table.as_str()).collect::<Vec<_>>());
    }
}
```

Or simply remove if no longer needed for investigation.

## Info

### IN-01: Dead timing variables in advance_persistent_pipeline

**File:** `packages/zqlite-rs/src/advance.rs:1504-1508`

**Issue:** `t_start`, `t_diff`, `t_push`, `t_flatten` are written-only — never
read or logged. `t_child_has_parent` is declared (line 1508) and never even
written to. This is pre-existing dead code, but it sits in the B11 fix path.
Unused local vars cost a few cycles per advance.

**Fix:** Remove the declarations and the corresponding `t* += t*0.elapsed()`
assignments, or wire them into a `log::trace!` / OTel histogram if they were
intended for instrumentation.

### IN-02: arb-ast.ts `arbB3Targeted` falls back to `{table: table.name}` on no-relationship draws — yields no B3 trigger

**File:** `tools/ivm-parity/arb-ast.ts:644-654`

**Issue:** When `arbCorrelatedSubqueryForTable(table, adapted, {withLimit: lim})`
returns undefined (table has no relationships), the targeted arbitrary returns
`fc.constant({table: table.name})` — a bare AST with no `related[]`, no `limit`,
which exercises NEITHER the B3 trigger shape nor any other interesting
operator. With the `arbAstWithTargeted` weights `(7,1,1,1)` and only some
tables having relationships, this can quietly skip B3 trigger generation.

The defensive constant prevents a throw, but a stronger fallback is to filter
to `arbTableWithRels` and panic if empty (the schema is fixed):

**Fix:**

```ts
const arbB3Targeted: fc.Arbitrary<AST> = arbTableWithRels.chain(table => {
  return fc.integer({min: 1, max: 5}).chain(lim => {
    const csqArb = arbCorrelatedSubqueryForTable(table, adapted, {
      withLimit: lim,
    });
    if (!csqArb) {
      throw new Error(
        `arbB3Targeted: ${table.name} unexpectedly has no relationships — ` +
          `arbTableWithRels filter must be broken.`,
      );
    }
    return csqArb.map(csq => ({table: table.name, related: [csq]}));
  });
});
```

### IN-03: Misleading comment in BatchedRunner contradicts implementation

**File:** `tools/ivm-parity/harness-fuzz.ts:670-673`

**Issue:** Comment says "Sub-batch case: caller may still flush directly via
finalFlush()" but the next two lines unconditionally flush. Either the comment
is stale or the code is wrong (per WR-02). Update both consistently.

### IN-04: harness-fuzz `cleanFastReturns` synthetic-only — telemetry will be misleading until live batching lands

**File:** `tools/ivm-parity/harness-fuzz.ts:646-651`

**Issue:** `lastCleanupFastReturned` is hard-coded to `true` after every flush,
so `stats.cleanFastReturns` always equals `stats.batches`. This is documented
in the comment ("synthetic in hydrate-only mode — always true"), but the
metric is exposed via `runner.stats` and printed by `random-ast-fuzz.ts:407`
as if it were a real measurement. A user reading the timing-probe output may
draw false conclusions about Mitigation 2's effectiveness.

**Fix:** Either remove `cleanFastReturns` from the public stats until the live
advance-aware batched flush lands, or annotate the stats comment so callers
can distinguish synthetic-vs-measured fields:

```ts
/** ... When live advance-aware batching is added, set this from the actual
 *  cleanupDb return path. **Until then this counter equals `batches`.** */
cleanFastReturns: number;
```

---

_Reviewed: 2026-04-29T00:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
