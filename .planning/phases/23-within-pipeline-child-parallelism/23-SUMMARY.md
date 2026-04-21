# Phase 23 Summary — Within-Pipeline Child Parallelism

## What Was Done

- Added within-pipeline child parallelism via `ParallelJoinOperator` and `ParallelExistsOperator`
- These operators use Rayon `par_iter` to fetch children for all parent rows in parallel
- Each parallel iteration creates a fresh child operator tree from config (via `make_child_source` + `build_operator_with_live_source`)
- Key fix: `OperatorConfig` now derives `Clone`; `RustTableSource` exposes `db_path()` for child source creation
- Used free functions (`fetch_children_for_row_static`, `fetch_child_count_static`) to avoid `&self` capture issues with `par_iter`

## Test Results

- **cargo test (zqlite-rs):** 139 passed, 0 failed
- **vitest (pipeline-driver.test.ts):** 28 passed, 2 failed (pre-existing failures)

## Files Changed

- `packages/zqlite-rs/src/hydrate.rs` — ParallelJoinOperator, ParallelExistsOperator, free functions, make_child_source
- `packages/zqlite-rs/src/table_source.rs` — db_path() accessor
- `packages/zero-ivm-rs/src/pipeline.rs` — Clone derive on OperatorConfig
