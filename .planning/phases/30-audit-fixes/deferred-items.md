# Phase 30 — Deferred Items

Pre-existing issues discovered during plan execution that are out of scope for the current plan but should be triaged.

## Discovered during 30-01 (LIKE case sensitivity fix)

### 1. `advance::tests::bench_persistent_pipeline_sequential_vs_parallel` mismatch

- **File:** `packages/zqlite-rs/src/advance.rs:1875`
- **Symptom:** Sequential vs parallel pipelines produce different row-change counts (sequential: 32149, parallel: 40000) under 20 pipelines / 1000 rows / 50 changes / 100 iterations.
- **Why deferred:** Unrelated to the LIKE/ILIKE flag fix in `hydrate.rs::parse_predicate_json`. Failure exists at HEAD (220f0fc3b) before any 30-01 changes were applied. Behaviour is in `advance.rs`, not touched by 30-01.
- **Severity:** Test gate failure (correctness divergence between sequential and parallel persistent-pipeline advance). Likely a real bug surfaced by the unified-operator-tree refactor (commit 94f9a8c7a).
- **Suggested owner:** Phase 30 follow-up plan (post-AUDIT-04) or a dedicated correctness plan in milestone v5.0 streaming. Investigate whether parallel path double-counts edits or whether sequential under-counts (40000 = 20 pipelines × 50 changes × 40 something — inspect the operator chains).

### 2. Pre-existing `table_source.rs::test_push_epoch_increments` compilation error

- **File:** `packages/zqlite-rs/src/table_source.rs:923,927,932`
- **Status:** AUTO-FIXED in 30-01 commit 22cba6141 (Rule 3 — blocking issue) by locking the `Mutex<u64>` for read.
- **Note:** This was a compile error preventing all tests in zqlite-rs from running. The field `push_epoch` was changed from `u64` to `Mutex<u64>` at HEAD without updating the test assertion sites.

## Discovered during 30-04 (debug_assert! → assert! promotion)

### 3. `pipeline-driver.exists-parent-edit.test.ts` failures (AUDIT-02 regression)

- **File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver.exists-parent-edit.test.ts:197,227`
- **Symptom:** Two tests added in 30-02 to verify the AUDIT-02 fix are now failing — `expected [] to have a length of 1 but got +0`. The expected REMOVE/ADD changes are not being emitted by the pipeline.
  - `EXISTS parent_field edit emits Remove when membership lost`
  - `EXISTS parent_field edit emits Add when membership gained`
- **Why deferred:** AUDIT-03 (plan 30-04) only modifies Rust source files (`zero-ivm-rs/src/{join,exists,or_exists,take,cap}_op.rs`) — no zero-cache TS files were touched in this plan. The TS test failures cannot be caused by the assert!/debug_assert! promotions. The failure messages contain none of the promoted assertion text (no "Parent edit must not change relationship", no "Unexpected re-entrancy", etc.). This appears to be a pre-existing regression in the merged 30-02 work — possibly the split_edit_keys fix did not propagate correctly through the persistent pipeline path that this test exercises.
- **Severity:** AUDIT-02 (Bug #2) regression. Tests added by 30-02 to verify its own fix are failing on the post-merge base. Should be triaged as part of AUDIT-02 follow-up, not AUDIT-03.
- **Suggested owner:** Re-open 30-02 verification or a dedicated 30-02-FOLLOWUP plan. Investigate whether the fix in `advance.rs::collect_split_edit_keys` is being reached by the test fixtures (TS-side persistent pipeline build), or whether the pipeline driver bypasses the split-edit path under certain configurations.
- **Confirmation that AUDIT-03 is unaffected:** All 7 newly-promoted `assert!` sites pass their `#[should_panic]` regression tests under `cargo test --release`. The pipeline-driver.test.ts main suite (40/40) and fuzz-ivm.test.ts (6/6 with FUZZ_NUM_RUNS=1000) pass — confirming AUDIT-03 introduces no operator-semantics regressions.
- **Resolution (30-05):** Closed by gap plan 30-05 — see
  `.planning/phases/30-audit-fixes/30-05-PLAN.md` and
  `.planning/phases/30-audit-fixes/30-05-SUMMARY.md`. The 3 integration
  tests now pass per the verification gate. Root cause was a stale napi
  build artifact at the parent monorepo path: vitest resolves `zqlite-rs`
  through `node_modules/zqlite-rs -> packages/zqlite-rs`, and the parent
  repo's `.node` binary had not been rebuilt after the 30-02 source fix
  landed — so tests silently exercised pre-fix IVM logic. Fix: added a
  build-freshness gate (`assertNapiBinaryFreshness`) at the napi load
  site in `pipeline-driver.ts` that throws a clear error if the `.node`
  is older than the Rust source, plus a Rust regression unit test
  (`test_audit_02_production_ast_shape_does_not_regress`) that pins the
  full runtime AST shape captured from the diagnostic log. The
  AUDIT-02 source-code fix from 30-02 was already correct end-to-end.

### 4. `advance::tests::bench_persistent_pipeline_sequential_vs_parallel` still failing

- **File:** `packages/zqlite-rs/src/advance.rs:2130`
- **Status:** Same as item 1 — pre-existing, unrelated to AUDIT-03. Re-confirmed during 30-04 verification gate. Counts shifted slightly (32149 vs 40000 → still mismatched) but the underlying issue is unchanged: parallel persistent-pipeline advance does not match sequential output.
