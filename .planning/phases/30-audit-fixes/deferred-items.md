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
