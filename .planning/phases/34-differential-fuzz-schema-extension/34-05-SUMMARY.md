---
phase: 34
plan: 34-05
slug: track-2-b3-headline-fix
subsystem: Rust IVM (zqlite-rs ast_to_config + zero-ivm-rs take_op) + tools/ivm-parity diff tests
tags: [phase-34, track-2, hardening, b3, headline-fix, wave-2, partition-key]
dependency_graph:
  requires:
    - 34-CONTEXT.md (D-15 in-scope BLOCKING fixes; D-17 TS-as-spec; D-18 no regressions)
    - 34-RESEARCH.md (B3 fix specification: 4 coordinated edits + RUST take_op state-key audit)
    - 34-01-SUMMARY.md (Wave 0 #[ignore] red-state stubs for B3 in ast_to_config + take_op)
    - 34-02-SUMMARY.md (Wave 1 baseline: B1 + B2 fixes; diff-tests-track2.ts existing structure)
    - IVM-PORT-AUDIT-DEEP.md §B3 (silent push-as-no-op for child Takes under related-with-limit)
    - IVM-PORT-AUDIT.md §Risk #1 (partition_key not threaded — same root cause)
  provides:
    - B3 fixed: ast_to_operator_configs accepts partition_key parameter; child Take inherits parent's correlation.childField as partition_key — closes IVM-PORT-AUDIT-DEEP §B3 + IVM-PORT-AUDIT Risk #1 simultaneously.
    - B3 fixed: Take fetch/push state keys equal when constraint values match the row's partition columns — no more silent push-as-no-op.
    - take_op.rs fetch fallback retains safety net + promoted assert! per AUDIT-03 discipline.
    - 2 Wave 0 ignored stubs flipped GREEN (test_b3_partition_key_threading + test_b3_partition_state_consistency).
    - 1 NEW focused test added (test_b3_push_emits_change_for_constrained_child_take).
    - tools/ivm-parity/diff-tests-track2.ts extended with B3 related-with-limit + child mutation case.
  affects:
    - packages/zqlite-rs/src/ast_to_config.rs (signature change + 4 internal recursive call sites + apply_exists_limit + B3 stub flipped + Take config)
    - packages/zqlite-rs/src/advance.rs (sole external test caller updated to pass None)
    - packages/zero-ivm-rs/src/take_op.rs (assertion promoted + B3 stub flipped + new focused regression)
    - tools/ivm-parity/diff-tests-track2.ts (4th test case [4/4] + b3Ast + B3 mutation block + script header updated)
tech_stack:
  added: []
  patterns:
    - TS-as-spec citation in code comments (// mirrors TS builder.ts:626-632)
    - Source-level GREEN-state assertion (replaces Wave 0 RED-state include_str! stub)
    - Promoted assertion pattern (debug_assert! → assert!) per AUDIT-03 / Risk #2 discipline
    - Hash-equality-after-mutation differential test pattern (extended from B2 push)
key_files:
  created:
    - .planning/phases/34-differential-fuzz-schema-extension/34-05-SUMMARY.md
  modified:
    - packages/zqlite-rs/src/ast_to_config.rs (+114 / −31 lines)
    - packages/zqlite-rs/src/advance.rs (+5 / −0 lines)
    - packages/zero-ivm-rs/src/take_op.rs (+148 / −50 lines)
    - tools/ivm-parity/diff-tests-track2.ts (+183 / −8 lines)
decisions:
  - All 4 internal recursive call sites in ast_to_config.rs (related[] at line ~263, plus 3 EXISTS sites at ~385, ~429, ~553) pass `Some(related.correlation.child_field.clone())` — mirrors TS builder.ts:626-632 which always passes `sq.correlation.childField` for ANY CSQ recursion. The plan's "EXISTS parent_field" language was reconciled to child_field after careful re-read of TS spec.
  - apply_exists_limit gained a 3rd partition_key parameter — its inserted Take (when no limit was set on the EXISTS subquery's AST) was previously hard-coded `partition_key: None`, the same bug as the related[] Take site. All 3 callers pass Some(related.correlation.child_field.clone()).
  - take_op.rs fetch path audit: at lines 286-289, when self.partition_key.is_some() AND constraint is present, the existing code already correctly walks self.partition_key (builds Row from constraint, calls take_state_key) — no fetch logic change needed. The legacy fallback at lines 319-345 (None partition + constraint) is now reachable only on a programmer error, surfaced by the promoted `assert!` per AUDIT-03 discipline.
  - The B3 stub in take_op.rs (test_b3_partition_state_consistency) was flipped from include_str!-based source assertion to a behavioral assertion: build TakeOperator with Some(["channelId"]); assert fetch-derived key (constraint→Row→take_state_key) equals push-derived key (row→take_state_key). This is the canonical TS take.ts:710-757 invariant.
  - Diff test mutation target: m-3 (seed.sql:41, conversationId='co-2', body='standup notes'). co-2 has 2 messages (m-3, m-4) — both within limit=5, so the post-mutation hydrate must include the edited body, exposing any silent push-as-no-op.
metrics:
  duration_seconds: 1320
  duration_human: ~22 minutes
  completed_date: 2026-04-29
  tasks_completed: 3
  files_created: 1
  files_modified: 4
  total_lines_added: ~450
---

# Phase 34 Plan 05: Track 2 B3 Headline Fix Summary

**One-liner:** Land Track 2's headline fix B3 — thread `partition_key` through `ast_to_operator_configs` so child Takes inherit the parent's `correlation.childField` as their partition. Closes silent push-as-no-op for every related-subquery with `limit` AND IVM-PORT-AUDIT Risk #1 simultaneously. All Wave 0 B3 stubs flip GREEN; new B3 differential test covers `conversations.related(messages.limit(5))` + child mutation.

## Objective Recap

Per CONTEXT D-15 (BLOCKING in-scope) + D-17 (TS-as-spec) + D-18 (no regressions): land the headline B3 fix with full test coverage, including a proper push-side behavioral test (not just source inspection) and a TS↔RS differential test for the canonical limited-related shape.

Per RESEARCH "B3 is the headline fix" — every `related[]` subquery with `limit` silently mishandles child-table edits/adds/removes during advance because Rust's `take_state_key(&row)` returns `"[\"take\"]"` for `partition_key: None` while fetch built per-constraint keys. State written by hydrate was invisible to push.

The fix mandate: thread `partition_key: Option<Vec<String>>` through `ast_to_operator_configs` so child Takes carry the parent's `correlation.childField` (per TS `builder.ts:626-632`). Both fetch AND push then use the same state-key derivation through `take_state_key`.

## What Shipped

### Task 1 — Thread partition_key through ast_to_operator_configs

**Sites modified:** `packages/zqlite-rs/src/ast_to_config.rs` + `packages/zqlite-rs/src/advance.rs`.

**Signature change:**
```rust
// ast_to_config.rs:196-211
pub fn ast_to_operator_configs(
    schema: &mut SchemaCache,
    ast: &Ast,
    primary_key: &[String],
    // B3: partition_key threaded per TS builder.ts:260-261. ...
    partition_key: Option<Vec<String>>,
) -> Result<Vec<OperatorConfig>, String>
```

**4 internal recursive call sites updated** (all pass `Some(related.correlation.child_field.clone())` — mirrors TS `builder.ts:626-632` which always uses `sq.correlation.childField`):

| Site | Function | Line (post-fix) | Context |
|------|----------|-----------------|---------|
| 1 | top-level related[] | ~272-279 | `ast.related[].subquery` recursion |
| 2 | append_condition_configs (CSQ) | ~411-418 | direct EXISTS condition |
| 3 | append_csq_as_exists | ~464-471 | OR(simple, EXISTS) — single-CSQ branch |
| 4 | collect_exists_branches | ~596-603 | OrExists multi-CSQ branch |

**Take config (line ~257-264):** changed from hard-coded `partition_key: None` to `partition_key: partition_key.clone()`.

**apply_exists_limit (line ~640-672):** gained a 3rd parameter `partition_key: Option<Vec<String>>`. Was previously hard-coding `partition_key: None` for its inserted EXISTS_LIMIT Take — same silent push-as-no-op bug. All 3 callers pass `Some(related.correlation.child_field.clone())`.

**External caller (advance.rs:1108-1117):** updated to pass `None` at top level (top-level callers always pass None — mirrors TS `buildPipelineInternal` initial invocation).

**Wave 0 stub flip:** `test_b3_partition_key_threading` (formerly `#[ignore]` red-state) inverted to assert the post-fix invariants: signature carries the new parameter, Take config threads `partition_key.clone()`, related-recursion site passes `Some(rel.correlation.child_field.clone())`. Source-level structural verification (sidesteps SchemaCache db_path coupling per Wave 0 D-18 pattern).

**Commit:** `12f4d43cc` — `fix(34-05): B3 — thread partition_key through ast_to_operator_configs`

### Task 2 — take_op.rs state-key consistency + new focused regression

**Site:** `packages/zero-ivm-rs/src/take_op.rs`.

**Audit findings (per plan Task 2 step 3):**
- `take_op.rs:286-289` fetch path: when `self.partition_key.is_some()` AND constraint is present, code already correctly builds a Row from constraint and calls `take_state_key(&m)` — same path as push. **No change needed.** Keys agree post-Task-1.
- `take_op.rs:319-345` legacy fallback (None partition + constraint): pre-Phase-34 this was the ONLY path that built per-constraint keys for child Takes; push silently no-oped. Post-Phase-34 plan 34-05 makes this branch unreachable from correctly-built pipelines because all CSQ-recursion sites in `ast_to_config` thread `partition_key`.

**Change 1 — promoted assertion** (per AUDIT-03 / Risk #2 discipline: framework invariants are `assert!` not `debug_assert!`):
```rust
assert!(
    self.partition_key.is_some() || c.columns.is_empty(),
    "Take fetch: partition_key not threaded for constrained child \
     Take. See .planning/IVM-PORT-AUDIT-DEEP.md §B3 and \
     .planning/IVM-PORT-AUDIT.md Risk #1."
);
```
Surfaces a missed call site in release builds, not just debug. The `c.columns.is_empty()` guard accommodates the rare degenerate case of a constraint with zero columns (where the legacy fallback's key reduces to `["take"]`).

**Change 2 — Wave 0 stub flip:** `test_b3_partition_state_consistency` inverted from RED-state `include_str!` source check to GREEN-state behavioral check:
```rust
let push_key = op.take_state_key(&row);  // partition Some(["channelId"]), row {channelId: "ch-1"}
let fetch_row: Row = constraint.columns.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
let fetch_key = op.take_state_key(&fetch_row);
assert_eq!(fetch_key, push_key);  // B3 (TS take.ts:710-757)
```

**Change 3 — NEW focused regression `test_b3_push_emits_change_for_constrained_child_take`:** mirrors TS `take.ts:219` push semantics end-to-end. Build TakeOperator(partition=["channelId"]); fetch with constraint `{channelId: "ch-1"}` to prime state under `["take","ch-1"]`; push Add for a third row in the same partition; assert non-empty result. Pre-fix this returned `vec![]` (state miss). Post-fix returns the Add transition.

**Commit:** `3d6b32b43` — `test(34-05): B3 — flip take_op secondary stub green, add push regression`

### Task 3 — diff-tests-track2.ts B3 differential test

**Site:** `tools/ivm-parity/diff-tests-track2.ts`.

**New AST shape (b3Ast):**
```ts
{
  table: 'conversations',
  orderBy: [['id', 'asc']],
  related: [{
    correlation: { parentField: ['id'], childField: ['conversationId'] },
    subquery: { table: 'messages', alias: 'b3_thread', orderBy: [['id','asc']], limit: 5 },
  }],
}
```
This is exactly the canonical TS-spec partition_key shape (TS `builder.ts:626-632`) — the child Take inside the related subquery gets `partition_key: ["conversationId"]` post-fix.

**Test flow [4/4] B3 push:**
1. Precondition probe: `SELECT id, body FROM messages WHERE id = 'm-3'` — fail loudly (per threat T-34-14) if absent.
2. Pre-mutation diff (`diffTest('b3-related-limit-pre-mutation', b3Ast, ...)`) — TS↔RS hash-equal baseline.
3. Apply mutation: `UPDATE messages SET body = 'b3-track2-edited-<ts>' WHERE id = 'm-3'`.
4. Wait `POKE_QUIESCE_MS` for replicators.
5. Post-mutation diff (`b3-related-limit-post-mutation`) with a fresh hash to avoid CVR cache reuse.
6. Sanity check: m-3 must appear in TS payload AND its `body` must equal the edited value (else mutation didn't propagate — false-positive guard).
7. Restore in `finally{}`: `UPDATE messages SET body = '<original>' WHERE id = 'm-3'`. Logs warning on cleanup failure with manual-fix instructions.

**Mutation target:** `m-3` (seed.sql:41, conversationId='co-2', body='standup notes'). Verified: co-2 has 2 messages (m-3, m-4) — both within limit=5, so the edited body MUST appear in the hydrated payload, exposing any silent push-as-no-op.

**Step counter and header updated:** `[1/3] B1`, `[2/3] B2 hydrate`, `[3/3] B2 push` → `[1/4] B1`, `[2/4] B2 hydrate`, `[3/4] B2 push`, `[4/4] B3 push`. Output banner reads "B1 + B2 + B3".

**Smoke test (caches not running locally):** `npx tsx diff-tests-track2.ts --skip-mutation` — exits 1 (B1+B2 hydrate steps throw because caches absent), structure intact, B3 step shows `SKIPPED (--skip-mutation)`. End-to-end exit-0 verification requires both caches up per script header.

**Commit:** `d29114f73` — `test(34-05): B3 — extend diff-tests-track2.ts with related-with-limit child mutation`

## Resolution of apply_exists_limit recursive call sites' partition_key choice

Per plan Task 1 step 4: each `apply_exists_limit` call site received `Some(related.correlation.child_field.clone())`. Reasoning:

- TS spec `builder.ts:626-632` always passes `sq.correlation.childField` for any subquery recursion — **no exceptions**, including EXISTS contexts.
- The plan's "EXISTS parent_field" language was a typo; the correct mirror is child_field. Verified by re-reading TS `builder.ts:308-329` (the EXISTS_LIMIT downgrade context):
  ```ts
  // builder.ts ~330-345
  return new Take(end, ..., sq.correlation.childField);  // childField, not parentField
  ```
- Each of the 3 EXISTS recursion sites operates on a `related: Box<CorrelatedSubquery>`, so the immediately-adjacent partition is `related.correlation.child_field`.

This decision is robust against nested EXISTS shapes: each level passes its OWN parent's child_field, never the grandparent's. The 4 sites are independent.

## take_op.rs fetch fallback audit findings

Per plan Task 2 step 3:
- The fetch path at lines 286-318 (partition_key.is_some() branch) **already** correctly derives the state key by walking `self.partition_key` (line 288 builds Row from constraint, line 289 calls `take_state_key(&m)` which uses partition_key columns). **Push-side (line 391) calls `take_state_key(&change.node().row)` — same shape.** Keys match exactly when constraint values equal the row's partition columns.
- The legacy fallback at lines 319-345 (partition_key.is_none() + constraint) **stays as-is** as a safety net. The promoted `assert!` ensures a missed call site surfaces immediately.
- No fetch logic restructure was required. The plan's contingency ("if the existing fetch path STILL constructs `["take", colName, colValue]` even when partition_key is Some, fix the fetch") did not apply — the existing code already handled this path correctly.

## Diff test mutation target verification

The chosen mutation target `m-3` is verified to exist in `tools/ivm-parity/seed.sql`:
```sql
-- seed.sql:41
('m-3',  'co-2', 'u1', 'standup notes',  2100, NULL),
```

Verification commands (when PG is up):
```bash
PGPASSWORD=password psql -h 127.0.0.1 -p 6434 -U user -d parity -c "SELECT id, body FROM messages WHERE id = 'm-3';"
PGPASSWORD=password psql -h 127.0.0.1 -p 6434 -U user -d parity -c "SELECT COUNT(*) FROM messages WHERE \"conversationId\" = 'co-2';"
# expected: 2 (m-3, m-4) — both within limit=5
```

The B3 test's runtime precondition (`SELECT id, body FROM messages WHERE id = 'm-3'`) catches any future seed.sql refactor that drops or renames m-3 — failing loudly with an explanatory message rather than silently passing.

## Verification

| Check | Result |
| --- | --- |
| `cd packages/zqlite-rs && cargo test --release --lib` | **131 passed**, 0 failed, 3 ignored. (Was 130 + 4 ignored — Wave 0 B3 ast_to_config stub flipped GREEN.) |
| `cd packages/zero-ivm-rs && cargo test --release --lib` | **195 passed**, 0 failed, **0 ignored**. (Was 193 + 1 ignored — Wave 0 B3 take_op stub flipped GREEN, new focused B3 push test added, plus B2 stub previously flipped in 34-02.) |
| `cd packages/zqlite-rs && cargo test --release --lib -- --ignored` | 3 passed (B1-CAVEAT, B11 descendants, streaming companion — none B3). |
| `cd packages/zero-ivm-rs && cargo test --release --lib -- --ignored` | 0 tests (all flipped). |
| `cargo test --release --lib ast_to_config::tests::test_b3_partition_key_threading` | 1 passed. |
| `cargo test --release --lib take_op::tests::test_b3_partition_state_consistency` | 1 passed. |
| `cargo test --release --lib take_op::tests::test_b3_push_emits_change_for_constrained_child_take` | 1 passed. |
| `grep -E "partition_key: Option<Vec<String>>" packages/zqlite-rs/src/ast_to_config.rs` | 2 lines (signature + apply_exists_limit). |
| `grep -c "partition_key" packages/zqlite-rs/src/ast_to_config.rs` | 14 occurrences (parameter + Take field + 4 recursive sites + apply_exists_limit + tests). |
| `grep -E "builder\.ts:626\|builder\.ts:341\|builder\.ts:260" packages/zqlite-rs/src/ast_to_config.rs` | 5+ TS-as-spec citations. |
| `grep -q "partition_key: None,\\s*$" packages/zqlite-rs/src/ast_to_config.rs` (production code) | 0 — no remaining hard-coded None at the Take config sites. |
| `grep -q "ast_to_operator_configs.*None" packages/zqlite-rs/src/advance.rs` | OK — test caller passes None explicitly. |
| `grep -q "assert!(self.partition_key.is_some()" packages/zero-ivm-rs/src/take_op.rs` | OK — promoted assertion present. |
| `grep -E "AUDIT-03\|Risk #2\|Risk #1" packages/zero-ivm-rs/src/take_op.rs` | OK — citations present. |
| `grep -q "test_b3_push_emits_change_for_constrained_child_take" packages/zero-ivm-rs/src/take_op.rs` | OK — new focused test added. |
| `grep -E "b3.*related.*limit\|b3_thread\|B3 differential test" tools/ivm-parity/diff-tests-track2.ts` | 5 matches. |
| `grep -E "UPDATE messages.*WHERE id" tools/ivm-parity/diff-tests-track2.ts` | 3 matches (mutation + restore + warn-on-cleanup-fail). |
| `grep -q "RESEARCH.md §B3" tools/ivm-parity/diff-tests-track2.ts` | OK — TS-as-spec citation. |
| `npx tsx diff-tests-track2.ts --skip-mutation` (caches not running) | Parses cleanly, executes B1+B2 (ERROR — expected without caches), B2+B3 push SKIPPED, exits 1. End-to-end exit-0 verification requires both caches up. |

## Commits (atomic, with `--no-verify` per parallel-executor protocol)

| # | Hash | Type | Subject |
| - | ---- | ---- | ------- |
| 1 | `12f4d43cc` | fix  | B3 — thread partition_key through ast_to_operator_configs |
| 2 | `3d6b32b43` | test | B3 — flip take_op secondary stub green, add push regression |
| 3 | `d29114f73` | test | B3 — extend diff-tests-track2.ts with related-with-limit child mutation |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 — Critical correctness] apply_exists_limit silent push-as-no-op (same B3 bug, secondary site)**
- **Found during:** Task 1 step 4 (audit of `apply_exists_limit` callers).
- **Issue:** `apply_exists_limit` (line 617 pre-fix) hard-coded `partition_key: None` for the Take it inserts when an EXISTS subquery has no explicit `limit`. This was the SAME bug as the related[] Take site — but the plan's spec (Task 1 step 4) only required updating the recursive `ast_to_operator_configs` call sites, not changing `apply_exists_limit` itself. Without also threading partition_key into `apply_exists_limit`, EXISTS-with-no-limit shapes (the EXISTS_LIMIT downgrade) would still silently no-op on push.
- **Fix:** Added a 3rd `partition_key: Option<Vec<String>>` parameter to `apply_exists_limit`. All 3 callers (in `append_condition_configs`, `append_csq_as_exists`, `collect_exists_branches`) pass `Some(related.correlation.child_field.clone())` — the same value they pass to the recursive `ast_to_operator_configs` call.
- **Files modified:** `packages/zqlite-rs/src/ast_to_config.rs` (apply_exists_limit signature + 3 call sites).
- **Commit:** Folded into commit `12f4d43cc`.

**2. [Rule 1 — Bug] Plan's "EXISTS parent_field" language was a typo**
- **Found during:** Task 1 step 4 (re-read of TS spec).
- **Issue:** The plan's Task 1 action step 4 said "Per RESEARCH: each `apply_exists_limit` call site should pass the EXISTS parent_field". Re-reading TS `builder.ts:626-632`, every CSQ recursion in TS passes `sq.correlation.childField` — no exceptions. The plan's "parent_field" was a typo; the correct mirror is child_field.
- **Fix:** Used `Some(related.correlation.child_field.clone())` at all 4 ast_to_operator_configs recursion sites + 3 apply_exists_limit call sites.
- **Files modified:** none (this was a planning interpretation, not a code change beyond what Task 1 already required).
- **Commit:** N/A — caught before any code committed with the wrong field.

### Authentication Gates

None.

### Out-of-Scope Discoveries (deferred)

None observed. The plan's spec was complete and self-consistent (modulo the parent_field/child_field typo above). The fix is surgical; full Rust test suites green; no Wave 1 baseline regressions.

## Tests Investigated per RESEARCH Pitfall 5

RESEARCH §B3 pitfall 5: "B3 fix surfaces existing tests that relied on the buggy 'global Take state shared across constraints' — investigate any newly-failing test as latent buggy expectation per D-17, do not work around."

**Test status post-fix:**
- zqlite-rs: 131 passed (was 130 baseline + 1 from B3 stub flipping). No tests newly failed.
- zero-ivm-rs: 195 passed (was 193 + 2 from B2 stub flipping in 34-02 + B3 stub flipping here + new B3 push test). No tests newly failed.

The pre-existing AUDIT-03 promoted-assertion regression tests (`test_take_duplicate_pk_in_inside_outside_branch_panics` + `test_take_duplicate_pk_in_outside_outside_branch_panics`) still pass with `should_panic` — confirming the duplicate-PK assertion logic is unaffected.

The `test_max_bound_updated_across_partitions` test (which exercises multi-partition state) and `test_push_to_unknown_partition_ignored` both passed without modification — partition handling for the explicit Some(partition_key) path was already correct; the B3 fix only changed which paths are reached, not what they do.

## Known Stubs

None introduced by this plan. All Wave 0 B3 stubs (one in `ast_to_config.rs`, one in `take_op.rs`) are now GREEN with behavioral assertions.

The diff-tests-track2.ts B3 case follows the same hash-equality-after-mutation pattern as the existing B2 push case (a per-poke row-changes capture would require adopting harness-advance-coverage.ts:279-396's subscribe+drain pattern). For Phase 34 plan 34-05 scope, hash equality after the same mutation proves the operators converge to the same state — sufficient to verify the partition_key threading. Documented in the script as a follow-up extension point (alongside the same note for B2 push).

## Threat Flags

None — Phase 34 plan 34-05 changes touch only internal Rust IVM (no new network surface, no auth path, no schema for production data) plus the parity test apparatus in `tools/ivm-parity/`. The diff test connects only to the existing parity database (port 6434).

The threat model items from the plan:
- **T-34-12 (latent buggy expectations on global Take state)** — mitigated. No tests required modification; full suites green.
- **T-34-13 (promoted assert! firing in production)** — mitigated. Task 1 audited all 4 internal recursive sites + apply_exists_limit; the assertion fires only on programmer error (forgotten call site), not user input.
- **T-34-14 (B3 diff test picks a message ID not in any conversation's top-5 — silently passes)** — mitigated. The runtime precondition `SELECT id, body FROM messages WHERE id = 'm-3'` fails loudly if the row is absent, AND the post-mutation sanity check confirms the edited body appears in the hydrated payload.

## TDD Gate Compliance

Plan type is `execute` (not `tdd`), so RED/GREEN/REFACTOR gate sequence enforcement does not apply. However, the Wave 0 red-state stub philosophy (D-18) is itself a TDD-like pattern:
- **RED:** Wave 0 (commits b4b970719 from 34-01) wrote `#[ignore]` tests asserting BROKEN behavior — `partition_key: None,` literal in the Take config branch + `"[\"take\"]"` global bucket constant in take_state_key.
- **GREEN:** This plan inverted both stubs and landed the source fix in coordinated atomic commits per Wave 0 D-18 contract:
  - Commit `12f4d43cc` contains (1) the source fix (signature + 4 recursive sites + apply_exists_limit) and (2) the ast_to_config stub inverted to assert post-fix invariants.
  - Commit `3d6b32b43` contains (1) the take_op safety-net assertion promotion, (2) the take_op stub inverted to assert key consistency behaviorally, and (3) the new focused B3 push regression test.
  - Commit `d29114f73` adds the differential test for the canonical TS-spec shape.
- **REFACTOR:** No additional refactor commits needed; the fixes are surgical.

## Self-Check: PASSED

**Files modified (verified via `git diff --stat 21fad59a8..HEAD`):**
- `packages/zqlite-rs/src/ast_to_config.rs` (+114/−31) — FOUND
- `packages/zqlite-rs/src/advance.rs` (+5) — FOUND
- `packages/zero-ivm-rs/src/take_op.rs` (+148/−50) — FOUND
- `tools/ivm-parity/diff-tests-track2.ts` (+183/−8) — FOUND

**Files created (verified via `ls`):**
- `.planning/phases/34-differential-fuzz-schema-extension/34-05-SUMMARY.md` — FOUND (this file)

**Commits verified via `git log 21fad59a8..HEAD`:**
- `12f4d43cc` fix(34-05) B3 partition_key threading — FOUND
- `3d6b32b43` test(34-05) B3 take_op stub flip + push regression — FOUND
- `d29114f73` test(34-05) B3 differential test — FOUND

**Test runs verified:**
- zqlite-rs: 131 passed, 3 ignored (was 130 + 4 ignored baseline; +1 from B3 ast_to_config stub flipping green).
- zero-ivm-rs: 195 passed, 0 ignored (was 193 + 1 ignored Wave 1 baseline; +2 from B3 take_op stub flipping + new B3 push test).
- All Wave 0 B3 stubs are now GREEN. Remaining ignored tests in zqlite-rs are the B1-CAVEAT (deferred per 34-02), B11 descendants (deferred to plan 34-06), and the streaming companion test (pre-existing).

All claims verified. No missing files, no missing commits.
