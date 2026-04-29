---
phase: 30-audit-fixes
plan: 02
subsystem: ivm/advance-path
tags: [audit-fix, exists, split-edit-keys, regression-fix]
requires:
  - 220f0fc3b
provides:
  - collect_split_edit_keys handles Condition::CorrelatedSubquery
  - advance_persistent_pipeline applies split-edit semantics for EXISTS parent_field
affects:
  - packages/zqlite-rs/src/advance.rs
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts
tech-stack:
  added: []
  patterns:
    - matches RustTableSource::maybe_split_edit shape
    - mirrors TS builder.ts:273-290 splitEditKeys gather
key-files:
  created:
    - packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts
  modified:
    - packages/zqlite-rs/src/advance.rs
    - packages/zqlite-rs/src/table_source.rs
decisions:
  - D-04 (CSQ arm + recurse into subquery.where_cond) implemented at advance.rs:868-883
  - D-05 (5 Rust unit tests) implemented at advance.rs::tests
  - D-06 (3 TS integration tests) implemented at pipeline-driver.exists-parent-edit.test.ts
  - D-07 (regression guard) implemented as test_collect_split_edit_keys_no_csq_unchanged + test_collect_split_edit_keys_includes_top_level_related
metrics:
  duration_min: 50
  completed: 2026-04-29
requirements:
  - AUDIT-02
---

# Phase 30 Plan 02: AUDIT-02 EXISTS parent_field in split_edit_keys Summary

**One-liner:** Closes Bug #2 from `IVM-PORT-AUDIT.md` end-to-end — `collect_split_edit_keys` now walks `Condition::CorrelatedSubquery` (and recurses into the subquery's own where), and `advance_persistent_pipeline` now consults `split_edit_keys` to split a source `Edit` into `Remove(old)+Add(new)` so downstream `ExistsOperator` reacts to membership transitions.

## What was done

### Task 1 — Rust collector + unit tests (TDD: RED then GREEN)

**File modified:** `packages/zqlite-rs/src/advance.rs`

The inner `collect_from_cond(cond, keys)` function inside `collect_split_edit_keys` previously matched only `Condition::And` and `Condition::Or`, falling through `_ => {}` for `Simple` and `CorrelatedSubquery`. As a result, when an EXISTS / NOT EXISTS lived in a where clause, its `parent_field` columns were never added to `split_edit_keys`.

The fix adds a new arm:

```rust
crate::ast_to_config::Condition::CorrelatedSubquery { related, .. } => {
    for f in &related.correlation.parent_field {
        keys.insert(f.clone());
    }
    if let Some(inner) = &related.subquery.where_cond {
        collect_from_cond(inner, keys);
    }
}
```

Located in `packages/zqlite-rs/src/advance.rs` at the inner `collect_from_cond` (within the body of the outer `collect_split_edit_keys` defined at line 879). The arm both inserts `parent_field` columns and recurses into the subquery's own where_cond so nested EXISTS are caught.

**TS parity reference:** `packages/zql/src/builder/builder.ts:273-290` via `gatherCorrelatedSubqueryQueryConditions`.

**Five new Rust unit tests** in `advance.rs::tests`:

| Test | Purpose |
| --- | --- |
| `test_collect_split_edit_keys_csq_in_where` | Top-level EXISTS in where → parent_field collected |
| `test_collect_split_edit_keys_csq_nested_in_and` | EXISTS inside an AND condition is reachable |
| `test_collect_split_edit_keys_csq_recurses_into_subquery_where` | Nested EXISTS inside outer subquery's where_cond is collected (proves D-04 recursion) |
| `test_collect_split_edit_keys_no_csq_unchanged` | D-07 regression guard: AST without any CSQ → empty keys |
| `test_collect_split_edit_keys_includes_top_level_related` | Top-level `ast.related[*].correlation.parent_field` still works |

All 5 pass under `cargo test --release test_collect_split_edit_keys`.

### Task 2 — TS integration test + advance-path wiring

