---
phase: 30-audit-fixes
plan: 04
subsystem: testing
tags: [rust, ivm, assertions, framework-invariants, audit-fixes]

# Dependency graph
requires:
  - phase: 30-audit-fixes
    provides: 30-02 split_edit_keys fix for EXISTS parent_field (the AUDIT-02 work that this plan's promoted asserts protect from silent regression)
provides:
  - Framework-invariant violations now panic loudly in release builds via `assert!` (not silently elided via `debug_assert!`)
  - 7 release-mode `#[should_panic]` regression tests preventing reversion to `debug_assert!`
  - Two `#[cfg(test)] pub(crate) fn force_in_push_for_test()` helpers on `ExistsOperator` and `OrExistsOperator` for re-entrancy guard testing
affects: [phase-31-streaming, future-ivm-operator-changes]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - 'Pattern: Framework-invariant assertions promoted from `debug_assert!` to `assert!` so they fire in release builds. Apply this rule to any future invariant where silent miscalculation downstream is worse than a loud panic.'
    - 'Pattern: Re-entrancy guards tested via `#[cfg(test)] pub(crate) fn force_in_push_for_test()` helper instead of constructing complex nested-push scenarios.'
    - 'Pattern: `#[should_panic(expected = ...)]` regression tests run under `cargo test --release` to confirm assertions survive release-mode dead-code elimination.'

key-files:
  created: []
  modified:
    - 'packages/zero-ivm-rs/src/join_op.rs (2 promotions, 2 tests)'
    - 'packages/zero-ivm-rs/src/exists_op.rs (1 promotion, 1 test, 1 helper)'
    - 'packages/zero-ivm-rs/src/or_exists_op.rs (1 promotion, 1 test, 1 helper, new test module)'
    - 'packages/zero-ivm-rs/src/take_op.rs (2 promotions, 2 tests)'
    - 'packages/zero-ivm-rs/src/cap_op.rs (1 promotion, 1 test)'

key-decisions:
  - 'Promoted only the 5 framework-invariant sites listed in D-08 (7 lines total, including the 2 Take sites and the OrExists parity site). No other debug_assert! calls were touched, per D-09.'
  - "Added the OrExists re-entrancy promotion as a parity-with-Exists fix per D-09's spirit, since AUDIT-03 conceptually covers `Unexpected re-entrancy` and OrExistsOperator carries identical risk. Audit text mentions only exists_op.rs:170 but the invariant is the same."
  - 'Used `#[cfg(test)] pub(crate) fn force_in_push_for_test()` helpers rather than constructing real re-entrancy via mock child sources. Helpers do not appear in release artifacts.'
  - 'Took the TDD path: RED commit first (tests added against still-debug_assert! sites — confirmed all 7 fail in `cargo test --release`), then GREEN commit promoting the asserts (all 7 pass).'

patterns-established:
  - 'Framework-invariant promotion: `debug_assert!` → `assert!` whenever silent miscalculation is the alternative failure mode.'
  - 'Release-mode panic tests: `cargo test --release` runs `#[should_panic]` tests as gates against accidental `debug_assert!` reversion.'

requirements-completed: [AUDIT-03]

# Metrics
duration: 35min
completed: 2026-04-29
---

# Phase 30 Plan 04: AUDIT-03 (debug_assert! → assert! Promotion) Summary

**Promoted 7 framework-invariant `debug_assert!`/`debug_assert_eq!` sites to `assert!`/`assert_eq!` across Join/Exists/OrExists/Take/Cap operators so silent miscalculation regressions surface as loud release-mode panics, with 7 `#[should_panic]` regression tests proving the assertions fire under `cargo test --release`.**

## Performance

- **Duration:** ~35 min
- **Started:** 2026-04-29T07:06:00Z
- **Completed:** 2026-04-29T07:41:43Z
- **Tasks:** 2 (Task 1: TDD-style promotion + tests; Task 2: verification gate)
- **Files modified:** 5 Rust source files (no production API touched)

## Accomplishments

- 7 framework-invariant assertion sites are now active in release builds (previously stripped by `debug_assert!`)
- 7 `#[should_panic]` regression tests verify each promoted assertion fires under `cargo test --release` — preventing future reversion to `debug_assert!`
- Two `#[cfg(test)] pub(crate) fn force_in_push_for_test()` test helpers added to `ExistsOperator` and `OrExistsOperator`, both `#[cfg(test)]`-gated so they do not appear in release artifacts
- 161 zero-ivm-rs unit tests pass in release mode (previous total was 154; +7 new panic tests)
- No public API changes (`git diff | grep '^[+-]\s*pub fn '` returns empty)
- No wire-format changes (no `encode_advance_result_buf`/`decodeAdvanceResultBuf` modifications)

## Task Commits

1. **Task 1 RED: Add 7 should_panic regression tests** — `a605d5213` (test)

   Adds the failing tests against the still-`debug_assert!` sites. Confirmed all 7 fail in `cargo test --release` because `debug_assert!` is stripped (no panic → `should_panic` test fails).

2. **Task 1 GREEN: Promote 7 debug_assert sites to assert** — `54b195129` (fix)

   The 7 promoted sites (per D-08):
   - `join_op.rs:219` `debug_assert!` → `assert!` ("Parent edit must not change relationship.")
   - `join_op.rs:263` `debug_assert!` → `assert!` ("Child edit must not change relationship.")
   - `exists_op.rs:170` `debug_assert!` → `assert!` ("Unexpected re-entrancy")
   - `or_exists_op.rs:224` `debug_assert!` → `assert!` ("Unexpected re-entrancy") — parity with exists, per D-09 spirit
   - `take_op.rs:200` `debug_assert!` → `assert!` ("Invalid state. Row has duplicate primary key") — outside/outside branch
   - `take_op.rs:246` `debug_assert!` → `assert!` ("Invalid state. Row has duplicate primary key") — inside/outside branch
   - `cap_op.rs:166` `debug_assert_eq!` → `assert_eq!` ("Cap: partition key must not change on edit")

   After this commit, `cargo test --release _panics` shows `7 passed; 0 failed` — confirming D-11 (assertions fire in release builds).

3. **Plan metadata commit** — _(this commit, including SUMMARY.md and deferred-items.md update)_

The 7 `#[should_panic]` test functions:

| Test name                                                 | File                       | Promoted-assert message                      |
| --------------------------------------------------------- | -------------------------- | -------------------------------------------- |
| `test_join_parent_edit_changing_relationship_panics`      | join_op.rs                 | Parent edit must not change relationship.    |
| `test_join_child_edit_changing_relationship_panics`       | join_op.rs                 | Child edit must not change relationship.     |
| `test_exists_reentrancy_panics`                           | exists_op.rs               | Unexpected re-entrancy                       |
| `test_or_exists_reentrancy_panics`                        | or_exists_op.rs            | Unexpected re-entrancy                       |
| `test_take_duplicate_pk_in_inside_outside_branch_panics`  | take_op.rs (line 246 site) | Invalid state. Row has duplicate primary key |
| `test_take_duplicate_pk_in_outside_outside_branch_panics` | take_op.rs (line 200 site) | Invalid state. Row has duplicate primary key |
| `test_cap_partition_key_change_on_edit_panics`            | cap_op.rs                  | Cap: partition key must not change on edit   |

## Files Created/Modified

- `packages/zero-ivm-rs/src/join_op.rs` — Promoted parent and child edit relationship-key invariants (`debug_assert!` → `assert!`); added 2 `#[should_panic]` tests.
- `packages/zero-ivm-rs/src/exists_op.rs` — Promoted in_push re-entrancy invariant; added `force_in_push_for_test()` test helper (cfg-gated) and 1 `#[should_panic]` test.
- `packages/zero-ivm-rs/src/or_exists_op.rs` — Promoted in_push re-entrancy invariant (parity with Exists); added `force_in_push_for_test()` test helper (cfg-gated) and a new `#[cfg(test)] mod tests` block containing 1 `#[should_panic]` test.
- `packages/zero-ivm-rs/src/take_op.rs` — Promoted both duplicate-primary-key invariants (line 200 outside/outside branch and line 246 inside/outside branch); added 2 `#[should_panic]` tests.
- `packages/zero-ivm-rs/src/cap_op.rs` — Promoted partition-key edit invariant (`debug_assert_eq!` → `assert_eq!`); added `make_node_with_region()` helper and 1 `#[should_panic]` test.

## Decisions Made

- **D-08 (compliance):** Promoted only the 5 framework-invariant sites named in 30-CONTEXT.md (which expand to 7 lines after counting the 2 Take sites and adding the OrExists parity site). Confirmed via `grep` that no other `debug_assert!` calls in these files were touched.
- **D-09 spirit (parity):** Promoted `or_exists_op.rs:224` as well, even though the audit only mentions `exists_op.rs:170`. The invariant "in_push re-entrancy" applies identically to both operators, and silently failing on OrExists while loudly failing on Exists would be inconsistent and misleading.
- **D-10 (regression tests):** Each promoted assertion has a corresponding `#[should_panic(expected = "...")]` test in the same file. Test naming follows the exact pattern listed in the plan (`test_<op>_<scenario>_panics`).
- **D-11 (release-mode validation):** All 7 panic tests run under `cargo test --release`, which is the only configuration where the difference between `debug_assert!` and `assert!` matters. RED phase confirmed `debug_assert!` is stripped (tests failed expecting a panic that never came); GREEN phase confirmed `assert!` fires (all 7 pass).
- **Helper visibility:** `force_in_push_for_test()` is `pub(crate)` rather than `pub` because it should not be reachable from downstream crates — only from same-crate tests. The `#[cfg(test)]` gate keeps it out of release artifacts entirely. Verified via `git diff | grep -B1 'fn force_in_push_for_test' | grep -c 'cfg(test)'` returning 2 (both helpers gated).

## Deviations from Plan

None — plan executed exactly as written.

The plan's `<promotion-sites>` block listed 6 line changes total (Join: 2, Exists: 1, OrExists: 1, Take: 2, Cap: 1 = 7 lines, the plan text mentions "6 line changes" elsewhere but enumerates 7); we promoted exactly the 7 sites enumerated in `<action>` Step 1.

Per the plan, the OrExists re-entrancy site (`or_exists_op.rs:224`) was promoted explicitly per D-09 spirit (audit text mentions only `exists_op.rs:170`, but the invariant covers Exists re-entrancy generally and OrExistsOperator carries the same risk).

## Issues Encountered

- **Pre-existing test failures during the verification gate (out of scope, deferred):**
  - `pipeline-driver.exists-parent-edit.test.ts` (2 failed tests testing AUDIT-02 fix) — documented in `deferred-items.md` item 3. AUDIT-03 only modifies Rust IVM operator files; the failing test file is in TS, exercises the persistent pipeline path, and contains none of the promoted assertion messages. Failure is therefore in AUDIT-02 territory, not AUDIT-03.
  - `advance::tests::bench_persistent_pipeline_sequential_vs_parallel` — pre-existing flake (item 1 + item 4 in `deferred-items.md`). Reproduces against `b2d397d6a` (the plan-04 base) without any of plan-04's changes. AUDIT-03 modifies only `zero-ivm-rs` source files; this bench is in `zqlite-rs/src/advance.rs` (unmodified by this plan).

  Confirmation that AUDIT-03 is clean: `pipeline-driver.test.ts` (40/40 pass) and `fuzz-ivm.test.ts` with `FUZZ_NUM_RUNS=1000` (6/6 pass + 1 todo) both pass cleanly — these are the canonical operator-semantics regression suites referenced in the verification gate. No new IVM panics introduced.

## Verification Gate Results

| Check                                                      | Result                                                  | Notes                                                                                      |
| ---------------------------------------------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `cargo test --release` (zero-ivm-rs)                       | PASS — 161/161 tests pass                               | Includes the 7 new `#[should_panic]` panic tests                                           |
| `cargo test --release` (zqlite-rs)                         | 121/122 pass; 1 fail (pre-existing)                     | The 1 failure is `bench_persistent_pipeline_sequential_vs_parallel`, deferred-items #1+#4  |
| `vitest pipeline-driver.test.ts`                           | PASS — 40/40                                            | No regressions from promotion                                                              |
| `vitest fuzz-ivm.test.ts (FUZZ_NUM_RUNS=1000)`             | PASS — 6/6 + 1 todo                                     | Canonical fuzz gate; 1000 iterations × 6 scenarios = 6000 random IVM cycles, no new panics |
| `vitest pipeline-driver.*.test.ts` (broader pattern)       | 133/135 pass; 2 fail (pre-existing AUDIT-02 regression) | Failures in `pipeline-driver.exists-parent-edit.test.ts`, deferred-items #3                |
| `git diff \| grep '^[+-]\s*pub fn '`                       | EMPTY                                                   | No public API surface changed                                                              |
| `git diff \| grep "encode_advance_result_buf"`             | 0 hits                                                  | No wire format changes                                                                     |
| Both `force_in_push_for_test` helpers `#[cfg(test)]`-gated | 2/2 confirmed                                           | Verified via `git diff -B1 ... \| grep -c 'cfg(test)'`                                     |

## Self-Check

After writing this SUMMARY, verifying the claims:

- Commit `a605d5213` (RED tests) exists in `git log`: confirmed
- Commit `54b195129` (GREEN promotion) exists in `git log`: confirmed
- File `packages/zero-ivm-rs/src/join_op.rs` modified with 2 promotions + 2 tests: confirmed
- File `packages/zero-ivm-rs/src/exists_op.rs` modified with 1 promotion + 1 helper + 1 test: confirmed
- File `packages/zero-ivm-rs/src/or_exists_op.rs` modified with 1 promotion + 1 helper + new test mod + 1 test: confirmed
- File `packages/zero-ivm-rs/src/take_op.rs` modified with 2 promotions + 2 tests: confirmed
- File `packages/zero-ivm-rs/src/cap_op.rs` modified with 1 promotion + 1 test: confirmed
- All 7 panic tests pass under `cargo test --release`: confirmed (`7 passed; 0 failed`)

## Self-Check: PASSED

## Next Plan Readiness

- **AUDIT-03 closed.** Phase 30 wave 2 work (this plan) complete and ready for merge.
- AUDIT-04 (Plan 30-03 — Exists Edit handling with `or_predicate`) was scheduled in parallel wave 2; that plan owns the next set of operator-semantics changes.
- The 7 release-mode panic tests act as long-term gates: future agents who refactor the IVM operators cannot revert these `assert!` to `debug_assert!` without making `cargo test --release` fail.
- Deferred items (`pipeline-driver.exists-parent-edit.test.ts` failures, `bench_persistent_pipeline_sequential_vs_parallel` flake) are documented in `deferred-items.md` for follow-up work outside Phase 30.

---

_Phase: 30-audit-fixes_
_Plan: 04 (AUDIT-03)_
_Completed: 2026-04-29_
