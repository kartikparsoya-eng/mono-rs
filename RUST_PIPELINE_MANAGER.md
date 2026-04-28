# RustPipelineManager — Implementation Status

## Overview

Unified Rust IVM system where Rust handles ALL IVM operations (hydration + advance + companion checks + permission filtering) with no TS fallbacks. `RustPipelineManager` consolidates pipeline management, replacing the old per-query `RustPipeline` approach.

**Key Discovery**: `diff_and_advance()` is NOT viable with the current snapshot model. Both prev and curr snapshots are `BEGIN CONCURRENT` transactions on the same SQLite WAL2 file. Rust opening fresh read-only connections sees the latest committed state for both, making the diff meaningless. The code was built, tested, found broken, and removed.

**Architecture**: TS computes the diff (via `snapshotter.advance()` using frozen connections), passes changes as JSON to `manager.advance()`, Rust handles all operator tree fan-out (IVM), permission table filtering, minRowVersion bump, and companion scalar checks. All 106 tests pass.

## Architecture

```
ViewSyncerService (one per client group)
  └── PipelineDriver (one per client group)
        ├── #manager: RustPipelineManager
        ├── #instanceId: string
        └── Snapshotter (TS — holds frozen BEGIN CONCURRENT connections)

RustPipelineManager (Rust NAPI class)
  └── instances: RwLock<HashMap<String, Arc<Mutex<PipelineInstance>>>>
        └── PipelineInstance
              ├── pipelines: Vec<Mutex<PipelineState>>  (operator trees)
              ├── companions: Vec<CompanionInfo>
              ├── permission_tables: HashSet<String>
              ├── shared_pool: ConnectionPool
              ├── db_path: current snapshot path
              └── table_specs: HashMap<String, TableAndZqlSpec>
```

## NAPI Surface

```rust
#[napi]
impl RustPipelineManager {
    #[napi(constructor)]
    fn new() -> Self

    #[napi]
    fn create_instance(id: String, db_path: String) -> Result<()>

    #[napi]
    fn remove_instance(id: String) -> Result<()>

    #[napi]
    fn set_table_specs(id: String, specs_json: String) -> Result<()>

    #[napi]
    fn add_query(id: String, query_id: String, ast_json: String) -> Result<()>

    #[napi]
    fn remove_query(id: String, query_id: String) -> Result<()>

    #[napi]
    fn hydrate(id: String) -> Result<Buffer>        // par_iter across pipelines

    #[napi]
    fn hydrate_query(id: String, query_id: String) -> Result<Buffer>

    #[napi]
    fn advance(id: String, changes_json: String) -> Result<Buffer>  // TS-computed diff

    // Async versions — run on libuv worker threads for cross-client-group parallelism
    #[napi(ts_return_type = "Promise<Buffer>")]
    fn advance_async(id: String, changes_json: String) -> AsyncTask<AdvanceTask>

    #[napi(ts_return_type = "Promise<Buffer>")]
    fn hydrate_async(id: String) -> AsyncTask<HydrateTask>

    #[napi(ts_return_type = "Promise<Buffer>")]
    fn hydrate_query_async(id: String, query_id: String) -> AsyncTask<HydrateQueryTask>

    #[napi]
    fn swap_snapshot(id: String, new_db_path: String) -> Result<()>

    #[napi]
    fn pipeline_count(id: String) -> Result<u32>

    #[napi]
    fn set_permission_tables(id: String, tables_json: String) -> Result<()>

    #[napi]
    fn set_query_companions(id: String, query_id: String, companions_json: String) -> Result<()>
}
```

## Data Flow

### Hydration (pull-based)

1. TS calls `manager.addQuery(instanceId, queryId, astJson)`
2. Rust builds `PipelineState` with both `fetch_chain` (chained operators for pull) and `op_list` (flat list for push)
3. TS calls `manager.hydrateQuery(instanceId, queryId)` → Rust walks `fetch_chain`, returns encoded rows

### Advance (push-based)

1. TS `Snapshotter.advance()` computes diff using frozen `BEGIN CONCURRENT` connections
2. TS collects changes, serializes to JSON
3. TS calls `manager.advance(instanceId, changesJson)` → Rust:
   a. Pushes changes through `op_list` for each pipeline
   b. Filters out rows from permission tables
   c. Bumps `_0_version` to minRowVersion for older rows
   d. Checks companion scalars — queries SQLite for new value, returns reset signal if changed
4. TS calls `manager.swapSnapshot(instanceId, newDbPath)` → Rust updates connection pool
5. TS decodes binary result; if `reset_signal` present, throws `ResetPipelinesSignal`

### Companion Scalar Checks (in Rust `advance()`)

