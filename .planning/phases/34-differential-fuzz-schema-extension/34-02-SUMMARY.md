---
phase: 34
plan: 34-02
slug: track-2-b1-b2-fixes
subsystem: Rust IVM (zqlite-rs ast_to_config + zero-ivm-rs exists_op) + tools/ivm-parity diff tests
tags: [phase-34, track-2, hardening, b1, b2, wave-1]
dependency_graph:
  requires:
    - 34-CONTEXT.md (D-15 in-scope BLOCKING fixes, D-17 TS-as-spec, D-18 no regressions)
    - 34-RESEARCH.md (B1 + B2 fix specifications, TS file:line citations)
    - 34-01-SUMMARY.md (Wave 0 red-state stubs for B1/B2 — flipped green here)
    - IVM-PORT-AUDIT-DEEP.md §B1, §B2
  provides:
    - B1 fixed: Skip emitted before append_condition_configs, mirroring TS builder.ts:302-345.
    - B2 fixed: parent_sizes caches REAL children.len(), no .max(1) poison.
    - Wave 0 stubs B1 + B2 flipped from #[ignore] red-state to passing GREEN.
    - One new B2 push regression test (test_b2_push_add_after_empty_or_predicate_parent).
    - B1 CAVEAT documented (#[ignore]'d test test_b1_csq_then_filter_inside_and) for follow-up.
    - tools/ivm-parity/diff-tests-track2.ts — TS↔RS shape parity for B1 + B2.
  affects:
    - packages/zqlite-rs/src/ast_to_config.rs (Skip placement order; widened B3 stub search window)
    - packages/zero-ivm-rs/src/exists_op.rs (max(1) → real count; new test helper parent_sizes_for_test)
    - tools/ivm-parity/package.json (added diff:track2 script)
tech_stack:
  added: []
  patterns:
    - TS-as-spec citation in code comments (// mirrors TS builder.ts:302-345)
    - Source-level guard via runtime-assembled needle to avoid include_str! self-match
    - Self-contained differential test script duplicating harness primitives (avoids modifying parity-test apparatus)
key_files:
  created:
    - tools/ivm-parity/diff-tests-track2.ts (499 lines)
    - .planning/phases/34-differential-fuzz-schema-extension/34-02-SUMMARY.md
  modified:
    - packages/zqlite-rs/src/ast_to_config.rs (+108 / −36 lines)
    - packages/zero-ivm-rs/src/exists_op.rs (+183 / −27 lines)
    - tools/ivm-parity/package.json (+1 line)
decisions:
  - B2 push_impl audit (plan Task 2 step 2) showed all push branches that need or_predicate truth call or_condition_matches directly (Change::Add/Remove parent at ~237; Change::Edit at ~262/279; Change::Child at ~308). Removing `.max(1)` is therefore surgical — cache is delta-only, or_predicate decisions are re-evaluated.
  - Differential test script is self-contained (duplicates subscribeAndHydrate / summarizeDiff / hashRows from harness-coverage.ts) rather than exporting from harness-coverage.ts, keeping the parity-test apparatus untouched per SKILL.md hard rule #2.
  - B1 CAVEAT (CSQ vs Filter inner ordering inside an And) is documented as an #[ignore]'d test rather than fixed inline — RESEARCH.md §B1 explicitly notes this as a potential follow-up; not in plan 34-02 scope.
  - B3 Wave 0 stub's source-search window widened from 300 to 600 chars after B1 fix added doc comments to the Take section. Stub still red-state passes; B3 (plan 34-05) work intact.
metrics:
  duration_seconds: 1450
  duration_human: ~24 minutes
  completed_date: 2026-04-29
  tasks_completed: 3
  files_created: 2
  files_modified: 3
  total_lines_added: ~792
---

# Phase 34 Plan 02: Track 2 B1 + B2 Surgical Fixes Summary

**One-liner:** Land Track 2 BLOCKING fixes B1 (Skip placement order in `ast_to_operator_configs`) and B2 (`parent_sizes.max(1)` cache poison in `ExistsOperator::fetch`), each citing exact TS file:line spec and flipping its Wave 0 red-state stub to GREEN. Adds `tools/ivm-parity/diff-tests-track2.ts` differential test for both shapes.

## Objective Recap

Per CONTEXT D-15 (in-scope BLOCKING) + D-17 (TS-as-spec) + D-18 (no regressions): land surgical, mirror-of-TS fixes for B1 and B2 with full test coverage and a TS↔RS differential test. Both fixes are single-site (no signature changes; no architectural restructuring) and verifiable via Rust unit tests + parity tooling.

Both bugs were discovered in `IVM-PORT-AUDIT-DEEP.md`. Wave 0 (plan 34-01) put `#[ignore]` red-state stubs in place asserting the BROKEN behavior. This plan inverts those assertions and lands the corresponding source fixes in the same atomic commit per Wave 0 D-18 contract.

## What Shipped

### Task 1 — B1: Skip placement order

**Site:** `packages/zqlite-rs/src/ast_to_config.rs` lines 215-265 (region of `ast_to_operator_configs`).

**Change:** Reordered the operator-config emission to mirror TS `packages/zql/src/builder/builder.ts:302-345`:

| Step | Pre-fix Rust | Post-fix Rust (this plan) | TS canonical |
| ---- | ------------ | ------------------------- | ------------ |
| 1    | Source       | Source                    | builder.ts:291-296 |
| 2    | Filter+Exists (`append_condition_configs`) | **Skip** | **builder.ts:302-306** |
| 3    | Skip          | Filter+Exists (`append_condition_configs`) | builder.ts:308-333 |
| 4    | Take (`partition_key: None`) | Take (`partition_key: None`, B3 still pending plan 34-05) | builder.ts:335-345 |
| 5    | Related Joins | Related Joins             | builder.ts:347-356 |

The change is order-only; no signature change to `ast_to_operator_configs`. Each section's comment cites the corresponding TS line range per D-17 discipline.

**Tests:**
- `test_b1_skip_before_conditions` (was Wave 0 #[ignore] red-state stub) — flipped GREEN. Now asserts `skip_marker < where_marker` (i.e., Skip precedes conditions) per TS spec.
- `test_b1_csq_then_filter_inside_and` (NEW, #[ignore]'d) — documents the CAVEAT from RESEARCH §B1: when an `And` wraps both a simple predicate and a CSQ, `append_condition_configs` walks them in declaration order, so `[simple, csq]` produces `[Filter, Exists]` (Rust) vs `[Exists, Filter]` (TS). Tracking for follow-up plan; not in 34-02 scope.

**Side effect:** B3 Wave 0 stub `test_b3_partition_key_threading` reads source via `include_str!` and looks for `partition_key: None,` within 300 chars after the `// 4. Limit -> Take` marker. The B1 fix added a multi-line doc comment to the Take section, pushing the literal past the 300-char window. Widened the search window to 600 chars to keep the B3 stub red-state intact (it still passes against the unchanged `partition_key: None,`). Documented inline in the test.

**Commit:** `4fa50987e` — `fix(34-02): B1 — emit Skip before conditions in ast_to_operator_configs`

### Task 2 — B2: parent_sizes max(1) cache poison

**Site:** `packages/zero-ivm-rs/src/exists_op.rs` line 152 (now line 157 after the comment expansion).

**Change:**
```diff
-                self.parent_sizes.insert(pk, children.len().max(1));
+                self.parent_sizes.insert(pk, children.len());
```

Mirrors TS `packages/zql/src/ivm/exists.ts`. TS Exists is a pure FilterOperator that tracks per-parent state via the upstream Join's relationship contents — there is no `.max(1)` adjustment. The Rust cache previously stored 1 instead of 0 for or_predicate-positive parents with empty children, causing two failure modes:
1. Push Add boundary check `old_count==0 && new_count==1` failed when the cached value was 1 instead of 0, eliding the Add transition for the first child of an or_predicate parent.
2. AUDIT-04 Edit-with-or_predicate flip's count fallback could read a stale 1 even when the real count was 0.

**push_impl audit findings (Task 2 action item 2):**
- `Change::Add | Change::Remove` parent branch (line ~237): calls `self.or_condition_matches(&row)` directly and short-circuits if true. Cache used only for delta detection.
- `Change::Edit` branch (line ~262, ~279): the AUDIT-04 fix already evaluates `or_condition_matches` on BOTH `old_row` AND `new_row` — cache used only for the count-fallback when or_predicate is false.
- `Change::Child` branch (line ~308): calls `self.or_condition_matches(&node.row)` first and short-circuits with passthrough if true. The cache feeds `old_count`/`new_count` for boundary detection only.

**Conclusion:** all push branches consult `or_condition_matches` directly when they need that decision — the cache is purely a delta-detection structure. Removing `.max(1)` is safe; no additional edits required.

**Tests:**
- `test_b2_parent_sizes_real_count` (was Wave 0 #[ignore] red-state stub) — flipped GREEN. Now a behavioral check: build ExistsOperator with `or_predicate(id == 1)` and zero children, hydrate, assert `parent_sizes_for_test()[pk] == 0` (real count, not max(1)=1). Includes a source-level guard against re-introducing the buggy literal (uses runtime-assembled needle to avoid include_str! self-match).
- `test_b2_push_add_after_empty_or_predicate_parent` (NEW, GREEN) — exercises the push path. After hydrate primes cache to 0, push child Add for the or_predicate parent and assert downstream change emitted (passthrough Child per TS exists.ts contract: or_predicate-positive parent stays in output via short-circuit at exists_op.rs:308-310).

**New test helper:** `parent_sizes_for_test(&self) -> &HashMap<String, usize>`, gated on `#[cfg(test)]`.

**Commit:** `1f99fd75b` — `fix(34-02): B2 — drop max(1) from parent_sizes cache in ExistsOperator::fetch`

### Task 3 — Differential test for B1 + B2 shapes

**File:** `tools/ivm-parity/diff-tests-track2.ts` (499 lines, NEW).

**Purpose:** TS↔RS parity verification for the post-fix B1 and B2 shapes against running zero-cache instances on `:4858` (TS reference) and `:4868` (Rust). Per CONTEXT D-18, this is the second leg of the no-regressions mandate alongside the Rust unit tests.

**Three shapes:**

1. **B1 (b1-skip-exists)** — `messages.where(EXISTS(attachments)).start({...})`. Verifies the post-fix Source → Skip → Exists ordering produces TS-equivalent payload.
2. **B2 hydrate (b2-or-exists-hydrate)** — `channels.where(OR(visibility=public, EXISTS(conversations)))`. Verifies the parent_sizes cache now holds the real child count for or_predicate-positive empty-children parents.
3. **B2 push (b2-or-exists-mutation)** — insert a conversation for `x-ch-empty` (the seed corpus's empty-children parent), wait for replicator quiescence, re-hydrate both caches, assert byte-equal payloads. Mutation undone in `finally{}` regardless of outcome.

**Implementation choices:**
- `subscribeAndHydrate` / `summarizeDiff` / `hashRows` / `canon` are duplicated from `harness-coverage.ts:100-290` rather than exported. This keeps `harness-coverage.ts` untouched per SKILL.md hard rule #2 (don't modify the apparatus to hide divergence; safest interpretation is full duplication for an auditable test).
- B2 push verification is hash-equality after the same mutation, NOT a per-poke row-changes capture (which would require adopting `harness-advance-coverage.ts:279-396`'s subscribe+drain pattern). For plan 34-02's acceptance bar, hash-equality after the same mutation proves the operators converge to the same state. Documented inline as a follow-up extension point.
- Configurable via env (`PARITY_TS_URL`, `PARITY_RS_URL`, `PARITY_PG_URL`, `HYDRATION_TIMEOUT_MS`, `POKE_QUIESCE_MS`). CLI flags `--skip-mutation`, `--verbose`. Exit codes: 0 on parity, 1 on divergence, 2 on config error.

**npm script:** `tools/ivm-parity/package.json` adds `"diff:track2": "tsx diff-tests-track2.ts"`.

**Cache startup procedure (documented in script header + below):**
```bash
# 1. Postgres (parity DB on :6434):
cd apps/zbugs && npm run db-up
cd ../../tools/ivm-parity
npm run db-create && npm run db-migrate

# 2. TS cache on :4858 — runs from /private/tmp/ivm-parity-ts-ref worktree:
./run.sh

# 3. RS cache on :4868:
npm run start-rs

# 4. Wait a few seconds for replicators to sync the seed.

# 5. Run differential tests:
npm run diff:track2
```

**Commit:** `23eb15c9f` — `test(34-02): differential tests for B1 + B2 shapes (diff-tests-track2.ts)`

## Verification

| Check | Result |
| --- | --- |
| `cd packages/zqlite-rs && cargo test --release --lib` | **130 passed**, 0 failed, 4 ignored (B1-CAVEAT new + B3 ast/take + B11 + 1 pre-existing). +1 from B1 stub flipping green. |
| `cd packages/zero-ivm-rs && cargo test --release --lib` | **193 passed**, 0 failed, 1 ignored (B3 take_op secondary, plan 34-05). +2 from B2 stub flipping green and new test_b2_push_add_after_empty_or_predicate_parent. |
| `cargo test --release --lib -- --ignored` (zqlite-rs) | 4 passed (B1-CAVEAT, B3 partition, B11 descendants, pre-existing) — all stubs still assert their intended state. |
| `cargo test --release --lib -- --ignored` (zero-ivm-rs) | 1 passed (B3 take_op) — stub intact. |
| `grep -n "// 2. Skip — moved here per B1" packages/zqlite-rs/src/ast_to_config.rs` | 1 line. |
| `grep -n "builder\.ts:302" packages/zqlite-rs/src/ast_to_config.rs` | 2 lines (B1 fix + B1 test docstring). |
| `grep -n "// B2: cache the REAL child count" packages/zero-ivm-rs/src/exists_op.rs` | 1 line. |
| `grep -n "exists\.ts" packages/zero-ivm-rs/src/exists_op.rs` | Multiple (test docstrings + production-code citation). |
| `grep -c "children\.len()\.max(1)" packages/zero-ivm-rs/src/exists_op.rs` (excluding test self-references in doc comments) | 0 occurrences in production code path; matches in doc comments are documentation of the historical bug. |
| `test -f tools/ivm-parity/diff-tests-track2.ts` | OK. |
| `grep -E "B1.*Skip\|b1-skip-exists" tools/ivm-parity/diff-tests-track2.ts` | 3 matches. |
| `grep -E "B2.*OR.*EXISTS\|b2-or-exists" tools/ivm-parity/diff-tests-track2.ts` | 3 matches. |
| `grep -q "compareChanges\|summarizeDiff" tools/ivm-parity/diff-tests-track2.ts` | OK (uses summarizeDiff). |
| `grep -q '"diff:track2"' tools/ivm-parity/package.json` | OK. |
| `npx tsx diff-tests-track2.ts --skip-mutation` (caches NOT running) | Parses + executes; reports ERROR per shape because TS/RS caches not started in this env (expected). End-to-end exit-0 verification requires both caches up. |

## Commits (atomic, with `--no-verify` per parallel-executor protocol)

| # | Hash         | Type | Subject |
| - | ------------ | ---- | ------- |
| 1 | `4fa50987e`  | fix  | B1 — emit Skip before conditions in ast_to_operator_configs |
| 2 | `1f99fd75b`  | fix  | B2 — drop max(1) from parent_sizes cache in ExistsOperator::fetch |
| 3 | `23eb15c9f`  | test | differential tests for B1 + B2 shapes (diff-tests-track2.ts) |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 — Blocking] B3 Wave 0 stub source-search window broken by B1 fix**
- **Found during:** Task 1 verification (`cargo test -- --ignored ast_to_config::tests::test_b3_partition_key_threading`).
- **Issue:** The Wave 0 B3 red-state stub asserts `partition_key: None,` exists within 300 chars after the `// 4. Limit -> Take` marker via `include_str!`. The B1 fix added a multi-line doc comment to the Take section, pushing the literal past the 300-char window — stub flipped to FAILED.
- **Fix:** Widened the source-search window from 300 to 600 chars in `test_b3_partition_key_threading`, with an inline comment documenting why. Stub still passes against the unchanged `partition_key: None,` literal. B3 (plan 34-05) work intact.
- **Files modified:** `packages/zqlite-rs/src/ast_to_config.rs` (test only).
- **Commit:** Folded into commit `4fa50987e`.

**2. [Rule 1 — Bug] B2 source-level guard self-matched include_str! string literal**
- **Found during:** Task 2 verification (running `test_b2_parent_sizes_real_count`).
- **Issue:** The test's regression-guard assertion `assert!(!src.contains("self.parent_sizes.insert(pk, children.len().max(1));"))` — the string literal in the assertion itself appears in the source file when read via `include_str!`, causing the test to falsely report the bug as "reintroduced."
- **Fix:** Built the search needle at runtime via `["self.parent_sizes.insert(pk, ", "children.len()", ".max(1));"].concat()` so the assembled string does not match the assertion's own source representation.
- **Files modified:** `packages/zero-ivm-rs/src/exists_op.rs` (test only).
- **Commit:** Folded into commit `1f99fd75b`.

### Authentication Gates

None.

### Out-of-Scope Discoveries (deferred)

- **B1 CAVEAT (CSQ vs Filter inner ordering inside an And):** RESEARCH §B1 explicitly notes that when an `And` wraps both a simple predicate and a CSQ, the inner relative ordering of the emitted Filter vs Exists configs may not match TS. `append_condition_configs::Condition::And` in `ast_to_config.rs` walks declaration order; TS does CSQ first then Filter. Documented as an `#[ignore]`'d test `test_b1_csq_then_filter_inside_and`. Tracking for a follow-up plan (potential Phase 35 candidate).

## Status of CAVEAT (from plan output spec)

**B1 CAVEAT — CSQ vs Filter inner ordering inside an And:**

- **Status:** Documented; not fixed in plan 34-02.
- **Test:** `ast_to_config::tests::test_b1_csq_then_filter_inside_and` (#[ignore]'d).
- **Spec source:** TS `packages/zql/src/builder/builder.ts:308-333` orders `csqConditions` first (line 308-329) then `applyWhere` (line 331-333). Rust `append_condition_configs::Condition::And` (lines 327-330) walks `conditions[]` in declaration order, so an AST with `where: { type: 'and', conditions: [{simple_pred}, {csq EXISTS}] }` produces `[Filter, Exists]` in Rust but `[Exists, Filter]` in TS.
- **Impact:** Architectural divergence; payload may still match at hydrate level if the operators are commutative for the specific shapes in the test corpus, but the pipeline observation order is wrong. The fuzzer (Track 1) will surface this if it generates such an And shape.
- **Follow-up:** Reorder `append_condition_configs::Condition::And` to emit CSQs first then simple predicates — small surgical change, but slightly invasive (changes operator-config order in production); deferred to keep plan 34-02 scope tight.

## Known Stubs

The B2 push regression test (`test_b2_push_add_after_empty_or_predicate_parent`) verifies the operator's BEHAVIOR (downstream change emitted, passthrough Child). It does NOT directly test the boundary check `old_count == 0 && new_count == 1` because the or_predicate short-circuit at exists_op.rs:308-310 fires first for or_predicate-positive parents, returning `vec![change]` before the boundary logic runs. This is the correct TS-mirroring behavior (TS also short-circuits or_predicate-positive parents). A test exercising the boundary check directly would need a parent that does NOT match or_predicate but transitions 0→1; that's adequately covered by the existing `test_exists_push_child_add_0_to_1_transition`.

## Threat Flags

None — Phase 34 plan 02 changes are surgical to internal Rust IVM (no new network surface, no auth path, no schema for production data). The differential test script in `tools/ivm-parity/` connects to the existing parity test database (port 6434) only.

## TDD Gate Compliance

Plan type is `execute` (not `tdd`), so RED/GREEN/REFACTOR gate sequence enforcement does not apply. However, the Wave 0 red-state stub philosophy (D-18) is itself TDD-like:
- **RED:** Wave 0 (commit b4b970719 from 34-01) wrote `#[ignore]` tests asserting BROKEN behavior.
- **GREEN:** This plan inverted the assertions and landed the source fixes in the SAME atomic commit per Wave 0 D-18 contract — `4fa50987e` and `1f99fd75b` each contain (1) the source fix, (2) the stub test inverted to assert the post-fix correct state, (3) at least one new behavioral regression test.
- **REFACTOR:** No additional refactor commits were needed; the fixes are surgical.

## Self-Check: PASSED

**Files modified (verified via `git diff --stat 7df368859..HEAD`):**
- `packages/zqlite-rs/src/ast_to_config.rs` (+108 / −36) — FOUND
- `packages/zero-ivm-rs/src/exists_op.rs` (+183 / −27) — FOUND
- `tools/ivm-parity/package.json` (+1) — FOUND

**Files created (verified via `ls`):**
- `tools/ivm-parity/diff-tests-track2.ts` (499 lines) — FOUND
- `.planning/phases/34-differential-fuzz-schema-extension/34-02-SUMMARY.md` — FOUND (this file)

**Commits verified via `git log 7df368859..HEAD`:**
- `4fa50987e` fix(34-02) B1 — FOUND
- `1f99fd75b` fix(34-02) B2 — FOUND
- `23eb15c9f` test(34-02) differential — FOUND

**Test runs verified:**
- zqlite-rs: 130 passed, 0 failed (was 129 baseline; +1 from B1 flipping green).
- zero-ivm-rs: 193 passed, 0 failed (was 191 baseline; +2 from B2 flipping green + new B2 push test).
- All ignored tests still pass against their intended assertion state (B3 ast/take, B11, B1-CAVEAT, plus 1 pre-existing).

All claims verified. No missing files, no missing commits.
