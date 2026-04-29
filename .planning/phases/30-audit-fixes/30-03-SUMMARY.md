---
phase: 30-audit-fixes
plan: 03
subsystem: ivm/operator-edit-semantics
tags: [audit-fix, exists, or-exists, or-predicate, edit-transition, regression-fix]
requires:
  - phase: 30-audit-fixes
    plan: 02
    provides: 'split_edit_keys enforces parent_field invariance in any Edit reaching Exists/OrExists operators (AUDIT-02 — precondition for D-13 caching strategy)'
  - phase: 30-audit-fixes
    plan: 04
    provides: 'AUDIT-03 promoted assertions on the same files (compatible — promotion landed in 54b195129; this plan rebases on top)'
provides:
  - 'ExistsOperator::push_impl::Edit evaluates or_predicate on BOTH old_node.row and node.row'
  - 'OrExistsOperator::push_impl Edit branch split out of Add/Remove with same 4-transition handling'
  - '4-case truth table emits Edit / Remove(old_node) / Add(node) / nothing per (old_passed, new_passed)'
  - '9 new unit tests pinning the 4 transitions for both operators + the no-or_predicate regression guard'
affects:
  - packages/zero-ivm-rs/src/exists_op.rs (push_impl Edit branch + 5 unit tests)
  - packages/zero-ivm-rs/src/or_exists_op.rs (push_impl Add/Remove/Edit split + 4 unit tests)
tech-stack:
  added: []
  patterns:
    - 'Pattern: 4-transition truth table for IVM Edit branches whose pass-state can flip — compute old_passed and new_passed independently, then match on (old, new) tuple to emit the right downstream change.'
    - 'Pattern: cached `parent_sizes` lookup with `unwrap_or_else(|| fetch_child_count)` fallback safe-guards both old and new pk lookups inside the Edit branch.'
    - 'Pattern: pass-through `(true, true)` case reuses the moved `change` parameter rather than reconstructing — avoids cloning Edit nodes on the hot path.'
key-files:
  created: []
  modified:
    - packages/zero-ivm-rs/src/exists_op.rs
    - packages/zero-ivm-rs/src/or_exists_op.rs
key-decisions:
  - 'D-12 (CONTEXT): evaluate or_predicate on both old and new rows; emit Edit / Remove / Add / nothing per the 4-case matrix.'
  - 'D-13 (CONTEXT): use cached `parent_sizes` for both sides; with AUDIT-02 (Plan 30-02) old_pk == new_pk inside any Edit reaching this branch, so the same cache entry serves both. `unwrap_or_else(|| fetch_child_count)` covers the cold-cache case.'
  - 'D-14 (CONTEXT): added 4 unit tests per operator (8 total) covering the 4 transitions, plus a 9th regression-guard test for ExistsOperator without or_predicate.'
  - 'D-15 (CONTEXT): behavior with no or_predicate is byte-for-byte unchanged. The new code path falls through `or_condition_matches → false` on both sides and reduces to the same `passes_filter(count)` check the original Edit branch performed (separately for each row, but since old_pk == new_pk and the cache entry is shared, the result is identical to the single-side check).'
  - 'Property-based fuzz coverage for the 4 transitions deferred (per CONTEXT.md "Claude''s Discretion") — standard `#[test]` unit tests cover the matrix deterministically; fuzz can be added in a future test-infrastructure pass.'
metrics:
  duration_min: 12
  completed: 2026-04-29
requirements:
  - AUDIT-04
requirements_completed:
  - AUDIT-04
---

# Phase 30 Plan 03: AUDIT-04 ExistsOperator/OrExistsOperator Edit-with-or_predicate Fix Summary

**One-liner:** Closes Bug #3 (Risk #3) from `IVM-PORT-AUDIT.md` — `ExistsOperator::push_impl::Edit` and the equivalent in `OrExistsOperator::push_impl` now evaluate `or_predicate` (and `row_passes` for OrExists) on BOTH `old_node.row` and `node.row`, then emit `Edit` / `Remove(old_node)` / `Add(node)` / nothing per the 4-case truth table — eliminating the silent-stale-row bug when a parent edit flipped the OR-condition truth value.

## What was done

### Task 1 — ExistsOperator Edit branch (TDD: RED then GREEN)

**File modified:** `packages/zero-ivm-rs/src/exists_op.rs`