1. TS calls `manager.setQueryCompanions(id, queryId, companionsJson)` after `addQuery()`
2. `CompanionInfo` stores: `query_id`, `table`, `child_field`, `resolved_value`, `where_conditions`, `primary_key`
3. During `advance()`, if a change touches a companion's table, Rust queries SQLite via pool for the current scalar value
4. If the value differs from `resolved_value`, Rust sets `reset_signal` in the result
5. TS decodes flag bit 2 (`0x04`) from the binary buffer → throws `ResetPipelinesSignal`

### Permission Table Filtering (in Rust `advance()`)

1. TS calls `manager.setPermissionTables(id, tablesJson)` after `addQuery()`
2. During `advance()`, any row change whose table is in the `permission_tables` set is dropped
3. This prevents internal permission-check tables from leaking to clients

## Binary Protocol

### `advance()` result encoding

```
[u32] change_count
[u8]  flags
  bit 0 (0x01): has_error
  bit 1 (0x02): has_timings
  bit 2 (0x04): has_reset_signal

If has_error:
  [u16 + bytes] error message
  [u16 + bytes] error type

Per RowChange:
  [u8]  change_type (0=add, 1=remove, 2=edit, 3=other)
  [u16 + bytes] query_id
  [u16 + bytes] table
  [json_value]  row_key
  [u8]  has_row
  If has_row:
    [u16] col_count
    Per column:
      [u16 + bytes] col_name
      [json_value]  col_value

If has_timings: (after all changes)
  [u64] totalUs
  [u32] pipeline_count
  Per pipeline: [u16+bytes query_id, 6x u64 timing fields]

If has_reset_signal: (after timings)
  [u16 + bytes] reason string
```

## Key Files

### Rust

| File                                         | Lines | Description                                                                                        |
| -------------------------------------------- | ----- | -------------------------------------------------------------------------------------------------- |
| `packages/zqlite-rs/src/pipeline_manager.rs` | ~700  | `RustPipelineManager` NAPI class with companion/permission/minRowVersion logic, async Task structs |
| `packages/zqlite-rs/src/advance.rs`          | ~1857 | Old `RustPipeline` (kept for bench), `PipelineState`, `advance_persistent_pipeline()`, encoding    |
| `packages/zqlite-rs/src/diff.rs`             | ~600  | Diff engine (exists but unused — snapshot isolation prevents use)                                  |
| `packages/zqlite-rs/src/hydrate.rs`          | —     | Operator builders                                                                                  |
| `packages/zqlite-rs/src/connection_pool.rs`  | —     | `ConnectionPool`                                                                                   |
| `packages/zero-ivm-rs/src/or_exists_op.rs`   | —     | `OrExistsOperator`                                                                                 |
| `packages/zero-ivm-rs/src/exists_op.rs`      | —     | `ExistsOperator` with `push_child()`                                                               |
| `packages/zero-ivm-rs/src/join_op.rs`        | —     | `JoinOperator`                                                                                     |
| `packages/zero-ivm-rs/src/take_op.rs`        | —     | `TakeOperator` with per-parent LIMIT fix                                                           |

### TypeScript

| File                                                                 | Description                                  |
| -------------------------------------------------------------------- | -------------------------------------------- |
| `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts`    | Main driver, uses `#manager` + `#instanceId` |
| `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` | Binary decoder with reset_signal support     |

### Tests (106 total, all passing)

| File                                   | Count |
| -------------------------------------- | ----- |
| `pipeline-driver.test.ts`              | 40    |
| `pipeline-driver.edge-cases.test.ts`   | 46    |
| `pipeline-driver.runaway-push.test.ts` | 3     |
| `pipeline-driver.not-exists.test.ts`   | 8     |
| `pipeline-driver.unicode.test.ts`      | 5     |
| `decode-advance-buf.test.ts`           | 4     |

## Bugs Fixed (8)

1. Hydration returning 0 rows — added `fetch_chain` (PassthroughOperator issue)
2. Child change routing bypassing operators — Exists via `push_child()`, Join via `child_table_map`
3. Companion `ResetPipelinesSignal` — `queryCompanionScalar()` direct SQLite query
4. Remove query not cleaning up Rust side
5. Companion row changes not emitted
6. Per-parent LIMIT bug — TakeOperator state key from constraint
7. OrExists not supported — new `OrExistsOperator`
8. Exists `or_condition` silently dropped

## Completed Phases

### Phase 1: Rust IVM Unification (COMPLETE)

- Made Rust handle all hydration and advance via persistent operator trees
- Fixed 8 bugs, all TS IVM fallbacks removed
- `RustPipelineManager` NAPI class created

