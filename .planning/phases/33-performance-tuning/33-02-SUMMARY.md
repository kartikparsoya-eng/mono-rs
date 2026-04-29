---
phase: 33-performance-tuning
plan: 02
subsystem: zero-ivm-rs
tags: [test-coverage, or-exists, ivm, rust, hardening]
requirements: [HARDEN-02]
requires: []
provides:
  - or_exists_op.rs::tests count ≥22 (achieved: 26)
  - exists_op.rs categorical parity for or_exists_op.rs
affects:
  - packages/zero-ivm-rs/src/or_exists_op.rs (tests block only)
tech_stack_added: []
tech_stack_patterns:
  - 'TDD additive test mirroring (RED→GREEN cycle skipped because impl already correct)'
  - 'Anti-hack guardrail D-28: production code byte-for-byte unchanged'
key_files_created: []
key_files_modified:
  - packages/zero-ivm-rs/src/or_exists_op.rs (lines 597-1167, tests-only additions)
decisions:
  - 'Builder-spec parity test simplified to op_type() smoke test — pipeline.rs::build_operator requires SQLite-backed sources, not pure-unit-testable'
  - 'Cache-clear test uses independent operators (not MutableMockInput) — simpler, deterministic, no Arc<Mutex<>> scaffolding needed'
  - 'Stopped at 26 tests (within 25-28 D-11 aspirational band) — additional categorical coverage would have been contrived'
metrics:
  duration: ~30 min
  completed: '2026-04-29'
  tasks_completed: 3
  tests_added: 21
  files_modified: 1
---

# Phase 33 Plan 02: HARDEN-02 OrExists Test Breadth Parity Summary

Closed the 4.4× test-breadth gap between `exists_op.rs` (22 tests) and `or_exists_op.rs` (was 5, now 26) by mirroring the categorical structure of `exists_op.rs::tests` and adding OR-specific multi-branch combination tests required by D-09.

This plan added tests ONLY (D-28 anti-hack guardrail). Production code in `or_exists_op.rs` (lines 1-446) is byte-for-byte identical to the pre-plan state — verified via `diff` of the first 446 lines against `HEAD~3`.

## Final Test Count

```
$ cargo test --release -p zero-ivm-rs --lib or_exists_op:: -- --list \
    | grep -c '^or_exists_op::tests::test_'
26
```

**26 tests** ≥ target 22 (parity with `exists_op.rs`), within the D-11 aspirational band of 25-28.

## Categorical Breakdown

| Category                      | Count | Notes                                                                |
| ----------------------------- | ----- | -------------------------------------------------------------------- |
| Fetch                         | 3     | filters_parents_without_children, or_predicate_short_circuits, with_parent_constraint |
| Parent push                   | 3     | add_blocked_when_no_branches_pass, add_passes_via_or_predicate, remove_emits_when_was_passing |
| Child push                    | 5     | add_0_to_1, remove_1_to_0, different_relationship_passthrough, child_edit_passthrough, add_other_branch_already_passing |
| Edit, no or_predicate         | 2     | count_change_no_predicate, no_or_predicate_passes_when_children_exist |
| Edit, with or_predicate (4 transitions, pre-existing) | 4 | edit_both_pass, edit_old_only, edit_new_only, edit_neither (Phase 30-03) |
| Cache invalidation            | 2     | cache_cleared_on_fetch, child_add_without_prior_fetch_recomputes     |
| In-push re-entrancy           | 2     | reentrancy_panics (pre-existing, Phase 30-04), in_push_flag_cleared_after_push |
| OR-branch combinations (D-09) | 4     | two_branches_both_pass, two_branches_one_passes, two_branches_neither_passes, mixed_exists_not_exists_branches |
| Builder-spec parity (D-09)    | 1     | op_type_smoke (simplified — see Deviations below)                    |
| **TOTAL**                     | **26** |                                                                     |

## Test Names Added (21 new)

```
test_or_exists_fetch_filters_parents_without_children
test_or_exists_fetch_or_predicate_short_circuits
test_or_exists_fetch_with_parent_constraint
test_or_exists_push_parent_add_blocked_when_no_branches_pass
test_or_exists_push_parent_add_passes_via_or_predicate
test_or_exists_push_parent_remove_emits_when_was_passing
test_or_exists_push_child_add_0_to_1_transition
test_or_exists_push_child_remove_1_to_0_transition
test_or_exists_push_different_relationship_child_passthrough
test_or_exists_push_child_edit_passthrough
test_or_exists_push_child_add_other_branch_already_passing
test_or_exists_edit_count_change_no_predicate
test_or_exists_edit_no_or_predicate_passes_when_children_exist
test_or_exists_cache_cleared_on_fetch
test_or_exists_child_add_without_prior_fetch_recomputes
test_or_exists_in_push_flag_cleared_after_push
test_or_exists_two_branches_both_pass
test_or_exists_two_branches_one_passes
test_or_exists_two_branches_neither_passes
test_or_exists_mixed_exists_not_exists_branches
test_or_exists_op_type_smoke
```

## Pre-existing Tests (5, all still pass unchanged)

- `test_or_exists_reentrancy_panics` (Phase 30-04, AUDIT-03)
- `test_or_exists_edit_both_pass` (Phase 30-03, AUDIT-04)
- `test_or_exists_edit_old_only` (Phase 30-03, AUDIT-04)
- `test_or_exists_edit_new_only` (Phase 30-03, AUDIT-04)
- `test_or_exists_edit_neither` (Phase 30-03, AUDIT-04)

## Helpers Added

Inside `mod tests` only (no production-code reuse outside `#[cfg(test)]`):

