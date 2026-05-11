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

## ivm-parity-divergences — compound OR-of-EXISTS hydrate parity 0/6 → 5/6 (B5 family)

- **Date:** 2026-05-11
- **Error patterns:** ivm-parity, regression-runner, A-nested-OR-with-EXISTS, D-simple-OR-with-EXISTS, FanOut, ParallelFanOutOperator, ParallelExistsOperator, TakeOperator, partition_key, batch_fetch_children, apply_child_operators, hydrate, EXISTS, OR-of-EXISTS, compound OR, AND-inside-OR, gate condition dropped, collect_exists_branches, build_branch_subpipeline_for_hydrate, rel_to_table, collect_child_tables, collect_rel_to_table_from_configs, uniquify_top_level_csq_aliases, alias-leak, zsubq, relationship_name, flatten_nodes_to_row_changes, coerce_row, column_types, RS returns zero rows, TS oracle, hydrate parity, zqlite-rs, zero-ivm-rs
- **Root cause:** Two distinct production bugs masked behind the same 0/6 hydrate divergence symptom — both surfaced only once compound OR-of-EXISTS branches were routed through the FanOut topology (commit 0f2361b52, B5 fix). Bug A in hydrate.rs::ParallelFanOutOperator::fetch: branch sub-pipelines built via build*operator_with_live_source → build_next_operator → ParallelExistsOperator drove EXISTS-LIMIT Take through batch_fetch_children → apply_child_operators with a default (no-constraint, no-max-bound) FetchRequest. TakeOperator::fetch on a partitioned Take with no constraint AND no max_bound returns vec![] by design (advance-mode invariant) → all EXISTS child rows silently dropped → every parent's EXISTS check saw zero children. Bug B in advance.rs::build_pipeline_state: rel_to_table was seeded from collect_child_tables(&query.ast) — the un-uniquified AST — but Phase-34 alias-leak fix (commit dc16c2588) renames every CSQ relationship to `<alias>*<count>` at config-emission time. Runtime change-output emits the uniquified name; flatten_nodes_to_row_changes lookup missed → fallback used the uniquified name as a table name → coerce_row returned None → every CSQ-EXISTS child row dropped from emitted RowChanges.
- **Fix:** Bug A — packages/zqlite-rs/src/hydrate.rs: new build_branch_subpipeline_for_hydrate(source, configs) constructs each FanOut branch via build_push_next_operator + SourceBridgeOperator (the production hydrate path), routing EXISTS through sequential ExistsOperator which issues per-parent constrained fetches and hits TakeOperator's req.constraint.is_some() branch. Bug B — packages/zqlite-rs/src/advance.rs: new collect_rel_to_table_from_configs(configs) walks the emitted OperatorConfig tree (Join, Exists, OrExists branches, FanOut branches) and keys rel_to_table by the **uniquified** relationship_name; build_pipeline_state seeds from this helper and merges AST-derived entries for `related[]` Joins.
- **Files changed:** packages/zqlite-rs/src/hydrate.rs, packages/zqlite-rs/src/advance.rs, tools/ivm-parity/regression-runner-last.json (5/6 snapshot)

---
