---
phase: 30-audit-fixes
review_type: code-review
depth: standard
reviewed: 2026-04-29
status: issues_found
files_reviewed: 9
findings:
  critical: 0
  warning: 1
  info: 4
  total: 5
---

# Phase 30 (audit-fixes) — Code Review

**Reviewed:** 2026-04-29
**Depth:** standard
**Files Reviewed:** 9
**Status:** issues_found (1 Warning, 4 Info, no Critical)

## Summary

Phase 30 closes 4 audit findings (AUDIT-01..04) with surgical changes. Overall the patches are correct and well-tested:

- **AUDIT-01** (`hydrate.rs:769`): single-character flip, correctly tested with case-sensitive vs case-insensitive evaluation.
- **AUDIT-02** (`advance.rs::collect_from_cond`): the new `CorrelatedSubquery` arm correctly walks `related.correlation.parent_field` and recurses into `related.subquery.where_cond` for nested EXISTS. No infinite-recursion risk because `subquery.where_cond` is a finite tree distinct from the parent AST. Tests cover top-level CSQ, CSQ in AND, nested CSQ in subquery where, and a no-CSQ regression guard.
- **AUDIT-03** (5 promoted asserts): all 5 sites guard genuine framework invariants (re-entrancy in Exists/OrExists, parent-edit join key in Join, child-edit join key in Join, partition-key change in Cap, duplicate-PK in Take). Each is paired with a `#[should_panic]` regression test.
- **AUDIT-04** (Exists/OrExists Edit branch): the 4-case truth table mapping `(old_passed, new_passed)` to `Edit/Remove/Add/nothing` is correctly implemented in both operators, with full per-transition test coverage and a no-or_predicate regression guard.

`table_source.rs` is in scope only for a pre-existing test-side change (Mutex deref for `push_epoch` reads, commit `22cba6141`); no production code changed. No findings here.

## Warnings

### WR-01: ExistsOperator Edit branch bypasses `in_push` cache discipline (pre-existing, surfaced by AUDIT-04)

**File:** `packages/zero-ivm-rs/src/exists_op.rs:262-293`
**Issue:** The new Edit branch reads `parent_sizes` directly via `self.parent_sizes.get(&old_pk).copied().unwrap_or_else(...)` rather than going through `get_or_fetch_count(row)` (lines 118-128), which is the helper that honors `in_push` by skipping the cache during a push. This means during an Edit push, a stale cache value (from a prior fetch or prior push that never invalidated) can be used instead of refetching the child count.

Note: this exact pattern is already used by the `Add | Remove` arm (line 241) and the `Child` arm (line 314, 330, 357, 387), so the new Edit code is consistent with existing behavior — but it diverges from the TS `#inPush` discipline that the comment at line 16-18 claims to mirror.

By contrast, `OrExistsOperator::push_impl::Edit` (`or_exists_op.rs:325-326`) correctly routes through `row_passes` → `branch.passes` → `get_or_fetch_count(row, in_push)` which DOES respect `in_push`. The two operators now disagree on cache discipline during Edit.

In practice the AUDIT-02 invariant (parent_field unchanged in Edit reaching this branch) means `old_pk == new_pk` and the cached count is the same one used to admit the parent into the result set — so the stale-read window is narrow. But the comment claim of TS parity is incorrect, and the inconsistency between the two operators is a maintenance hazard.

**Fix:** Either (a) add a follow-up plan to route both operators' Edit branches through `get_or_fetch_count` for true `in_push` parity, or (b) document that ExistsOperator deliberately skips the `in_push` discipline (as an optimization safe under AUDIT-02), and reconcile the OrExists branch the same way. Pick one and apply consistently. Out of scope for closing AUDIT-04, but worth a deferred-item entry.

## Info

### IN-01: AUDIT-04 fix relies on AUDIT-02 invariant — assert it explicitly

**File:** `packages/zero-ivm-rs/src/exists_op.rs:252-257` (and `or_exists_op.rs:319-324`)
**Issue:** The comment "With AUDIT-02 enforced upstream, parent_field doesn't change in an Edit reaching this branch" is a load-bearing precondition for cache correctness (`old_pk == new_pk`), but there is no runtime check. If AUDIT-02 ever regresses (or a non-source-driven Edit reaches this branch through some future code path), the cache lookup for `old_pk` may differ from `new_pk` and stale state could be observed.
**Fix:** Add a `debug_assert_eq!(old_pk_str, new_pk_str, "AUDIT-02 invariant: parent_field must not change in Edit reaching ExistsOperator")` at the top of the Edit branch. Cheap to compute, catches regressions in CI without affecting release perf. (Promoting it to `assert!` per AUDIT-03 reasoning would be even stronger, but `debug_assert!` is sufficient since AUDIT-02 is enforced at the source layer two operators upstream.)