**Files:**
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts` (new, 305 lines)
- `packages/zqlite-rs/src/advance.rs` (additional changes)

The Rust collector fix alone was insufficient to close AUDIT-02 end-to-end. While `RustTableSource::maybe_split_edit` already consults a connection's `split_edit_keys`, the persistent advance pipeline (`advance_persistent_pipeline`) builds `SourceChange::Edit` directly via `diff_change_to_source_changes` and pushes through the IVM operator chain — bypassing `RustTableSource::push` and therefore bypassing `maybe_split_edit`. The keys were collected but never consulted at runtime.

To close the gap:

1. `PipelineState` gained a private `split_edit_keys: Vec<String>` field, populated from `collect_split_edit_keys(&query.ast)` at `build_pipeline_state` time.
2. New helper `maybe_split_edit_for_advance(sc: SourceChange, split_edit_keys: &[String]) -> Vec<SourceChange>` mirrors `RustTableSource::maybe_split_edit`: when any column in `split_edit_keys` differs between an `Edit`'s `old_row` and `new_row`, the Edit is split into `Remove(old)+Add(new)`. Other variants pass through unchanged.
3. `advance_persistent_pipeline` flat-maps the helper across `diff_change_to_source_changes` output before pushing through the operator chain.

**TS integration tests (3):**

| Test | Verifies |
| --- | --- |
| `EXISTS parent_field edit emits Remove when membership lost` | Canonical Bug #2 regression: A→B (B has no children) emits Remove for parent (NOT Edit) |
| `EXISTS parent_field edit emits Add when membership gained` | Inverse direction: replicator-driven A→B then B→A emits Add for parent (NOT Edit) |
| `EXISTS parent_field edit emits Edit when membership preserved` | Regression guard: A→A2 (both have children) does not over-trigger; net effect remains consistent |

All 3 pass against the freshly-built native binary.

### Task 3 — Verification gate

| Check | Result |
| --- | --- |
| `cd packages/zqlite-rs && cargo test --release --lib` | 118 passed, 1 failed (pre-existing flaky benchmark — see Deferred below) |
| `cd packages/zero-ivm-rs && cargo test --release --lib` | 154 passed, 0 failed |
| `cd packages/zero-cache && npx vitest run "src/services/view-syncer/pipeline-driver"` (12 files) | 135 passed |
| `cd packages/zero-cache && FUZZ_NUM_RUNS=1000 npx vitest run src/services/view-syncer/fuzz-ivm.test.ts` | 6 passed, 1 todo |
| `git diff 220f0fc3b HEAD packages/zqlite-rs/src/advance.rs \| grep -E '^[+-]\s*pub '` | 0 (no public API change) |
| `git diff 220f0fc3b HEAD packages/zqlite-rs/src/advance.rs \| grep -c "encode_advance_result_buf"` | 0 (wire format untouched) |
| New `+fn` lines (all are new privates / new test fns) | `maybe_split_edit_for_advance`, 5 test fns, 4 helpers |

## Deviations from Plan

The plan's stated fix was the `collect_split_edit_keys` arm only. End-to-end verification revealed a related upstream gap that had to be closed for the integration test to actually exercise AUDIT-02's intended fix.

### Auto-fixed Issues

**1. [Rule 1 — Bug] split_edit_keys collected but never applied at advance path**
- **Found during:** Task 2 (TS integration test failed even after the Rust fix; debug log showed source emitted `Edit` not `Remove+Add`)
- **Issue:** `RustTableSource::maybe_split_edit` only fires when a caller invokes `source.push()`. The persistent advance pipeline builds `SourceChange::Edit` via `diff_change_to_source_changes` and pushes directly through the IVM operator chain, bypassing the source's split logic. The collected `split_edit_keys` were stored on the connection but never consulted at runtime in the persistent advance path.
- **Fix:** Added `maybe_split_edit_for_advance` helper, threaded `split_edit_keys` onto `PipelineState`, and applied the split in `advance_persistent_pipeline` before pushing. Mirrors the existing `maybe_split_edit` shape.
- **Files modified:** `packages/zqlite-rs/src/advance.rs`
- **Commit:** `b8939b9b1`

**2. [Rule 3 — Blocking issue] Pre-existing compile error in table_source.rs test block**
- **Found during:** Task 1 (RED phase — `cargo test` failed to compile)
- **Issue:** `push_epoch` field had been changed from `u64` to `Mutex<u64>` in production code, but `test_push_epoch_increments` still used `assert_eq!(src.push_epoch, 0)` which doesn't compile (no `==` for `Mutex<u64>`). This blocked **all** `cargo test --lib` runs in `zqlite-rs`, including the AUDIT-02 verification gate.
- **Fix:** Replaced `src.push_epoch` with `*src.push_epoch.lock().unwrap()` at the three assertion sites. Pre-existing baseline bug, unrelated to AUDIT-02 in scope, but a hard blocker for the verification gate.
- **Files modified:** `packages/zqlite-rs/src/table_source.rs`
- **Commit:** `aa6f0d7e2`

## Deferred Issues

**1. `bench_persistent_pipeline_sequential_vs_parallel` flaky assertion**
This benchmark in `advance.rs::tests` asserts that sequential and parallel runs produce equal change counts (32149 vs 40000 observed). The failure is **pre-existing and reproduces identically on the unmodified base commit `220f0fc3b`** (verified via `git stash` round-trip). It is NOT caused by AUDIT-02 work and falls outside the SCOPE BOUNDARY rule for auto-fixes. Tracked for a future plan or for AUDIT-03 cleanup.

## Authentication Gates

None — fully autonomous execution.

## TDD Gate Compliance

Plan-level TDD was followed for Task 1:
- **RED gate** (`f01fbdc63`): 5 new unit tests added; 3 fail (CSQ-handling), 2 pass (regression guards).
- **GREEN gate** (`d4b648927`): the `Condition::CorrelatedSubquery` arm makes all 5 pass.

Task 2 added a follow-up `feat` commit (`b8939b9b1`) wiring split_edit_keys into the advance path and adding 3 TS integration tests. The TS tests serve as the end-to-end RED→GREEN gate for the integration.

## Hard Constraints (preserved)

- **No public/exported NAPI signature changes** in `advance.rs` (verified by `git diff | grep '^[+-]\s*pub '` returns empty).
- **No wire-format changes** to `encode_advance_result_buf` / `decodeAdvanceResultBuf` (`git diff | grep encode_advance_result_buf` returns 0).
- **No change to existing function signatures** — only the internal `PipelineState` struct gained a private field (which is `pub(crate)` to the same crate where `build_pipeline_state` lives).

## Commits

| Hash | Type | Description |
| --- | --- | --- |
| `aa6f0d7e2` | fix | dereference Mutex<u64> in test_push_epoch_increments (Rule 3 unblock) |
| `f01fbdc63` | test | add failing tests for collect_split_edit_keys with EXISTS (RED) |
| `d4b648927` | fix | collect EXISTS parent_field into split_edit_keys (GREEN — Task 1) |
| `b8939b9b1` | feat | apply split_edit_keys at advance path + add TS integration test (Task 2) |

## References

- Audit: `.planning/IVM-PORT-AUDIT.md` § "Bug #2 — `EXISTS` parent_field missing from `split_edit_keys`"
- Context: `.planning/phases/30-audit-fixes/30-CONTEXT.md` decisions D-04 through D-07
- TS parity: `packages/zql/src/builder/builder.ts:273-290`
- Rust source modified: `packages/zqlite-rs/src/advance.rs` (`collect_split_edit_keys`, `PipelineState`, `maybe_split_edit_for_advance`, `advance_persistent_pipeline`)
- Out of scope (per plan): partition_key threading (Risk #1 in audit) — NOT promoted to Phase 30 requirements.
- Precondition for: Plan 03 (AUDIT-04) — that fix assumes parent_field doesn't change in Edit, which AUDIT-02 now enforces at the source level.

## Self-Check: PASSED

- Files exist:
  - `packages/zqlite-rs/src/advance.rs` (FOUND)
  - `packages/zqlite-rs/src/table_source.rs` (FOUND)
  - `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts` (FOUND)
- Commits exist (verified via `git log 220f0fc3b..HEAD`):
  - `aa6f0d7e2` (FOUND)
  - `f01fbdc63` (FOUND)
  - `d4b648927` (FOUND)
  - `b8939b9b1` (FOUND)
- All 5 new Rust unit tests pass under `cargo test --release test_collect_split_edit_keys` (5/5).
- All 3 new TS integration tests pass under `npx vitest run pipeline-driver.exists-parent-edit.test.ts` (3/3).
- All 12 existing pipeline-driver TS test files pass (135/135).
- All 154 zero-ivm-rs library tests pass.
- Hard-constraint diff checks confirm no public signatures changed and no wire-format edits.
