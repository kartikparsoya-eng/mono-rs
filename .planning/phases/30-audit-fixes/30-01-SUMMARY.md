---
phase: 30-audit-fixes
plan: 01
subsystem: zqlite-rs (IVM operator parsing)
tags: [bugfix, audit, ivm, like, ilike, case-sensitivity, parity]
requires:
  - phase: 28
    plan: '*'
    provides: 'unified Rust IVM parser (parse_predicate_json) at hydrate.rs'
provides:
  - 'Case-sensitive LIKE filter in Rust IVM (matches TS getLikePredicate semantics)'
  - 'Regression tests pinning LIKE vs ILIKE case behavior in mod tests'
affects:
  - packages/zqlite-rs/src/hydrate.rs (parse_predicate_json — like arm flag)
  - packages/zqlite-rs/src/table_source.rs (test fix only — Mutex<u64> assertion)
tech-stack:
  added: []
  patterns:
    - 'TDD RED→GREEN cycle for one-character production fix'
    - 'Co-located #[cfg(test)] mod tests with serde_json::json! fixtures'
key-files:
  created: []
  modified:
    - packages/zqlite-rs/src/hydrate.rs
    - packages/zqlite-rs/src/table_source.rs
decisions:
  - 'D-01 (CONTEXT): hydrate.rs:769 third arg of Predicate::Like for the like key changed from true to false; ilike arm unchanged at true'
  - 'D-02 (CONTEXT): three regression tests added inside the existing mod tests block — case-sensitive LIKE, case-insensitive ILIKE, and distinguishing test'
  - 'D-03 SKIPPED (per plan): no separate TS integration test added — existing pipeline-driver.*.test.ts suite covers the end-to-end path; a duplicate would not increase regression coverage'
metrics:
  duration: '~30 minutes (RED + GREEN + REFACTOR + verification)'
  completed: 2026-04-29
---

# Phase 30 Plan 01: AUDIT-01 LIKE Case Sensitivity Fix Summary

**One-liner:** One-character bugfix flipping `Predicate::Like(_, _, true)` → `false` for the `like` arm in `parse_predicate_json`, plus three Rust regression tests pinning that LIKE is case-sensitive and ILIKE is case-insensitive.

## What Shipped

### Production fix (1 line)

**File:** `packages/zqlite-rs/src/hydrate.rs:769`

```diff
         if let Some(val) = obj.get("like") {
             let pattern = val.as_str().ok_or("'like' value must be a string")?.to_string();
-            return Ok(Predicate::Like(field, pattern, true));
+            return Ok(Predicate::Like(field, pattern, false));
         }
```

The `ilike` arm at line 773 stays `true` (already correct). Now matches the parity reference in `packages/zero-ivm-rs/src/pipeline.rs` lines 128-135, which has been correct from the start.

### Regression tests (3 #[test] functions)

Added to `mod tests` block at the end of `packages/zqlite-rs/src/hydrate.rs` (~line 1782+):

1. **`test_parse_predicate_like_case_sensitive`** — pins `Predicate::Like(_, _, false)` for the `like` key and asserts:
   - `LIKE 'Foo%'` matches `'Foo Bar'` (true)
   - `LIKE 'Foo%'` does NOT match `'foo bar'` (false — case-sensitive)
   - `LIKE 'Foo%'` does NOT match `'FOO'` (false — case-sensitive)

2. **`test_parse_predicate_ilike_case_insensitive`** — pins `Predicate::Like(_, _, true)` for the `ilike` key and asserts:
   - `ILIKE 'foo%'` matches `'Foo Bar'`, `'foo bar'`, AND `'FOO'`

3. **`test_parse_predicate_like_distinguishes_from_ilike`** — same input row `{name: "Foo Bar"}` produces opposite results under `LIKE 'foo%'` (false) vs `ILIKE 'foo%'` (true). Canonical Phase Success Criteria #1 distinguishing test.

## TDD Cycle Followed

- **RED:** Wrote all three tests against the bugged `like` arm. `cargo test --release test_parse_predicate_like` confirmed:
  - `test_parse_predicate_like_case_sensitive` FAILED with `expected Predicate::Like("name", "Foo%", false), got Like("name", "Foo%", true)`
  - `test_parse_predicate_like_distinguishes_from_ilike` FAILED with `LIKE 'foo%' must NOT match 'Foo Bar' (case-sensitive)`
  - `test_parse_predicate_ilike_case_insensitive` PASSED (ilike was never buggy)
- **GREEN:** Flipped `true` → `false` on hydrate.rs:769. All 3 tests now pass.
- **REFACTOR:** None — minimal patch.

## Verification Gate Results