### IN-02: Test for "membership preserved" Edit accepts both single-Edit and Remove+Add outputs

**File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts:283-302`
**Issue:** `test_exists_parent_field_edit_emits_edit_when_membership_preserved` asserts `1 <= totalChangesForP1 <= 2` — accepting both shapes. Comment at lines 264-268 documents this is intentional, but the looseness means a future regression (e.g., emitting two Adds for p1) could pass this test if it stays within `<=2`. Consider tightening to assert exact shape: either exactly 1 Edit, OR exactly 1 Remove + exactly 1 Add (mutually exclusive, total == counts of expected types).
**Fix:** Replace the bounds check with a stricter exact-match using `XOR`-style branching, e.g.:

```ts
const isEdit = edits.length === 1 && removes.length === 0 && adds.length === 0;
const isSplit = edits.length === 0 && removes.length === 1 && adds.length === 1;
expect(isEdit || isSplit).toBe(true);
```

### IN-03: `node()` helper called twice on `change.node()` in CapOperator Edit branch

**File:** `packages/zero-ivm-rs/src/cap_op.rs:165, 173`
**Issue:** `change.node()` is invoked twice (once for `new_part_key`, once for `new_pk`). On the assumption `change.node()` is a cheap accessor returning a borrow this is fine, but if it ever clones the row map this becomes a per-Edit allocation. The same pattern repeats in `Change::Child` arm (lines 188, 189).
**Fix:** Bind once: `let new_node = change.node(); let new_part_key = cap_state_key(...., &new_node.row); let new_pk = serialize_pk(&self.primary_key, &new_node.row);`. Trivial readability + perf-safety improvement; not behavior-changing.

### IN-04: AUDIT-04 Edit branch in OrExistsOperator does NOT update `parent_sizes` cache via the (true,true) Edit pass-through, but Add/Remove sub-branches DO

**File:** `packages/zero-ivm-rs/src/or_exists_op.rs:325-336`
**Issue:** The `(true,true)` and `(false,false)` arms simply emit/drop without writing the recomputed counts back into `branch.parent_sizes`. This means the cache stays at whatever `row_passes` populated via `branch.passes → get_or_fetch_count` (which DOES insert under in_push, per `or_exists_op.rs:95`). So this is actually fine — `get_or_fetch_count` writes the count it computed even when in_push is true (line 95: `self.parent_sizes.insert(pk, count)` is unconditional). The `(true,false)` and `(false,true)` arms also don't explicitly update, but again the underlying `get_or_fetch_count` does. So no bug — but the cache update is implicit and easy to miss when reading the Edit branch in isolation.
**Fix:** No change required; consider a one-line comment in the Edit branch ("note: parent_sizes already updated by row_passes → get_or_fetch_count") to make the implicit invariant visible.

---

## Out-of-scope observations (FYI, not findings)

- `table_source.rs::reset_state` (lines 239-246) uses raw-pointer mutation through a `&self` borrow to modify `last_pushed_epoch` on each `Connection`. This is pre-existing pre-phase-30 code, not introduced or modified by phase 30. It's unsound under Rust's aliasing rules but won't surface unless `reset_state` races with a concurrent reader of `last_pushed_epoch`. Mention only because it's in a file in scope; phase 30 did not change this behavior.
- The `unsafe impl Send`/`Sync` block at `table_source.rs:70-71` is similarly pre-existing and not in scope.

---

## Files in scope (final list)

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts`
- `packages/zero-ivm-rs/src/cap_op.rs`
- `packages/zero-ivm-rs/src/exists_op.rs`
- `packages/zero-ivm-rs/src/join_op.rs`
- `packages/zero-ivm-rs/src/or_exists_op.rs`
- `packages/zero-ivm-rs/src/take_op.rs`
- `packages/zqlite-rs/src/advance.rs`
- `packages/zqlite-rs/src/hydrate.rs`
- `packages/zqlite-rs/src/table_source.rs` (no production change in phase; verified via `git log` against diff base)