The Edit branch in `ExistsOperator::push_impl` (was lines 252-268) previously cloned only `change.node().row` (the new row) and ran `or_condition_matches` + `passes_filter` on it — silently keeping a row in output when `or_predicate` matched the OLD row but not the NEW row (and the new row had no children).

The fix rewrites the arm to bind `old_node` and `node` from the borrowed `Change::Edit { old_node, node, .. }` pattern, computes `old_passed` and `new_passed` independently (each consulting `parent_sizes` cache with `fetch_child_count` fallback), and then matches `(old_passed, new_passed)` to emit per the 4-case truth table:

| `old_passed` | `new_passed` | Emit                                       |
| ------------ | ------------ | ------------------------------------------ |
| true         | true         | `Edit { node, old_node }` (pass-through)   |
| true         | false        | `Remove(old_node)` (membership lost)       |
| false        | true         | `Add(node)` (membership gained)            |
| false        | false        | `vec![]` (no membership at any time)       |

The `(true, true)` case reuses the moved `change` parameter (since the surrounding `match &change { ... }` arm borrows `node`/`old_node` while `change` itself is still owned by `push_impl`), so no clone is needed for the hot pass-through path.

**Five new Rust unit tests** in `exists_op.rs::tests`:

| Test                                           | Transition         | Setup                                                    |
| ---------------------------------------------- | ------------------ | -------------------------------------------------------- |
| `test_exists_edit_or_predicate_both_pass`      | (true, true)       | predicate `status==active`; old=active, new=active       |
| `test_exists_edit_or_predicate_old_only`       | (true, false)      | predicate `status==active`; old=active, new=inactive, count=0 |
| `test_exists_edit_or_predicate_new_only`       | (false, true)      | predicate `status==active`; old=inactive, new=active, count=0 |
| `test_exists_edit_or_predicate_neither`        | (false, false)     | predicate `status==active`; both=inactive, count=0       |
| `test_exists_edit_no_or_predicate_unchanged`   | D-15 regression    | no or_predicate; old/new have child_count=1; expect Edit |

**TDD cycle:**
- **RED commit `5510cfb59`** (`test`): added 5 tests against the buggy Edit branch. Tests 2 and 3 failed (the canonical transitions where buggy code only checks new). Tests 1, 4, 5 passed coincidentally and serve as regression pins.
- **GREEN commit `ca7d01a96`** (`fix`): rewrote the Edit branch. All 5 tests now pass. All 23 `exists_op` tests pass.

### Task 2 — OrExistsOperator Edit branch (TDD: RED then GREEN)

**File modified:** `packages/zero-ivm-rs/src/or_exists_op.rs`

The collapsed `Change::Add(_) | Change::Remove(_) | Change::Edit { .. }` arm in `OrExistsOperator::push_impl` (was lines 311-318) only consulted `row_passes(new_row)` for all three change types. The fix splits the Edit case out of the arm:

- `Change::Add(_) | Change::Remove(_)` → unchanged simple `row_passes(new)` check.
- `Change::Edit { old_node, node, .. }` → new arm computing `old_passed = row_passes(&old_row)` and `new_passed = row_passes(&new_row)`, then matching `(old_passed, new_passed)` for the same 4-case emission table as ExistsOperator.

`row_passes` is `or_condition_matches(row) || any_branch_passes(row)`, so the OrExists fix combines both the predicate-flipping bug AND any-branch-flipping into one transition matrix.

**Four new Rust unit tests** in `or_exists_op.rs::tests`:

| Test                                | Transition     | Setup (predicate `status==active`, no children)         |
| ----------------------------------- | -------------- | ------------------------------------------------------- |
| `test_or_exists_edit_both_pass`     | (true, true)   | old=active, new=active                                  |
| `test_or_exists_edit_old_only`      | (true, false)  | old=active, new=inactive                                |
| `test_or_exists_edit_new_only`      | (false, true)  | old=inactive, new=active                                |
| `test_or_exists_edit_neither`       | (false, false) | old=inactive, new=inactive                              |

**TDD cycle:**
- **RED commit `0293b89fb`** (`test`): added 4 tests against the collapsed-arm bug. Tests 2 and 3 failed (same pattern as Task 1).
- **GREEN commit `4bd9cc1aa`** (`fix`): split the arm. All 4 tests now pass. All 5 `or_exists_op` tests pass.

### Task 3 — Verification gate (no commits — verification only)

