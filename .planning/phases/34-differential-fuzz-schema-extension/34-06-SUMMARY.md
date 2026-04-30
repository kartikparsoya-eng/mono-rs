---
phase: 34
plan: 34-06
slug: track-2-b11-prev-snapshot-cascade-delete
subsystem: Rust IVM (zqlite-rs pipeline_manager + advance) + zero-cache pipeline-driver TS + tools/ivm-parity diff tests
tags: [phase-34, track-2, hardening, b11, cascade-delete, wave-2, prev-snapshot]
dependency_graph:
  requires:
    - 34-CONTEXT.md (D-15 in-scope BLOCKING fixes; D-17 TS-as-spec; D-22 / CLAUDE.md gate #4 — no signature change to existing buffered methods)
    - 34-RESEARCH.md (B11 fix specification — Option B preferred: additive napi method `set_prev_snapshot`)
    - 34-01-SUMMARY.md (Wave 0 #[ignore] red-state stub for B11 in advance.rs)
    - 34-02-SUMMARY.md (Wave 1 baseline B1 + B2 + diff-tests-track2.ts existing structure)
    - 34-05-SUMMARY.md (B3 fix; diff-tests-track2.ts pattern for [N/M] step counter)
    - IVM-PORT-AUDIT-DEEP.md §B11 (silent same-tx descendant elision via post-swap snapshot read)
  provides:
    - B11 fixed: emit_descendant_removals reads from PREV snapshot via additive set_prev_snapshot napi method.
    - PipelineInstance struct gained `prev_db_path: Option<String>` field (Mutex-protected via instance lock).
    - RustPipelineManager gained `#[napi] pub fn set_prev_snapshot(id, prev_db_path)` — ADDITIVE method, no signature change to existing buffered methods (CLAUDE.md gate #4 preserved).
    - emit_descendant_removals signature: `db_path: &str` → `prev_db_path: &str`. All 4 internal callers updated.
    - advance_persistent_pipeline + advance_persistent_pipeline_with_cancel both now accept `(db_path, prev_db_path)` — db_path remains for child_row_has_parent (curr/post-tx state required to verify Add parents); prev_db_path is used exclusively for descendant SQL reads.
    - TS pipeline-driver.ts calls `setPrevSnapshot(prev.db.db.name)` BEFORE every `swapSnapshot(curr.db.db.name)` on all 3 advance entry points (#rustAdvanceStreaming, #rustAdvanceAsync, #rustAdvance).
    - Wave 0 stub `test_b11_descendants_from_prev` flipped from red-state (#[ignore]) to GREEN.
    - 2 NEW source-level tests in advance.rs (test_b11_set_prev_snapshot_napi_present, test_b11_fallback_when_prev_not_set).
    - tools/ivm-parity/diff-tests-track2.ts extended with [5/5] B11 cascade-delete case (channels → conversations → messages multi-table tx).
  affects:
    - packages/zqlite-rs/src/pipeline_manager.rs (struct field + #[napi] method + 2 entry-point reads + 2 advance-call-site updates)
    - packages/zqlite-rs/src/advance.rs (emit_descendant_removals signature + 4 internal callers + 1 recursive self-call + advance_persistent_pipeline + advance_persistent_pipeline_with_cancel signatures + RustPipeline legacy caller + bench/test callers + Wave 0 stub flip)
    - packages/zero-cache/src/services/view-syncer/pipeline-driver.ts (interface + 3 setPrevSnapshot insertions before swapSnapshot)
    - tools/ivm-parity/diff-tests-track2.ts ([5/5] case + b11Ast + cascade test block + step counter renumber 1/4 → 1/5 etc.)
tech_stack:
  added: []
  patterns:
    - Additive napi method preserves CLAUDE.md gate #4 (no signature change to existing buffered methods)
    - TS-as-spec citation in code comments (// per TS pipeline-driver.ts:1542-1577)
    - Lifecycle-pitfall comment block (// MUST happen before swapSnapshot on every advance)
    - Logged-fallback for back-compat (eprintln + clone db_path when prev_db_path None)
    - GREEN-state source-level assertion replaces Wave 0 RED-state include_str! stub
    - Hash-equality-after-cascade-delete differential test pattern (extends B2/B3 push pattern)
key_files:
  created:
    - .planning/phases/34-differential-fuzz-schema-extension/34-06-SUMMARY.md
  modified:
    - packages/zqlite-rs/src/pipeline_manager.rs (+50 / −2 lines across Tasks 1a + 1b — field + napi method + 2 entry-point reads + 2 call-site updates)
    - packages/zqlite-rs/src/advance.rs (+128 / −66 lines — signature changes + 4+1 caller updates + RustPipeline legacy site + 3 bench/test callers + Wave 0 stub flip + 2 new tests)
    - packages/zero-cache/src/services/view-syncer/pipeline-driver.ts (+29 / −0 lines — interface decl + 3 setPrevSnapshot insertions)
    - tools/ivm-parity/diff-tests-track2.ts (+228 / −10 lines — header + b11Ast + cascade-delete test block + step counter renumber)
decisions:
  - "Option B (additive napi method) chosen over Option A (modifying existing advance signature) to preserve CLAUDE.md gate #4. Verified by `git diff packages/zqlite-rs/src/pipeline_manager.rs | grep -E '^-.*pub fn (advance|advance_async|advance_streaming|hydrate|addQuery|addQueries)' | wc -l` returning 0."
  - "Task 1a/1b split per checker scope-sanity: Task 1a delivered struct field + napi method + entry-point reads with `let _ = &prev_db_path;` placeholder. Task 1b removed placeholder by threading prev_db_path through advance_persistent_pipeline + advance_persistent_pipeline_with_cancel into emit_descendant_removals. Reviewable diffs at each commit boundary."
  - "PipelineInstance.prev_db_path is Option<String> (NOT Mutex<Option<String>>) because the field is only ever accessed under the instance Arc<Mutex<PipelineInstance>> lock — adding an inner Mutex would be redundant. set_prev_snapshot acquires the instance mutex and writes; advance entry points acquire the same mutex and read. Single-thread-of-mutation guarantee preserved."
  - "Legacy RustPipeline class (pre-PipelineManager) and bench/test sites within advance.rs pass `&db_path, &db_path` for both params — preserves pre-fix behavior in the legacy code path (mono-rs production uses RustPipelineManager which DOES wire prev_db_path via setPrevSnapshot). This is intentional: the legacy class has no prev_db_path setter; refactoring it would expand scope beyond Plan 34-06."
  - "child_row_has_parent (Add path verifies parent exists in post-tx state) intentionally KEEPS db_path — only emit_descendant_removals (Remove cascade enumeration) needs prev_db_path. This is precisely the TS pipeline-driver.ts:1542-1577 distinction: prev for Remove enumeration, curr for Add parent verification."
  - "advanceWithoutDiff site at pipeline-driver.ts:707 does NOT receive setPrevSnapshot wiring — it has no `prev` in scope (single-snapshot advance, no diff iterator), and no descendant SQL runs after it. Acceptance criteria's regex `swapSnapshot(this.#instanceId, curr.db.db.name)` correctly excludes this site."
  - "Wave 0 red-state stub flipped to 3 green-state assertions: test_b11_descendants_from_prev (signature contains prev_db_path: &str AND open_with_flags reads against prev_db_path), test_b11_set_prev_snapshot_napi_present (Task 1a artifact lock-in via 64-char preceding-window check for #[napi] attribute), test_b11_fallback_when_prev_not_set (back-compat via .unwrap_or_else + eprintln pattern)."
  - "B11 differential test schema has no FK CASCADE; explicit ordered DELETE (messages → conversations → channels) inside a single PG transaction drives the same single-tx semantics as a real cascade-delete-enabled schema. Per SKILL.md hard rule 2, all rows are inserted at test start with Date.now()-keyed ids and removed at test end — baseline seed.sql is NOT modified."
  - "Phase 33 review item WR-01 (strict-mode advance unusable) interaction analyzed: NO interaction. WR-01 is a TS-side parity-check limitation (tsAdvance returns empty in mono-rs); B11 is a Rust-side correctness bug (descendant SQL reading wrong snapshot). Independent."
metrics:
  duration_seconds: 2400
  duration_human: ~40 minutes
  completed_date: 2026-04-29
  tasks_completed: 4
  files_created: 1
  files_modified: 4
  total_lines_added: ~435
---

# Phase 34 Plan 06: Track 2 B11 Cascade-Delete Prev-Snapshot Fix Summary

**One-liner:** Land Track 2 fix B11 — route `emit_descendant_removals` SQL reads against the PREV snapshot via additive `set_prev_snapshot` napi method. Mono-rs's TS pipeline-driver materializes the snapshotter diff from `prev` BEFORE swap_snapshot; Rust must read descendants from the same snapshot (where same-tx-deleted rows still exist), not the post-swap `curr` (where they're already gone). CLAUDE.md gate #4 preserved via Option B (additive method).

## Executive summary

B11 (CONTEXT D-15 BLOCKING) was a silent same-tx descendant elision bug: when a parent row was deleted in the same transaction as its descendants, Rust's `emit_descendant_removals` opened a fresh read connection on `db_path` — which by the time advance ran had been pointed at `curr` (post-swap_snapshot). The descendants Rust queried for were already deleted in `curr`, so SELECT returned empty rows and Rust silently emitted no descendant Remove row-changes. TS upstream (pipeline-driver.ts:1542-1577) avoids this by walking the snapshotter diff iterator FROM the `prev` snapshot before swap_snapshot — descendants of a deleted parent are still present in prev.

The fix is purely structural and additive:
1. **Struct field** (Task 1a): `PipelineInstance.prev_db_path: Option<String>`.
2. **Napi method** (Task 1a): `#[napi] pub fn set_prev_snapshot(id, prev_db_path)` — ADDITIVE; no existing buffered method signature changed.
3. **Signature plumbing** (Task 1b): `emit_descendant_removals(prev_db_path: &str, ...)`; `advance_persistent_pipeline` + `advance_persistent_pipeline_with_cancel` accept `(db_path, prev_db_path)`. All 4 internal callers updated; the recursive self-call inside `emit_descendant_removals` also threads `prev_db_path`.
4. **TS wiring** (Task 2): `pipeline-driver.ts` calls `setPrevSnapshot(prev.db.db.name)` immediately BEFORE `swapSnapshot(curr.db.db.name)` on all 3 advance entry points.
5. **Wave 0 stub flip** (Task 1b): red-state `#[ignore]` removed; 3 green-state source-level assertions added (signature uses prev_db_path, napi method present, back-compat fallback path doesn't panic).
6. **Differential test** (Task 3): `tools/ivm-parity/diff-tests-track2.ts` gained a `[5/5] b11-cascade-multi-table-tx` case — inserts 1 channel + 2 conversations + 4 messages, hydrates baseline, deletes all 7 in a single PG transaction, re-hydrates, asserts TS↔RS hash equality. Fully restorable per SKILL.md hard rule 2.

## Exact location of RustPipelineManager struct + prev_db_path field

`packages/zqlite-rs/src/pipeline_manager.rs:58-86`. The `PipelineInstance` struct (line 58) gained the field. `RustPipelineManager` (line 87) is unchanged at the struct level — the new `set_prev_snapshot` napi method (line ~298, just before `swap_snapshot`) acquires the per-instance mutex and writes to the inner field. No top-level RwLock was needed.

## Number of swapSnapshot call sites and setPrevSnapshot pairing

| File line | Site | Has setPrevSnapshot directly preceding |
| --- | --- | --- |
| 707 | `advanceWithoutDiff` (single-snapshot, no `prev` in scope) | NO (intentional — no descendant SQL runs after this path; acceptance regex excludes via `swapSnapshot(this.#instanceId, curr.db.db.name)` filter) |
| 2096 | `#rustAdvanceStreaming` | YES |
| 2242 | `#rustAdvanceAsync` | YES |
| 2334 | `#rustAdvance` (buffered) | YES |

Verification:
```bash
count_swap=$(grep -c "this.#manager.swapSnapshot(this.#instanceId, curr.db.db.name)" packages/zero-cache/src/services/view-syncer/pipeline-driver.ts)
count_with_prev=$(grep -B 1 "this.#manager.swapSnapshot(this.#instanceId, curr.db.db.name)" packages/zero-cache/src/services/view-syncer/pipeline-driver.ts | grep -c "this.#manager.setPrevSnapshot(this.#instanceId, prev.db.db.name)")
test "$count_swap" -eq "$count_with_prev"   # → 3 == 3 → PASS
```

## Task commit hashes (per Task 1a/1b reviewability split)

- **Task 1a:** `3e4ca5182` — feat(34-06): B11 Task 1a — add set_prev_snapshot napi method + entry-point reads
- **Task 1b:** `78e063b2f` — fix(34-06): B11 Task 1b — route emit_descendant_removals against PREV snapshot
- **Task 2:** `d4176b047` — feat(34-06): B11 Task 2 — wire setPrevSnapshot before swapSnapshot in pipeline-driver.ts
- **Task 3:** `12911f8d3` — test(34-06): B11 — add cascade-delete differential test to diff-tests-track2.ts

## WR-01 interaction analysis

**Interaction: NONE (independent).** Phase 33 review item WR-01 ("Strict-mode parity check throws on legitimate empty TS oracle") is a TS-side limitation in `#maybeRunParityCheck` — `tsAdvance` returns empty in mono-rs (documented limitation), so strict mode unconditionally throws on first Rust output. WR-01 lives in pipeline-driver.ts:503-527; B11's wiring lives at lines 2096, 2242, 2334. They share a file but not a code path. The B11 fix neither resolves WR-01 nor regresses it.

## CLAUDE.md gate #4 verification (no signature change to existing buffered methods)

```
git diff $(git rev-parse HEAD~4) HEAD -- packages/zqlite-rs/src/pipeline_manager.rs \
  | grep -E "^-.*pub fn (advance|advance_async|advance_streaming|hydrate|addQuery|addQueries)" \
  | wc -l
→ 0
```

The only NEW `#[napi]` method in this plan is `set_prev_snapshot` (additive). All existing buffered method signatures are byte-identical pre/post Plan 34-06.

## Test results

| Suite | Pre-Plan | Post-Plan | Delta |
| --- | --- | --- | --- |
| `cargo test --release -p zqlite-rs --lib` | 130 passed, 1 ignored (B11 Wave 0 stub) | **134 passed, 2 ignored** | +3 passing tests (Wave 0 flip + 2 new B11 tests); 1 unrelated ignored stub remains |
| `cargo test --release -p zero-ivm-rs --lib` | 195 passed | **195 passed** | no change |
| `vitest pipeline-driver.streaming.test.ts` | 6 passed | **6 passed** | no change |
| `vitest pipeline-driver.test.ts` | 37 passed / 3 failed (pre-existing snapshot mismatches verified by stash-and-rerun) | **37 passed / 3 failed** | no regression |

The 3 pre-existing pipeline-driver.test.ts failures (`whereExists query`, `subset client schema can hydrate whereExists helper tables`, `whereExists added by permissions return no rows`) are snapshot mismatches unrelated to B11 — they were failing before any changes in this plan. Verified by stashing the Plan 34-06 changes and re-running: same 3 tests fail with the same diff. Out-of-scope for B11 (logged here for visibility; Plan 34-07 verification can triage).

## B11 differential test channel (for 34-07 verification spot-check)

The B11 differential test inserts a temp channel `b11-ch-${Date.now()}` plus 2 conversations and 4 messages, then deletes them in a single PG transaction. Date.now()-keyed ids prevent cross-run collision. The test cleans up best-effort in finally{} — if a run leaves orphan rows (e.g., timeout), they'll have `b11-ch-` / `b11-co-` / `b11-m-` prefixes for easy manual cleanup:

```sql
DELETE FROM messages WHERE id LIKE 'b11-m-%';
DELETE FROM conversations WHERE id LIKE 'b11-co-%';
DELETE FROM channels WHERE id LIKE 'b11-ch-%';
```

No baseline `seed.sql` rows added. SKILL.md hard rule 2 preserved.

## Confirmation: no existing #[napi] method was modified

| Method | Pre-Plan signature | Post-Plan signature | Delta |
| --- | --- | --- | --- |
| `advance(id, changes_json)` | `pub fn advance(&self, id: String, changes_json: String) -> napi::Result<Buffer>` | unchanged | NONE |
| `advance_async(id, changes_json)` | `pub fn advance_async(...) -> napi::Result<AsyncTask<AdvanceTask>>` | unchanged | NONE |
| `advance_streaming(id, changes_json)` | `pub fn advance_streaming(...) -> napi::Result<AdvanceStream>` | unchanged | NONE |
| `hydrate*` (3 variants) | unchanged | unchanged | NONE |
| `add_query / remove_query / add_queries / set_table_specs / set_permission_tables / set_query_companions` | unchanged | unchanged | NONE |
| `swap_snapshot(id, new_db_path)` | unchanged | unchanged | NONE |
| `pipeline_count(id)` | unchanged | unchanged | NONE |
| **`set_prev_snapshot(id, prev_db_path)`** | (did not exist) | **NEW additive** | ADDED |

## Deviations from Plan

**None — plan executed as written.** Task 1a/1b split delivered reviewable diffs as designed. The legacy RustPipeline class and bench/test sites required `&db_path, &db_path` to satisfy the new signature; this is a documented decision (see decisions[]) — preserves pre-fix legacy behavior, which mono-rs production never relies on (production uses RustPipelineManager).

## Self-Check: PASSED

- [x] `.planning/phases/34-differential-fuzz-schema-extension/34-06-SUMMARY.md` exists at expected path.
- [x] Commit `3e4ca5182` exists (Task 1a).
- [x] Commit `78e063b2f` exists (Task 1b).
- [x] Commit `d4176b047` exists (Task 2).
- [x] Commit `12911f8d3` exists (Task 3).
- [x] `cargo test --release -p zqlite-rs --lib` → 134 passed, 0 failed.
- [x] `cargo test --release -p zero-ivm-rs --lib` → 195 passed, 0 failed.
- [x] `grep -c "setPrevSnapshot" packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` ≥ 1.
- [x] `grep -B 1 "this.#manager.swapSnapshot(this.#instanceId, curr.db.db.name)" ... | grep -c "this.#manager.setPrevSnapshot"` == 3.
- [x] CLAUDE.md gate #4: 0 removed/modified existing #[napi] method signatures.
