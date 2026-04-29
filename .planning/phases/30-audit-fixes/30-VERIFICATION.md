---
phase: 30-audit-fixes
verified: 2026-04-29T09:04:36Z
status: gaps_found
score: 16/18 must-haves verified
overrides_applied: 0
gaps:
  - truth: 'Editing a column that is an EXISTS parent_field causes the table source to emit Remove + Add (not Edit) — verified end-to-end via TS integration test'
    status: failed
    reason: "The Rust-side fix (collect_split_edit_keys + maybe_split_edit_for_advance) is present in source, but the TS integration test in pipeline-driver.exists-parent-edit.test.ts that exercises the production-path persistent advance pipeline still fails to observe Remove+Add output. Two of three integration tests fail: 'EXISTS parent_field edit emits Remove when membership lost' (expected length 1, got 0) and 'EXISTS parent_field edit emits Add when membership gained' (cascade failure from same root cause). Phase Success Criteria #2 explicitly requires this end-to-end behavior; phase goal requires 'a known-correct operator baseline' for Phase 31."
    artifacts:
      - path: packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts
        issue: 'Tests at lines 197 and 227 fail. The Rust collector now includes Condition::CorrelatedSubquery (verified at advance.rs:903), and maybe_split_edit_for_advance is called in advance_persistent_pipeline (verified at advance.rs:1271), but the diff reaching the operator still arrives as Edit, not Remove+Add — meaning either split_edit_keys is empty at runtime for this query shape, or the diff_change_to_source_changes path produces a SourceChange variant that maybe_split_edit_for_advance does not split.'
    missing:
      - 'Trace why split_edit_keys is empty (or why maybe_split_edit_for_advance is bypassed) for the AUDIT-02 test query (PARENTS_WITH_MATCHING_CHILD with EXISTS in WHERE)'
      - 'Either fix the runtime wiring so split_edit_keys is consulted on the path the test exercises, or correct the test fixture if the production AST differs from the unit-tested AST shape'
      - 'Re-run pipeline-driver.exists-parent-edit.test.ts and confirm 3/3 pass'
  - truth: 'Downstream ExistsOperator produces correct Remove (or Add) when the membership transition flips on edit — verified end-to-end'
    status: failed
    reason: "Same root cause as above: the operator never receives Remove+Add because the upstream split is not happening on the production path. The Rust unit tests at the operator level (test_exists_edit_or_predicate_*) pass — confirming AUDIT-04's operator-level fix is correct in isolation — but the AUDIT-02 → AUDIT-04 chain is broken at the AUDIT-02 hand-off."
    artifacts:
      - path: packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts
        issue: 'Same two failing tests cover this truth (Remove emitted when membership lost; Add when gained). Both fail at the same assertion sites.'
    missing:
      - 'Resolve the upstream gap above; once Remove+Add reaches the operator, the operator-level fix from AUDIT-04 should drop the synthetic Add (B has no children) producing the net Remove output the test expects'
human_verification: []
---

# Phase 30: Audit Fixes Verification Report

**Phase Goal:** Ship the 4 fixes from the IVM port audit so the streaming work in Phase 31 builds on a known-correct operator baseline. These are independent of streaming, low-risk, and unblock the rest of the milestone.

**Verified:** 2026-04-29T09:04:36Z
**Status:** gaps_found
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths

#### AUDIT-01 (Plan 30-01)

| #   | Truth                                                                                | Status   | Evidence                                                                                                                                                                             |
| --- | ------------------------------------------------------------------------------------ | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | LIKE 'Foo%' filter is case-sensitive in Rust IVM (does NOT match 'foo bar' rows)     | VERIFIED | hydrate.rs:769 reads `Predicate::Like(field, pattern, false)` for the `like` arm; test_parse_predicate_like_case_sensitive at line 1792 confirms this; cargo test --release: PASS    |
| 2   | ILIKE 'foo%' filter is still case-insensitive (matches both 'Foo bar' and 'foo bar') | VERIFIED | hydrate.rs:773 reads `Predicate::Like(field, pattern, true)` for the `ilike` arm; test_parse_predicate_ilike_case_insensitive at line 1829 confirms this; cargo test --release: PASS |
| 3   | Regression test in hydrate.rs distinguishes LIKE from ILIKE on the same input rows   | VERIFIED | test_parse_predicate_like_distinguishes_from_ilike at line 1866; cargo test --release: PASS                                                                                          |
| 4   | Existing parse_predicate (pipeline.rs) and ILIKE behavior remain unchanged           | VERIFIED | grep shows pipeline.rs unchanged; ilike arm at hydrate.rs:773 byte-for-byte preserved (still `true`)                                                                                 |