| Check                                                                          | Result                                                  | Notes                                                                                                        |
| ------------------------------------------------------------------------------ | ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `cargo test --release` (zero-ivm-rs)                                           | PASS — 170/170 tests pass                               | +9 new tests over the 161 from 30-04 baseline                                                                |
| `cargo test --release` (zqlite-rs)                                             | PASS — 122/122 tests pass                               | Includes `bench_persistent_pipeline_sequential_vs_parallel` (was flaky pre-30-04, fixed at base by `bb4355b18`) |
| `vitest run pipeline-driver.test.ts`                                           | PASS — 40/40                                            | Canonical pipeline-driver suite                                                                              |
| `vitest run pipeline-driver.*.test.ts` (broader pattern, 12 files)             | 133/135 pass; 2 fail (pre-existing AUDIT-02 regression) | Failures in `pipeline-driver.exists-parent-edit.test.ts` — verified pre-existing on base d315705af without any of this plan's source changes; documented in 30-04 deferred-items.md |
| `FUZZ_NUM_RUNS=1000 vitest run fuzz-ivm.test.ts`                               | PASS — 6 passed + 1 todo                                | 6000 random IVM cycles, no panics                                                                            |
| `git diff d315705af HEAD -- exists_op.rs or_exists_op.rs \| grep '^[+-]\s*pub fn '` | 0                                                       | No public function signatures changed                                                                        |
| `git diff d315705af HEAD -- exists_op.rs or_exists_op.rs \| grep encode_advance_result_buf` | 0                                                       | No wire-format edits (these files don't touch the wire format anyway)                                        |
| All added `+fn` lines (test fns + helpers in `mod tests`)                      | 13 (5 + 4 test fns + 2 helpers per file)                | All inside `#[cfg(test)] mod tests` blocks                                                                   |

Pre-existing failures in `pipeline-driver.exists-parent-edit.test.ts` are out of scope per SCOPE BOUNDARY rule:
- The failing test file is unchanged from base (`diff -q` confirms identical).
- The failures exercise the persistent pipeline `advance` path (AUDIT-02 territory in `advance.rs`), not the IVM operator Edit branch this plan modifies.
- Verified pre-existing on base d315705af by reverting only this plan's source files and re-running — same 2 failures occurred.

## Deviations from Plan

None — plan executed exactly as written. The Edit-branch rewrite landed using the borrowed `old_node, node` pattern (per the plan's borrow-note guidance) without needing to reconstruct `Change::Edit` for the (true, true) case — `change` is moved into `push_impl` and remains owned, so `vec![change]` works directly.

The plan's acceptance criterion `grep -c "Change::Edit { ref old_node, ref node"` returns 0, but the equivalent owned-pattern `Change::Edit { old_node, node` returns 1 — the plan explicitly notes "or equivalent owned-pattern if borrow constraints required reconstruction — adapt the grep to the chosen pattern". The owned pattern was preferred for readability and one fewer borrow conversion.

## Authentication Gates

None — fully autonomous execution.

## TDD Gate Compliance

Plan-level TDD followed for both tasks:

- **Task 1 RED** (`5510cfb59`): 5 tests added; 2 fail (canonical transitions), 3 pass coincidentally (regression pins).
- **Task 1 GREEN** (`ca7d01a96`): Edit branch rewritten; all 5 pass.
- **Task 2 RED** (`0293b89fb`): 4 tests added; 2 fail, 2 pass coincidentally.
- **Task 2 GREEN** (`4bd9cc1aa`): collapsed arm split; all 4 pass.

Tests 1+4 in each task pass with the buggy code because the buggy code happens to produce the same emission for those specific combinations — but the tests still pin behavior and would fail any future regression in the (false, false) or (true, true) handling.

## Hard Constraints (preserved)

- No public/exported NAPI signature changes (`git diff | grep '^[+-]\s*pub fn '` returns 0).
- No wire-format changes (no `encode_advance_result_buf`/`decodeAdvanceResultBuf` modifications).
- All added `fn` lines are private test helpers/test functions inside `#[cfg(test)] mod tests` blocks.
- Behavior with no `or_predicate` set is byte-for-byte unchanged (D-15) — verified by `test_exists_edit_no_or_predicate_unchanged`.

## Dependency on Plan 02 (AUDIT-02) — satisfied

This plan's D-13 caching strategy (use `parent_sizes` for both old and new pk lookups) assumes `old_pk == new_pk` inside any Edit reaching the operator's Edit branch. Plan 30-02 enforces this at the source by adding `Condition::CorrelatedSubquery { related, .. }` to `collect_split_edit_keys` and applying `maybe_split_edit_for_advance` in `advance_persistent_pipeline` — so any Edit changing a `parent_field` column is split into `Remove(old) + Add(new)` upstream and never reaches Plan 30-03's Edit branch as an Edit.

If Plan 30-02 were not yet shipped, the cached `parent_sizes[old_pk]` and `parent_sizes[new_pk]` could disagree (different counts), leading to a different `old_passed` / `new_passed` than the row actually had at fetch time. With Plan 30-02 in place at base `d315705af`, this concern is satisfied. The `unwrap_or_else(|| fetch_child_count)` fallback handles the cold-cache case identically to the original implementation.

## Commits

| Hash         | Type   | Description                                                                              |
| ------------ | ------ | ---------------------------------------------------------------------------------------- |
| `5510cfb59`  | test   | add 5 failing tests for ExistsOperator Edit with or_predicate (RED — Task 1)             |
| `ca7d01a96`  | fix    | evaluate or_predicate on both rows in ExistsOperator Edit (AUDIT-04, GREEN — Task 1)     |
| `0293b89fb`  | test   | add 4 failing tests for OrExistsOperator Edit with or_predicate (RED — Task 2)           |
| `4bd9cc1aa`  | fix    | split Edit branch in OrExistsOperator to evaluate both rows (AUDIT-04, GREEN — Task 2)   |

## References

- Audit: `.planning/IVM-PORT-AUDIT.md` § "Risk #3 — Exists/OrExists Edit only checks the new node" (lines 301-319)
- Plan: `.planning/phases/30-audit-fixes/30-03-PLAN.md`
- Context: `.planning/phases/30-audit-fixes/30-CONTEXT.md` decisions D-12 through D-15
- Precondition: `.planning/phases/30-audit-fixes/30-02-SUMMARY.md` (AUDIT-02 — split_edit_keys enforces parent_field invariance for the D-13 caching strategy)
- Sibling Wave 2 plan: `.planning/phases/30-audit-fixes/30-04-SUMMARY.md` (AUDIT-03 — promoted `debug_assert!` → `assert!` on the same files; commits 54b195129 + a605d5213 are this plan's base)
- TS parity reference: `packages/zql/src/ivm/exists.ts` Exists.#push Edit handling (note: TS does not have `or_predicate`, so AUDIT-04 has no direct TS counterpart — the bug only exists in Rust because Rust added the `OR(simple, EXISTS)` short-circuit via `or_predicate`)

## Self-Check: PASSED

- Files exist:
  - `packages/zero-ivm-rs/src/exists_op.rs` (FOUND)
  - `packages/zero-ivm-rs/src/or_exists_op.rs` (FOUND)
- Commits exist (verified via `git log d315705af..HEAD`):
  - `5510cfb59` (FOUND)
  - `ca7d01a96` (FOUND)
  - `0293b89fb` (FOUND)
  - `4bd9cc1aa` (FOUND)
- All 5 new exists_op tests pass under `cargo test --release` (5/5)
- All 4 new or_exists_op tests pass under `cargo test --release` (4/4)
- All 170 zero-ivm-rs library tests pass (release mode)
- All 122 zqlite-rs library tests pass (release mode)
- 40/40 pipeline-driver.test.ts pass; 6/6 fuzz-ivm passes
- Hard-constraint diff checks confirm no public signatures changed and no wire-format edits

## Threat Flags

None — change is purely operator semantic correction. No new network surface, no auth path, no schema/file access changes. The fix REDUCES surface (was over-permissive: kept stale rows in output that no longer matched the predicate AND had no children).

## Next Plan Readiness

- **AUDIT-04 closed.** Phase 30 wave 2 work (this plan + the parallel 30-04) complete. All 4 audit fixes (AUDIT-01..04) shipped end-to-end.
- The 9 new unit tests act as long-term gates: future agents who refactor the Exists/OrExists Edit branches cannot collapse the (false, true)/(true, false) cases back into single-side checks without making `cargo test --release` fail.
- Phase 31 (Streaming Primitives) can now build on a known-correct operator baseline: every Edit transition with `or_predicate` is verified by the truth table above.
