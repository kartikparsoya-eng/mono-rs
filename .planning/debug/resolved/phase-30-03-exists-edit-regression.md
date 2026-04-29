---
status: resolved
trigger: 'Pre-existing test in `packages/zero-ivm-rs` regressed after edits to `src/exists_op.rs` and/or `src/or_exists_op.rs` for Phase 30 Plan 03 (AUDIT-04).'
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
---

## Current Focus

reasoning_checkpoint:
hypothesis: "The bench test `bench_persistent_pipeline_sequential_vs_parallel` in zqlite-rs/src/advance.rs has an assertion (seq_total_changes == par_total_changes) that cannot hold by construction: the operator state evolves toward a steady-state emit rate over many iterations. Sequential measures from a barely-warmed state (1 warm-up call) and ramps up; parallel runs AFTER sequential on the SAME pipelines and starts already in steady state. The two totals are inherently different by the warm-up tail."
confirming_evidence: - "Diagnostic logging of per-iteration emit counts: sequential first 5 iters [67,69,72,78,84], last 5 iters [400,400,400,400,400]; parallel first 5 [400,400,400,400,400], last 5 [400,400,400,400,400]. Parallel never sees the ramp." - "Numbers (32149 vs 40000) are deterministic across runs — not a flake, a structural difference." - "Test fails identically with the same exact numbers at commit aa6f0d7e2 — the commit immediately BEFORE plan 30-02 work began. Confirms regression predates 30-02/30-03/30-04 entirely." - "Working tree is clean (git status). No edits to exists_op.rs or or_exists_op.rs. Plan 30-03 has not been started." - "All 161 zero-ivm-rs tests pass — the package the user mentioned has no regression." - "Steady-state per-iteration emit count: 100 iters × 20 pipelines × 20 changes/iter avg = 40000, matches parallel total exactly."
falsification_test: "If I rebuild fresh, identically-warmed pipelines for sequential AND parallel and the totals still differ, my hypothesis is wrong. After fix, both produce 40000 deterministically."
fix_rationale: "Make sequential and parallel measure equivalent work by giving each fresh pipelines warmed to steady state (120-iter warm-up, since per-iteration counts converge by ~80 iters). The assertion now meaningfully checks 'the SAME workload produces the SAME emitted-row count regardless of execution mode' which is a real correctness property. The 2.46x parallel speedup remains visible in benchmark output."
blind_spots: - "Did not check the failing zero-cache vitest gates (pipeline-driver.\*.test.ts and fuzz-ivm.test.ts) called out in plan 30-03 Task 3 — but those gates are Plan 30-03 deliverables, not affected by this bench-test fix." - "User reported the regression as in zero-ivm-rs caused by 30-03 work. Both premises are false. Possible the user was confused by stale cargo cache or by referencing the wrong package name; my evidence is clear that neither claim holds." - "The 120-iter warm-up roughly triples test runtime (~24s vs ~9s) — acceptable for a bench test but worth noting."

next_action: "Wait for human verification that the fix is acceptable (alternative: mark the bench test #[ignore] since it's a benchmark, not a correctness gate)."

## Symptoms

expected: "All previously-passing tests in packages/zero-ivm-rs continue to pass after AUDIT-04 fix is applied."
actual: "At least one pre-existing test regressed after edits to exists_op.rs / or_exists_op.rs."
errors: "Not provided — must capture by re-running cargo test."
reproduction: "cd packages/zero-ivm-rs && cargo test --release"
started: "After WIP edits for plan 30-03 (AUDIT-04)."

## Eliminated

- hypothesis: "Plan 30-03 (AUDIT-04) source edits to exists_op.rs / or_exists_op.rs caused the regression."
  evidence: "git status confirms working tree clean. Both files match their pre-30-02 state exactly. No 4-transition Edit branch logic added."
  timestamp: 2026-04-29

- hypothesis: "Plan 30-02 split_edit_keys advance work introduced a sequential/parallel divergence."
  evidence: "Bench test fails with the EXACT SAME numbers (seq=32149, par=40000) at commit aa6f0d7e2 — the commit immediately before plan 30-02 work. Predates 30-02 entirely."
  timestamp: 2026-04-29

- hypothesis: "Failure is in packages/zero-ivm-rs."
  evidence: "cargo test --release in zero-ivm-rs passes all 161 tests in both debug and release modes. Failure is in packages/zqlite-rs."
  timestamp: 2026-04-29

- hypothesis: "It's a flaky/race-condition test."
  evidence: "Same exact numbers (32149, 40000) reproduce deterministically across multiple runs. Not a race."
  timestamp: 2026-04-29

## Evidence

- timestamp: 2026-04-29
  checked: ".planning/phases/30-audit-fixes/30-03-PLAN.md"
  found: "Spec rewrites Edit branch in both ExistsOperator::push_impl and OrExistsOperator::push_impl per 4-transition truth table. Plan depends on Plan 02 (committed)."
  implication: "Need to compare current code against the spec to find divergence."