- `fn make_node_with_parent(id: i64, parent_id: i64) -> Node` — mirror of `exists_op.rs:431-439`.
- `fn build_or_exists_simple_branch(parents, children) -> OrExistsOperator` — single-branch fixture, no or_predicate.
- `fn build_or_exists_two_branches(parents, ch_a, ch_b) -> OrExistsOperator` — two-branch fixture for OR-combination tests.

These compose with the existing helpers `make_node`, `make_status_edit`, `build_or_exists_with_status_predicate` (added in Phase 30-03).

## Verification

```
$ cargo test --release -p zero-ivm-rs --lib or_exists_op::tests -- --test-threads=1
running 26 tests
... (all 26 listed individually)
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 165 filtered out

$ cargo test --release -p zero-ivm-rs --lib -- --test-threads=1
test result: ok. 191 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Full crate suite passes 191/191 (was 170 baseline; +21 new or_exists tests = 191). Zero regressions in any other test module.

## D-28 Anti-Hack Guardrail Verification

```
$ git show HEAD~3:packages/zero-ivm-rs/src/or_exists_op.rs | head -446 > /tmp/before.txt
$ head -446 packages/zero-ivm-rs/src/or_exists_op.rs > /tmp/after.txt
$ diff /tmp/before.txt /tmp/after.txt
(no output — files identical)
```

```
$ git diff HEAD~3 packages/zero-ivm-rs/src/or_exists_op.rs --stat
 packages/zero-ivm-rs/src/or_exists_op.rs | 571 ++++++++++++++++++++++++++++++
 1 file changed, 571 insertions(+)
$ git diff HEAD~3 packages/zero-ivm-rs/src/or_exists_op.rs | grep -c "^-[^-]"
0
```

Zero deletions. All 571 added lines are inside the `mod tests` block (lines 597-1167). Production code (lines 1-446) is byte-for-byte unchanged.

## Deviations from Plan

### Auto-decisions / scope adjustments (D-11 escape clause)

**1. [Plan §<behavior> simplification — Builder-spec parity test]**

- **Found during:** Task 3.
- **Issue:** The plan suggested calling `ast_to_config::build_pipeline_state` to verify the OrExists construction path matches direct `OrExistsOperator::new()`. Investigation showed there is no `ast_to_config.rs` module; the construction path lives in `pipeline.rs::build_operator`, which requires SQLite-backed source operators (not callable from a pure unit test without standing up a real SQLite database).
- **Fix:** Took the simplification explicitly authorized by the plan's `<behavior>` block: implemented `test_or_exists_op_type_smoke` that asserts `op.op_type() == "or_exists"` and that fetch + push roundtrip on a direct construction doesn't panic. This covers the canonical contract that downstream pipeline-driver code relies on.
- **Files modified:** `packages/zero-ivm-rs/src/or_exists_op.rs` (test addition only).
- **Commit:** `cd5122205`.

**2. [Plan §<action> simplification — Cache invalidation test]**

- **Found during:** Task 2.
- **Issue:** The plan suggested either a `MutableMockInput { nodes: Arc<Mutex<...>> }` helper to mutate `MockInput.nodes` between fetches OR using two separate operators with different child sets to verify cache rebuild. The plan said "prefer the second (simpler) approach."
- **Fix:** Took the simpler approach. `test_or_exists_cache_cleared_on_fetch` uses two independent operators (one with 1 child, one with 0 children) to demonstrate that cached state in operator A does not leak into operator B's fetch. Then exercises a same-operator double-fetch on B to confirm cache rebuild produces the same empty result.
- **Files modified:** None additional (inside the same plan-scoped file).
- **Commit:** `540db1238`.

**3. [Plan target — final count]**

- **Aspirational target was 30; D-11 escape clause allowed 25-28.** Final count: 26. We implemented every meaningful category in the plan plus the OR-specific tests required by D-09. Reaching 30 would have required either (a) duplicating tests across `make_node_with_parent` variants without new semantic value or (b) re-creating exists_op.rs::test_not_exists_push_child_add_0_to_1_transition family for or_exists, which would test EXACTLY the same ExistsOperator-not-exists semantics already covered by the underlying branch's own behavior. Per D-11 we stopped at 26 (within the 25-28 band) rather than pad.

### Authentication gates encountered

None — pure Rust unit test additions.

## Threat Flags

None. The threat model in the plan (T-33-02-01 tampering / T-33-02-02 information disclosure) was honored:

- T-33-02-01 (Tampering / D-28): Verified by direct file diff — production lines 1-446 byte-for-byte identical to pre-plan.
- T-33-02-02 (Information Disclosure): No new test-only public API exposed. Only the pre-existing `force_in_push_for_test` helper (Phase 30-04) was used.

## Commits

- `f06fb6f3e` — `test(33-02): add fetch + parent + child push tests for OrExists` (Task 1, +318 lines, 11 new tests)
- `540db1238` — `test(33-02): add edit-no-predicate, cache, in-push tests for OrExists` (Task 2, +145 lines, 5 new tests)
- `cd5122205` — `test(33-02): add OR-branch combination + builder-spec parity tests` (Task 3, +108 lines, 5 new tests)

Total: 3 commits, +571 lines, 21 new tests added (5 → 26).

## Self-Check: PASSED

- `packages/zero-ivm-rs/src/or_exists_op.rs` modified: FOUND (1167 lines, was 596).
- Commits exist:
  - `f06fb6f3e` FOUND
  - `540db1238` FOUND
  - `cd5122205` FOUND
- Full crate test suite green: `191 passed; 0 failed`.
- D-28 byte-for-byte production unchanged: VERIFIED.
- Test count ≥22: VERIFIED (26 ≥ 22, within 25-28 D-11 band).