### Phase 1.5: Dead Code Cleanup (COMPLETE)

- Removed `ZERO_DISABLE_RUST_IVM` env var check, `USE_RUST_IVM`, `USE_RUST_JOIN`, `USE_RUST_EXISTS` flags
- Replaced conditional `new RustStorage()` with always-on
- Removed `#streamer` field (always null), `#rustJoinAvailable` field
- Removed dead `decodeDiffAdvanceResult()`, `decode-dispatch-buf.ts`, `advance-fanout-bench.ts`
- ~2048 lines of dead stateless code removed from `advance.rs` (3905 → 1857 lines)
- ~260 lines of TS IVM fallback code removed from `pipeline-driver.ts`

### Phase 2: Companions + Permissions in Rust (COMPLETE)

- `CompanionInfo` struct and `setQueryCompanions()` NAPI method
- `permission_tables` and `setPermissionTables()` NAPI method
- `advance()` now handles: permission filtering, minRowVersion bump, companion scalar checks
- Binary protocol extended with flag bit 2 for `reset_signal`
- TS `#rustAdvance()` rewritten to handle reset signal
- TS `#convertDispatchChanges()` simplified — no longer does permission/minRowVersion
- Dead TS functions removed: `queryCompanionScalar`
- Tests added for permission filtering during advance and reset_signal binary encoding

## TableAndZqlSpec Serialization Note

When TS calls `set_table_specs()`, `zqlSpec` must be wrapped as `{columns: zqlSpec}` because TS `zqlSpec` is a flat `Record<string, SchemaValue>` but Rust's `ZqlSpec` struct expects `{columns: {...}}`.

### Phase 3: Cross-client-group parallelism (COMPLETE)

**Problem**: Each `ViewSyncerService` calls `PipelineDriver.advance()` synchronously on the JS event loop. While one client group's IVM fan-out runs, all others are blocked. With many concurrent client groups, this serializes all advance work on the main thread.

**Solution**: Make hydration and advance non-blocking via `napi::Task`. The heavy IVM work (operator tree fan-out, permission filtering, companion checks) moves to libuv worker threads, freeing the JS event loop for other client groups to start their work concurrently.

**Architecture**:

```
Before (sequential):
  JS thread: [VS-A advance ████████] [VS-B advance ████████] [VS-C advance ████████]

After (parallel):
  JS thread: [VS-A start] [VS-B start] [VS-C start] ... [VS-A resolve] [VS-B resolve] [VS-C resolve]
  Worker 1:  [VS-A IVM ████████]
  Worker 2:  [VS-B IVM ████████]
  Worker 3:  [VS-C IVM ████████]
```

**Rust implementation**:

- `instances` field: `RwLock<HashMap<String, Arc<Mutex<PipelineInstance>>>>`
- Three `napi::Task` structs: `AdvanceTask`, `HydrateTask`, `HydrateQueryTask`
- Each task clones the `Arc<Mutex<PipelineInstance>>`, locks in `compute()` (worker thread), encodes result
- `resolve()` converts `Vec<u8>` to `Buffer` on the JS main thread
- Extracted standalone helpers: `advance_instance()`, `hydrate_instance()`, `hydrate_query_instance()`
- Sync methods preserved for backward compat (tests use sync path)

**TS integration**:

- `pipeline-driver.ts`: `advanceAsync()` + `#rustAdvanceAsync()` — async advance with reset signal handling
- `pipeline-driver.ts`: `addQueriesAsync()` — async batch hydration using `hydrateAsync()`
- `view-syncer.ts`: `#advancePipelines()` calls `await pipelines.advanceAsync()` (production path)
- `view-syncer.ts`: `#syncQueryPipelineSet()` converted from generator to async function using `addQueriesAsync()`

**Key design decisions**:

- Advance within a single pipeline uses sequential `iter` (not `par_iter`) — benchmarks proved sequential is faster within one client group
- Cross-client-group parallelism comes from different `ViewSyncerService` instances hitting different `PipelineInstance`s on separate worker threads
- Same-instance calls serialize on the instance `Mutex`

## Remaining Work

### Low Priority

1. Remove old `RustPipeline` class from `advance.rs` — blocked by `pipeline-driver.bench.ts` still using it
2. Run benchmarks to measure actual cross-client-group parallelism gains

### Future Phases (blocked)

- **Phase 3b**: Shared diff computation — cache diffs by (prevVersion, currVersion) so multiple client groups reuse the same diff
- **Phase 4**: Rust-side diff — blocked by snapshot isolation problem (both prev/curr are `BEGIN CONCURRENT` on same WAL2 file; Rust can't open connections with correct isolation)
