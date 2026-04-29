# GSD Debug Knowledge Base

Resolved debug sessions. Used by `gsd-debugger` to surface known-pattern hypotheses at the start of new investigations.

---

## phase-30-03-exists-edit-regression — IVM bench test asserts seq==par on shared pipelines, fails due to warm-up ramp bias

- **Date:** 2026-04-29
- **Error patterns:** bench_persistent_pipeline_sequential_vs_parallel, advance.rs, zqlite-rs, IVM, sequential vs parallel, assertion left=32149 right=40000, warm-up, steady state, ramp, pipeline emit count, exists_op, or_exists_op
- **Root cause:** Bench test in packages/zqlite-rs/src/advance.rs ran sequential and parallel phases on the SAME mutable pipelines vec with only one warm-up advance call. IVM operator emit count ramps from ~67 (cold) to 400 (steady state) over ~80 iterations. Sequential measured the entire ramp (total 32149); parallel ran second on already-stabilized pipelines and measured only steady state (total 40000). Assertion seq_total_changes == par_total_changes was structurally impossible by construction. Pre-existing bug, predates Phase 30 entirely (reproduces at commit aa6f0d7e2).
- **Fix:** Restructure bench_persistent_pipeline_sequential_vs_parallel to build fresh, fully-warmed (120-iter warm-up, safe margin above empirical ~80-iter convergence) pipelines for each phase. Both phases now measure 100 iterations from steady state; assertion meaningfully checks emit-count invariance under execution mode. 2.46x parallel speedup remains visible.
- **Files changed:** packages/zqlite-rs/src/advance.rs

---