#### AUDIT-02 (Plan 30-02)

| #   | Truth                                                                                                                         | Status   | Evidence                                                                                                                                                                                                                                                                                               |
| --- | ----------------------------------------------------------------------------------------------------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 5   | collect_split_edit_keys recurses into Condition::CorrelatedSubquery and adds its parent_field columns to the keys set         | VERIFIED | advance.rs:903 adds `Condition::CorrelatedSubquery { related, .. }` arm; advance.rs:907 recurses into `related.subquery.where_cond`; 5 unit tests test*collect_split_edit_keys*\* pass under cargo test --release                                                                                      |
| 6   | Editing a column that is an EXISTS parent_field causes the table source to emit Remove + Add (not Edit) — verified end-to-end | FAILED   | Rust unit tests pass; TS integration tests at pipeline-driver.exists-parent-edit.test.ts:197 fails: `expected [] to have a length of 1 but got +0`. The Rust source-level fix (advance.rs:1271) calls maybe_split_edit_for_advance, but the test does not observe Remove+Add output. See gaps section. |
| 7   | Downstream ExistsOperator produces correct Remove (or Add) when the membership transition flips on edit — verified end-to-end | FAILED   | Same root cause as #6. Tests `EXISTS parent_field edit emits Remove when membership lost` and `EXISTS parent_field edit emits Add when membership gained` both fail. The third test (`emits Edit when membership preserved`) passes.                                                                   |
| 8   | Behavior for queries WITHOUT EXISTS in the where clause is unchanged                                                          | VERIFIED | test_collect_split_edit_keys_no_csq_unchanged passes; main pipeline-driver.test.ts (40/40 tests) pass; cargo test --release for both crates passes (no regressions in other suites)                                                                                                                    |

#### AUDIT-04 (Plan 30-03)

| #   | Truth                                                                                                                                        | Status   | Evidence                                                                                                                                                                                                                                                                     |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 9   | ExistsOperator Edit with or_predicate evaluates the predicate on BOTH old_node.row and node.row                                              | VERIFIED | exists_op.rs:262 `or_condition_matches(&old_row)` and exists_op.rs:279 `or_condition_matches(&new_row)`; 4-case match at line 295                                                                                                                                            |
| 10  | Both pass: emit Edit (current behavior preserved); old_passed only: emit Remove(old_node); new_passed only: emit Add(node); Neither: nothing | VERIFIED | exists*op.rs:295-305 implements the (true,true)→Edit, (true,false)→Remove, (false,true)→Add, (false,false)→nothing matrix; tests test_exists_edit_or_predicate*{both_pass,old_only,new_only,neither} pass; or_exists_op.rs:327-336 has the same shape, 4 OrExists tests pass |
| 11  | Behavior for ExistsOperator without or_predicate is byte-for-byte unchanged                                                                  | VERIFIED | test_exists_edit_no_or_predicate_unchanged at exists_op.rs:1094 passes; the (true,true) pass-through reuses owned `change` so no semantic change                                                                                                                             |
| 12  | OrExistsOperator Edit case applies the same 4-transition logic combining or_predicate AND any_branch_passes                                  | VERIFIED | or_exists_op.rs:325-326 `row_passes(&old_row)` and `row_passes(&new_row)`; row_passes = or_condition_matches OR any_branch_passes (or_exists_op.rs:164-166); 4 transition tests pass                                                                                         |

#### AUDIT-03 (Plan 30-04)