| Check                                                                | Status | Notes                                                                                                                                            |
| -------------------------------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `cd packages/zqlite-rs && cargo test --release test_parse_predicate` | PASS   | 3 passed; 0 failed                                                                                                                               |
| `cd packages/zqlite-rs && cargo test --release` (full suite)         | PARTIAL | 116 passed; 1 failed (`bench_persistent_pipeline_sequential_vs_parallel`) — pre-existing, unrelated. Logged in `deferred-items.md` for follow-up |
| `cd packages/zero-ivm-rs && cargo test --release` (full suite)       | PASS   | 154 passed; 0 failed                                                                                                                             |
| Hard constraint: no pub/private fn signature changes outside tests   | PASS   | `git diff` only adds three new `fn test_*` functions; no existing function signatures touched                                                    |
| Hard constraint: no wire-format changes (`encode_advance_result_buf` / `decodeAdvanceResultBuf`) | PASS   | Grep returned 0 matches in those files' diffs                                                                                                    |
| Hard constraint: ilike arm at line 773 byte-for-byte unchanged       | PASS   | `git diff` shows only one `-/+` pair for the like arm, ilike arm is in unchanged context                                                         |

The `pipeline-driver.*.test.ts` and `fuzz-ivm.test.ts` TS gates were not run by this executor (the TS test infrastructure changes that exist outside HEAD are needed for those suites to even parse); the orchestrator owns post-wave validation per parallel-execution context.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking compile error] `packages/zqlite-rs/src/table_source.rs` test_push_epoch_increments**

- **Found during:** Task 1 RED-phase test compilation
- **Issue:** `assert_eq!(src.push_epoch, 0)` (and at lines 927, 932) failed to compile with `error[E0369]: binary operation '==' cannot be applied to type 'std::sync::Mutex<u64>'`. The field `push_epoch` was changed from `u64` to `Mutex<u64>` at HEAD (commit 599a7b229 / 94f9a8c7a) without updating the three test-side assertions.
- **Fix:** Changed `assert_eq!(src.push_epoch, N)` to `assert_eq!(*src.push_epoch.lock().unwrap(), N)` on lines 923, 927, 932.
- **Why auto-fixed:** Compilation error blocked `cargo test --release` from running my AUDIT-01 tests at all. Per Rule 3 (auto-fix blocking issues) — minimum scope: only the 3 assertion sites needed to unblock.
- **Files modified:** `packages/zqlite-rs/src/table_source.rs` (lines 923, 927, 932 only)
- **Commit:** Same commit as Task 1 (`22cba6141`).

### Other Deviations

None. The single-character production fix landed exactly as D-01 specified, and the three regression tests use the exact names listed in the plan's `<acceptance_criteria>`.

### Skipped Items (intentional, per plan)

- **D-03 (TS integration test)** — explicitly SKIPPED in 30-01-PLAN.md: "the TS integration test is covered by the existing `pipeline-driver.*.test.ts` suite passing unchanged… Adding a dedicated TS test here would duplicate coverage; the Rust unit tests are the regression gate."

## Authentication Gates

None — no external services or auth required.

## Threat Flags

None — change is to a pure-function predicate parser. No new network surface, no auth path, no schema/file access changes. The fix REDUCES surface (was over-permissive: case-insensitive matched too many rows).

## Commits

| Task | Description                                                                | Hash         | Files Modified                                                                |
| ---- | -------------------------------------------------------------------------- | ------------ | ----------------------------------------------------------------------------- |
| 1    | fix LIKE case-sensitivity + add 3 regression tests + unblock test compile | `22cba6141`  | `packages/zqlite-rs/src/hydrate.rs`, `packages/zqlite-rs/src/table_source.rs` |
| 2    | verification gate (no files modified — verification only)                  | (no commit)  | —                                                                             |

## Self-Check: PASSED

- `[x] FOUND: packages/zqlite-rs/src/hydrate.rs:769 reads 'Predicate::Like(field, pattern, false)' for the like arm`
- `[x] FOUND: packages/zqlite-rs/src/hydrate.rs:773 reads 'Predicate::Like(field, pattern, true)' for the ilike arm (unchanged)`
- `[x] FOUND: fn test_parse_predicate_like_case_sensitive in hydrate.rs`
- `[x] FOUND: fn test_parse_predicate_ilike_case_insensitive in hydrate.rs`
- `[x] FOUND: fn test_parse_predicate_like_distinguishes_from_ilike in hydrate.rs`
- `[x] FOUND: commit 22cba6141 in git log`
- `[x] FOUND: .planning/phases/30-audit-fixes/30-01-SUMMARY.md (this file)`
- `[x] FOUND: .planning/phases/30-audit-fixes/deferred-items.md (logging the unrelated benchmark failure)`

## References

- Plan: `.planning/phases/30-audit-fixes/30-01-PLAN.md`
- Context: `.planning/phases/30-audit-fixes/30-CONTEXT.md` (decisions D-01, D-02; D-03 deliberately deferred)
- Audit source: `.planning/IVM-PORT-AUDIT.md` Bug #1 section
- Parity reference: `packages/zero-ivm-rs/src/pipeline.rs` lines 128-135 (already correct)
- TS parity reference: `packages/zql/src/builder/filter.ts` lines 113-150 (`createPredicateImpl` / `getLikePredicate`)
- Deferred follow-ups: `.planning/phases/30-audit-fixes/deferred-items.md`
