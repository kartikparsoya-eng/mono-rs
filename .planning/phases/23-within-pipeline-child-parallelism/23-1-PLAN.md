# Phase 23 — Within-Pipeline Child Parallelism

## Plan 23-1: ParallelJoinOperator & ParallelExistsOperator

### Goal

Add within-pipeline child parallelism using Rayon `par_iter` for Join and Exists operators. While Phase 22 parallelizes across pipelines (each pipeline on a separate Rayon thread), this phase parallelizes _within_ a single pipeline — fetching children for all parent rows in parallel.

### Approach

Create `ParallelJoinOperator` and `ParallelExistsOperator` that rebuild child operator trees per parent row on Rayon threads. Introduce free functions `fetch_children_for_row_static` and `fetch_child_count_static` to avoid `&self` capture issues with `par_iter` (since `dyn Operator` is not `Sync`).

#### Implementation Steps

1. **Add `Clone` derive to `OperatorConfig`** in `packages/zero-ivm-rs/src/pipeline.rs` so configs can be cloned into `par_iter` closures.
2. **Create `make_child_source()`** — a helper that builds a table-specific `RustTableSource` for child tables. `RustTableSource` is table-specific (it wraps a connection to a particular SQLite DB), so child pipelines need their own source pointing at the same DB.
3. **Add `db_path()` accessor to `RustTableSource`** in `packages/zqlite-rs/src/table_source.rs` so `make_child_source` can read the DB path from the parent source and create a new connection.
4. **Implement free functions** `fetch_children_for_row_static` and `fetch_child_count_static` that accept all needed data as arguments (config, db_path, parent_row) instead of capturing `&self`. This sidesteps the `Sync` requirement that `par_iter` closures impose.
5. **Implement `ParallelJoinOperator`** — calls `par_iter` over parent rows, invokes `fetch_children_for_row_static` per row, collects results.
6. **Implement `ParallelExistsOperator`** — calls `par_iter` over parent rows, invokes `fetch_child_count_static` per row, filters based on exists/not-exists semantics.
7. **Wire into `build_operator_with_live_source`** so that when the operator type is Join or Exists, the parallel variant is used.

### Key Decisions

| Decision                                                       | Rationale                                                                                                 |
| -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| Added `Clone` derive to `OperatorConfig`                       | Required so config can be moved into `par_iter` closures; all fields are already Clone-compatible         |
| Created `make_child_source()`                                  | `RustTableSource` is table-specific; child pipelines need their own source pointing at the same SQLite DB |
| Added `db_path()` accessor to `RustTableSource`                | Needed by `make_child_source` to read the DB path from the parent source                                  |
| Used free functions instead of methods for `par_iter` closures | `dyn Operator` is not `Sync`; free functions avoid capturing `&self` entirely                             |
| `unsafe impl Send` for both parallel operators                 | Justified: closures only capture `Arc<RustTableSource>` (which is `Send+Sync`) and shared slice refs      |

### Files Changed

| File                                     | Change                                                                                        |
| ---------------------------------------- | --------------------------------------------------------------------------------------------- |
| `packages/zqlite-rs/src/hydrate.rs`      | Added `ParallelJoinOperator`, `ParallelExistsOperator`, free functions, `make_child_source()` |
| `packages/zqlite-rs/src/table_source.rs` | Added `db_path()` accessor                                                                    |
| `packages/zero-ivm-rs/src/pipeline.rs`   | Added `#[derive(Clone)]` to `OperatorConfig`                                                  |

### Tests

6 new tests added:

- `test_parallel_join_fetches_children` — basic parent→child join via par_iter
- `test_parallel_join_null_key_no_children` — null foreign key produces no children
- `test_parallel_exists_filters_correctly` — exists keeps only parents with children
- `test_parallel_not_exists` — not-exists keeps only parents without children
- `test_parallel_join_multiple_pipelines` — fan-out: multiple pipelines each using parallel join
- `test_parallel_join_with_filter_on_children` — parallel join combined with child-side filter

**Total: 139 cargo tests pass.**

### Threat Model

<threat_model>

**Unsafe Send impls**: `ParallelJoinOperator` and `ParallelExistsOperator` use `unsafe impl Send`. This is safe because:

1. The `par_iter` closures only capture shared references to `Arc<RustTableSource>`, which is `Send + Sync`.
2. Slice references (`&[Row]`) are `Send + Sync` when `Row: Send + Sync`.
3. Child `RustTableSource` instances are created per-row inside each parallel iteration — there is no shared mutable state between threads.
4. Each Rayon task builds its own operator tree from the cloned `OperatorConfig`, so no `dyn Operator` instances are shared across threads.

The unsafe boundary is minimal and well-contained: it exists solely to satisfy the compiler's conservative analysis of closure captures, not because actual unsafety is present.

</threat_model>