| #   | Truth                                                                                                                                   | Status   | Evidence                                                                                                                                                                                                                                                   |
| --- | --------------------------------------------------------------------------------------------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 13  | Framework-invariant violations panic loudly in release builds (cargo test --release runs the panic tests)                               | VERIFIED | All 7 #[should_panic] tests pass under cargo test --release (170 tests total in zero-ivm-rs)                                                                                                                                                               |
| 14  | Join parent edit that changes the relationship key panics with 'Parent edit must not change relationship.'                              | VERIFIED | join_op.rs:221 `assert!(!self.join_key_changed(...), "Parent edit must not change relationship.")`; test_join_parent_edit_changing_relationship_panics at line 828 with #[should_panic(expected = ...)]                                                    |
| 15  | Join child edit that changes the relationship key panics with 'Child edit must not change relationship.'                                | VERIFIED | join_op.rs:266 `assert!(!self.child_key_changed(...), "Child edit must not change relationship.")`; test_join_child_edit_changing_relationship_panics at line 848                                                                                          |
| 16  | Exists re-entrancy (push during push) panics with 'Unexpected re-entrancy'                                                              | VERIFIED | exists_op.rs:179 `assert!(!self.in_push, "Unexpected re-entrancy")`; or_exists_op.rs:234 same (parity per D-09); test_exists_reentrancy_panics + test_or_exists_reentrancy_panics both pass                                                                |
| 17  | Take edit producing duplicate primary key panics with 'Invalid state. Row has duplicate primary key' (both line-200 and line-246 sites) | VERIFIED | take_op.rs:201 and take_op.rs:248 both promoted to `assert!(new_cmp != Equal, "Invalid state. Row has duplicate primary key")`; test_take_duplicate_pk_in_inside_outside_branch_panics + test_take_duplicate_pk_in_outside_outside_branch_panics both pass |
| 18  | Cap edit changing partition key panics with 'Cap: partition key must not change on edit'                                                | VERIFIED | cap_op.rs:167-170 `assert_eq!(old_part_key, new_part_key, "Cap: partition key must not change on edit")`; test_cap_partition_key_change_on_edit_panics at line 480                                                                                         |
| 19  | Non-framework debug_assert! occurrences in the same files are NOT promoted (only the 5 specific framework-invariant sites)              | VERIFIED | grep "^.\*debug_assert" on the 5 files returns only comment references (mentioning the historical `debug_assert!` name) — no actual debug_assert! macro calls remain at the promoted sites; D-09 scope discipline preserved                                |