- timestamp: 2026-04-29
  checked: "packages/zero-ivm-rs/src/exists_op.rs lines 252-268 (Edit branch)"
  found: "Original buggy single-row Edit logic still in place. No 4-transition match. Working tree clean per git status."
  implication: "Plan 30-03 source edits have NOT been applied. The reported regression cannot have been caused by them."

- timestamp: 2026-04-29
  checked: "packages/zero-ivm-rs/src/or_exists_op.rs lines 309-318"
  found: "Add/Remove/Edit still collapsed in one arm (original code). No row_passes(&old_row) added."
  implication: "Same conclusion: 30-03 not applied."

- timestamp: 2026-04-29
  checked: "cd packages/zero-ivm-rs && cargo test --release"
  found: "test result: ok. 161 passed; 0 failed"
  implication: "zero-ivm-rs (the package the user mentioned) has zero failures. The reported regression is somewhere else."

- timestamp: 2026-04-29
  checked: "cd packages/zqlite-rs && cargo test --release"
  found: "test result: FAILED. 121 passed; 1 failed: advance::tests::bench_persistent_pipeline_sequential_vs_parallel — left=32149 right=40000"
  implication: "This is the actual failing test. It is in zqlite-rs (not zero-ivm-rs), and it's a bench-style test with a structural assertion."

- timestamp: 2026-04-29
  checked: "git worktree at commit aa6f0d7e2 (immediately pre-30-02), running same test"
  found: "Same exact failure: left=32149 right=40000."
  implication: "Regression predates plan 30-02. Not caused by any 30-XX work."

- timestamp: 2026-04-29
  checked: "Per-iteration diagnostic logging added to bench test"
  found: "Sequential per-iteration emit counts: [67, 69, 72, 78, 84, ..., 400, 400, 400, 400, 400] — ramps up over ~80 iterations. Parallel per-iteration: [400, 400, 400, 400, 400, ...] — already at steady state from iter 0."
  implication: "Confirmed the assertion fails because the two phases measure different operator states. Sequential measures from cold-warmed state through ramp-up. Parallel measures only steady state because sequential already warmed it. The assertion is structurally broken."

- timestamp: 2026-04-29
  checked: "bench_persistent_pipeline_sequential_vs_parallel after fix (rebuild warmed pipelines for each phase, 120-iter warm-up)"
  found: "test result: ok. Sequential 6.15s (40000 row changes), Parallel 2.50s (40000 row changes), Speedup 2.46x"
  implication: "Fix verified. Both phases now measure the same workload from steady state. Assertion is meaningful and passes."

- timestamp: 2026-04-29
  checked: "cd packages/zqlite-rs && cargo test --release (full suite after fix)"
  found: "test result: ok. 122 passed; 0 failed"
  implication: "All zqlite-rs tests pass. No new regressions."

- timestamp: 2026-04-29
  checked: "cd packages/zero-ivm-rs && cargo test --release (re-verify after fix)"
  found: "test result: ok. 161 passed; 0 failed"
  implication: "zero-ivm-rs unaffected — fix isolated to zqlite-rs/src/advance.rs."

## Resolution

root_cause: |
bench_persistent_pipeline_sequential_vs_parallel in packages/zqlite-rs/src/advance.rs
asserts seq_total_changes == par_total_changes after running both phases on the
SAME mutable `pipelines` vec with only a single warm-up advance call. The
per-iteration emit count of an IVM pipeline ramps from ~67 (cold) to 400
(steady state) over the first ~80 iterations. Sequential runs first and includes
the entire ramp in its measurement. Parallel runs second on pipelines that
sequential just stabilized, so it measures only steady-state emissions. The
assertion is structurally impossible to satisfy because the two phases observe
different operator-state regimes by construction. The bug is pre-existing and
unrelated to Phase 30 plans 02/03/04.
fix: |
Restructure the bench test in packages/zqlite-rs/src/advance.rs (function
bench_persistent_pipeline_sequential_vs_parallel) to build a fresh, fully-
warmed (120-iter warm-up) set of pipelines for the sequential phase, drop them
after measurement, and build another identically-warmed set for the parallel
phase. Both phases now measure 100 iterations of advance calls starting from
steady state, so the assertion seq == par is meaningful (it now checks that
the per-iteration emit count is invariant under execution mode, which is a
real correctness property of the IVM pipeline). The 120-iter warm-up was
chosen as a safe margin above the empirical ~80-iter convergence point.
verification: |

- cargo test --release advance::tests::bench_persistent_pipeline_sequential_vs_parallel
  → PASS, sequential=40000, parallel=40000, speedup=2.46x
- cd packages/zqlite-rs && cargo test --release
  → 122 passed, 0 failed (was 121 + 1 fail before)
- cd packages/zero-ivm-rs && cargo test --release
  → 161 passed, 0 failed (unchanged — fix did not touch this crate)
  files_changed:
- packages/zqlite-rs/src/advance.rs (bench_persistent_pipeline_sequential_vs_parallel)