**Score:** 16/18 truths verified (truths #6 and #7 FAILED — AUDIT-02 end-to-end gap)

### Required Artifacts

| Artifact                                                                                  | Expected                                                      | Status           | Details                                                                                                                                                                                                                                |
| ----------------------------------------------------------------------------------------- | ------------------------------------------------------------- | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `packages/zqlite-rs/src/hydrate.rs`                                                       | `parse_predicate_json` with correct case-sensitive LIKE       | VERIFIED         | Contains `Predicate::Like(field, pattern, false)` at line 769 (the `like` arm); ilike arm preserved at line 773                                                                                                                        |
| `packages/zqlite-rs/src/advance.rs`                                                       | `collect_split_edit_keys` with full Condition tree walk       | VERIFIED         | Contains `Condition::CorrelatedSubquery { related, ..` arm at line 903; recurses into subquery.where_cond at line 907                                                                                                                  |
| `packages/zero-ivm-rs/src/exists_op.rs`                                                   | Edit branch in push_impl with old/new state evaluation        | VERIFIED         | Contains `or_condition_matches(&old_row)` at line 262; full 4-case match block at lines 295-305                                                                                                                                        |
| `packages/zero-ivm-rs/src/or_exists_op.rs`                                                | Edit handling in push_impl that splits when row_passes flips  | VERIFIED         | Contains `row_passes(&old_row)` at line 325; full 4-case match at lines 327-336                                                                                                                                                        |
| `packages/zero-ivm-rs/src/join_op.rs`                                                     | Promoted asserts for parent/child relationship-key invariants | VERIFIED         | Two `assert!(...)` at lines 221, 266 (promoted from `debug_assert!`), each preserving original message text                                                                                                                            |
| `packages/zero-ivm-rs/src/take_op.rs`                                                     | Promoted asserts for duplicate-primary-key invariant          | VERIFIED         | Two `assert!(new_cmp != Equal, ...)` at lines 201, 248 (promoted from `debug_assert!`)                                                                                                                                                 |
| `packages/zero-ivm-rs/src/cap_op.rs`                                                      | Promoted assert_eq for partition-key invariant                | VERIFIED         | `assert_eq!(...)` at line 167 (promoted from `debug_assert_eq!`); message preserved                                                                                                                                                    |
| `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts` | TS integration tests for AUDIT-02 end-to-end                  | EXISTS_BUT_FAILS | File exists with all 3 named tests at lines 166, 205, 250. Two of three FAIL: only `EXISTS parent_field edit emits Edit when membership preserved` passes. Other two assert that Remove/Add are emitted but observe empty result sets. |

### Key Link Verification

| From                                          | To                                                       | Via                                                | Status       | Details                                                                                                                                                                                                                                                                                                                                               |
| --------------------------------------------- | -------------------------------------------------------- | -------------------------------------------------- | ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| hydrate.rs (parse_predicate_json `like` arm)  | zero_ivm_rs::filter::Predicate::Like                     | case-insensitive flag (third arg, now `false`)     | WIRED        | Direct construction; pattern `Predicate::Like\(field, pattern, false\)` present                                                                                                                                                                                                                                                                       |
| advance.rs::collect_split_edit_keys           | RustTableSource::connect (split_keys param)              | build_pipeline_state passes the collected keys     | WIRED (Rust) | advance.rs:1114 calls collect_split_edit_keys; advance.rs:1271 calls maybe_split_edit_for_advance with `pipeline.split_edit_keys`                                                                                                                                                                                                                     |
| advance_persistent_pipeline (production path) | maybe_split_edit_for_advance                             | flat_map over diff_change_to_source_changes output | PARTIAL      | The wiring exists in source code but the TS-level production-path test (pipeline-driver.exists-parent-edit.test.ts) does not observe Remove+Add output. Either split_edit_keys is empty at runtime for this query, or the SourceChange variant produced by diff_change_to_source_changes doesn't match the case maybe_split_edit_for_advance handles. |
| exists_op.rs::push_impl::Edit                 | passes_filter + or_condition_matches                     | old/new state computation, then 4-case match       | WIRED        | Pattern `old_passed.*new_passed` present at lines 262, 279, 295; cargo test --release passes                                                                                                                                                                                                                                                          |
| or_exists_op.rs::push_impl::Edit              | row_passes (= or_condition_matches OR any_branch_passes) | old/new evaluation + 4-case match                  | WIRED        | Lines 325-336                                                                                                                                                                                                                                                                                                                                         |
| cargo test --release                          | #[should_panic] tests                                    | release-mode panic-test confirms assertions fire   | WIRED        | All 7 panic tests pass under cargo test --release (170 tests, 0 failed)                                                                                                                                                                                                                                                                               |

### Behavioral Spot-Checks

| Behavior                                                                  | Command                                                                                                        | Result                                                                     | Status |
| ------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- | ------ |
| All zero-ivm-rs tests pass in release mode                                | `cd packages/zero-ivm-rs && cargo test --release`                                                              | 170 passed; 0 failed                                                       | PASS   |
| All zqlite-rs tests pass in release mode (including formerly-flaky bench) | `cd packages/zqlite-rs && cargo test --release`                                                                | 122 passed; 0 failed; bench_persistent_pipeline_sequential_vs_parallel: ok | PASS   |
| Main pipeline-driver TS suite passes unchanged                            | `cd packages/zero-cache && npx vitest run src/services/view-syncer/pipeline-driver.test.ts`                    | 40 passed (40)                                                             | PASS   |
| AUDIT-02 TS integration test (Phase 30 Success Criteria #2 gate)          | `cd packages/zero-cache && npx vitest run src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts` | 1 passed; 2 failed (3 total)                                               | FAIL   |
| 7 #[should_panic] tests for AUDIT-03 fire correctly                       | `cargo test --release` covers them; counted in 170/170 above                                                   | All 7 pass                                                                 | PASS   |

### Requirements Coverage

| Requirement | Source Plan | Description                                                                                                                                             | Status    | Evidence                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ----------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | --------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| AUDIT-01    | 30-01       | Fix LIKE case sensitivity at hydrate.rs:769; add regression test distinguishing LIKE 'Foo%' from LIKE 'foo%'                                            | SATISFIED | hydrate.rs:769 fixed; 3 regression tests added and pass                                                                                                                                                                                                                                                                                                                                                                                       |
| AUDIT-02    | 30-02       | Fix EXISTS parent_field missing from collect_split_edit_keys; add regression test that asserts source emits Remove+Add and downstream output is correct | BLOCKED   | Rust collector fix verified at advance.rs:903; Rust unit tests pass; **but the requirement explicitly mandates "assert source emits Remove+Add (not Edit) and downstream output is correct" — this is exactly what the failing TS tests assert. Phase Success Criteria #2 ("regression test confirms downstream output matches TS for both directions of the membership transition") is not met.** Production-path wiring gap blocks closure. |
| AUDIT-03    | 30-04       | Promote framework-invariant debug_assert! to assert! in 4 operators                                                                                     | SATISFIED | 7 promotions verified across 5 files; 7 #[should_panic] tests pass under cargo test --release                                                                                                                                                                                                                                                                                                                                                 |
| AUDIT-04    | 30-03       | Fix ExistsOperator and OrExistsOperator Edit handling when or_predicate is set                                                                          | SATISFIED | 4-case truth table implemented in both operators; 9 unit tests cover the matrix and pass                                                                                                                                                                                                                                                                                                                                                      |

No orphaned requirements: all 4 IDs in REQUIREMENTS.md for Phase 30 (AUDIT-01..04) are claimed by the 4 plans.

### Anti-Patterns Found

No anti-patterns detected in modified files. Source comments at the promoted assert sites correctly reference the historical `debug_assert!` for context (e.g., `// Promoted from debug_assert! per AUDIT-03 ...`) and are not stub indicators. The two `force_in_push_for_test` helpers in exists_op.rs and or_exists_op.rs are properly `#[cfg(test)]`-gated (verified at exists_op.rs:133 and or_exists_op.rs:178).

### Human Verification Required

None — all gaps are programmatically detectable.

### Gaps Summary

**Three of the four audit fixes (AUDIT-01, AUDIT-03, AUDIT-04) are fully closed and verified** — every must-have for those requirements passes both the source-level grep checks and the cargo test --release suite, with no regressions to other tests. The formerly-flaky `bench_persistent_pipeline_sequential_vs_parallel` test now passes cleanly.

**AUDIT-02 has a partial closure gap** that surfaces only at the TS integration layer:

- The Rust-side fix is structurally complete:
  - `collect_split_edit_keys` correctly handles `Condition::CorrelatedSubquery` (verified at advance.rs:903 and via 5 passing unit tests).
  - The internal helper `maybe_split_edit_for_advance` exists (advance.rs:159) and is called inside `advance_persistent_pipeline` (advance.rs:1271).
  - `PipelineState.split_edit_keys` is populated from `collect_split_edit_keys(&query.ast)` at `build_pipeline_state` time (advance.rs:1114).

- But the production path that the AUDIT-02 integration test exercises does NOT produce Remove+Add output:
  - `pipeline-driver.exists-parent-edit.test.ts` test 1 ("EXISTS parent_field edit emits Remove when membership lost") fails with `expected [] to have a length of 1 but got +0`.
  - Test 2 ("EXISTS parent_field edit emits Add when membership gained") fails as a cascade from test 1's setup.
  - Test 3 ("emits Edit when membership preserved") passes — confirming the no-membership-change path is unaffected.

This is the same failure documented in `deferred-items.md` item #3 ("AUDIT-02 regression"), discovered during Plan 30-04 verification. The 30-03 SUMMARY also flagged it as a "pre-existing AUDIT-02 regression" out of scope for that plan. **However, the failure squarely targets AUDIT-02's own success criteria** — it is not a separate concern from AUDIT-02 itself.

The likely root cause (per deferred-items.md and the wiring inspection above) is that either:

1. `split_edit_keys` is empty at runtime for the AUDIT-02 test query because the test fixture's AST shape doesn't match the structure the collector expects, OR
2. The diff path (`diff_change_to_source_changes` → operator chain) reaches the operator before `maybe_split_edit_for_advance` runs, OR
3. A different production code path (e.g., `pipeline_manager.rs::advance_*`) processes this test's edits and bypasses the persistent advance pipeline entirely.

Closing AUDIT-02 requires tracing why the test observes Edit instead of Remove+Add and patching the wiring so the runtime behavior matches the unit-test-verified collector behavior.

**Phase Goal alignment:** The phase goal is "a known-correct operator baseline" for Phase 31. With AUDIT-02 broken at the integration level, Phase 31 (streaming) would inherit a silently-wrong Edit handling for EXISTS parent_field changes — exactly the regression class the audit was meant to prevent. This gap should be closed before Phase 31 begins.

---

_Verified: 2026-04-29T09:04:36Z_
_Verifier: Claude (gsd-verifier)_
