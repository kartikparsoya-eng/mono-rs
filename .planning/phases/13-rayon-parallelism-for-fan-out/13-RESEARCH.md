# Phase 13: Rayon Parallelism — Research

**Completed:** 2026-04-20
**Researcher:** Claude

## Validation Architecture

### Test Strategy

- **Primary gate:** `pipeline-driver.test.ts` must pass unchanged (30 tests, 1 pre-existing failure)
- **Performance benchmark:** Create a multi-pipeline workload (10+ concurrent pipelines consuming same table changes) and measure advance loop latency. Target: 2-4x speedup over serial TS path.
- **Correctness:** Row changes returned must be identical in content and semantics to the TS path (order may differ between pipelines but intra-pipeline order must be preserved).
- **Fallback test:** Verify that unsupported pipeline configurations (e.g., scalar subqueries) fall back to TS path gracefully.

---

## 1. Snapshotter Diff Logic

### How `advance()` Works

The `Snapshotter` uses a **leapfrog** pattern with two SQLite connections alternating snapshots. On `advance()`:

1. The previous snapshot's connection is rolled back and reused for the next snapshot (`resetToHead()`)
2. A new `Snapshot` is created via `BEGIN CONCURRENT` + reading `replicationState` to acquire a read lock
3. A `Diff` object is returned wrapping `prev` and `curr` snapshots

### SnapshotDiff / Diff Iterator

The `Diff` class implements `Iterable<Change>` via `[Symbol.iterator]()`:

1. Queries `_zero.changeLog2` table: `SELECT stateVersion, table, rowKey, op FROM "_zero.changeLog2" WHERE stateVersion > ? ORDER BY stateVersion, pos LIMIT ? OFFSET ?` -- batched in groups of 500
2. For each changelog entry:
   - `RESET_OP` / `TRUNCATE_OP` -> throws `ResetPipelinesSignal` (rehydration needed)
   - `SET_OP` -> fetches `nextValue` from `curr` snapshot via `getRow()`, fetches `prevValues` from `prev` snapshot via `getRows()` (checks unique key conflicts)
   - Delete -> fetches `prevValue` from `prev` snapshot
   - Filters no-op changes (delete of non-existent row)
   - Converts values via `fromSQLiteTypes()` to ZQL row format
3. Returns `Change { table, prevValues, nextValue, rowKey }`

### Version Tracking

- `stateVersion` string from `_zero.replicationState` table
- `numChangesSince()`: `COUNT(*)` on `_zero.changeLog2` where `stateVersion > prevVersion`
- The `changesSince()` method uses `changesSinceBuf()` -- a dedicated Rust function in `database.rs` that returns binary buffer format

### What Rust Must Replicate

- The changelog query + batching logic (already partially exists as `changes_since_buf()` in `database.rs`)
- Row fetching from prev/curr snapshots (`getRow`, `getRows`)
- `fromSQLiteTypes` conversion (boolean, json, bigint bounds checking)
- RESET/TRUNCATE detection (throw equivalent error to signal rehydration)
- Unique key conflict detection (`getRows` with multiple unique keys)

**Key insight:** The diff reading is inherently serial (single DB connection per snapshot). Parallelism happens _after_ reading the diff -- during fan-out to pipelines.

---

## 2. Pipeline-Driver Advance Loop

### `#advance()` Generator (lines 660-754)

The generator iterates over `SnapshotDiff` and for each `Change`:

1. Looks up `TableSource` from `#tables` map by table name (skip if no pipelines use this table)
2. Gets primary key for the table
3. Determines change type:
   - Multiple `prevValues` with a matching `nextValue` (same PK) -> **edit** (the matching one), remaining -> **remove** (conflict resolution)
   - `nextValue` only -> **add**
   - `prevValues` only -> **remove**
4. Calls `#push(tableSource, sourceChange)` which:
   - Starts accumulating via `Streamer`
   - Calls `tableSource.genPush(change)` -- this fans out to ALL connections on that source
   - Collects `RowChange` results from the `Streamer`
5. After all changes processed, calls `table.setDB(curr.db.db)` on all TableSources
6. Tracks timing and yield points for time-slicing

### Data Flow

```
SnapshotDiff -> Change{table, prevValues, nextValue}
  -> TableSource.genPush(SourceChange)
    -> genPushAndWriteWithSplitEdit(connections, change, ...)
      -> For EACH connection: output.push(change)
        -> Operator chain: Filter -> Join -> Take -> Exists -> ...
          -> Pipeline output.push(change)
            -> Streamer.accumulate(queryID, schema, changes)
              -> RowChange[]
```

### Pipeline Topology

- `#tables: Map<string, TableSource>` -- one TableSource per referenced table
- `#pipelines: Map<string, PipelineInfo>` -- one pipeline per query
- Each pipeline's `input` is built by `buildPipeline()` which creates operator chains
- A TableSource has multiple `connections` -- one per pipeline that reads from that table
- **Fan-out point:** `genPushAndWriteWithSplitEdit` iterates over `connections[]` -- this is where Rayon parallelism applies

### Return Type

The `advance()` method returns `{ version: string, numChanges: number, changes: Iterable<RowChange | 'yield'> }` where `RowChange = { type, queryID, table, rowKey, row }`.

---

## 3. Table-Source Push

### `genPush()` (lines 490-506)

The push delegates to `genPushAndWriteWithSplitEdit()` from `memory-source.ts`:

1. Sets overlay on the source (for concurrent read consistency)
2. Iterates over all `connections` on this source
3. For each connection, pushes the change through `connection.output.push(change)`
4. Writes the change to the backing SQLite table (`#writeChange`)
5. Increments push epoch

### SourceChange Format

`SourceChange` is a tuple array: `[type, row, oldRow?]` where:

- `type`: ADD(0), REMOVE(1), EDIT(2)
- `row`: the current row data
- `oldRow`: for EDIT, the previous row data

### Connection / Overlay Model

- Each `Connection` has: `input`, `output`, `sort`, `filters`, `splitEditKeys`, `compareRows`, `lastPushedEpoch`
- The overlay is set per-source before iterating connections -- **critical for Rayon**: overlay is set once, then all connections process it. This is safe for parallel execution since overlay is immutable during iteration.
- `#writeChange` modifies the backing SQLite table -- this MUST happen on the main thread after all connections have processed

### Key Insight for Parallelism

The connection iteration in `genPushAndWriteWithSplitEdit` is the fan-out point. Each connection's operator chain is independent (no shared mutable state between connections). The overlay is read-only during iteration. The write to SQLite happens after all connections complete.

---

## 4. Existing Rust Operators

### Crate Structure (`zero-ivm-rs`)

- `filter.rs` -- `RustFilterPredicate` napi class with `filter_push_batch()` method. Evaluates predicate AST against rows. Returns indices of passing changes + edit split info.
- `join.rs` -- `rust_build_join_constraint()`, `rust_is_join_match()`, `rust_join_push_child_batch()`. All use JSON string serialization at boundary.
- `take_state.rs` -- `RustTakeState` napi class with state storage (HashMap), max bound tracking, row comparison.
- `exists.rs` -- `rust_exists_push_batch()`. Takes JSON array of change descriptors, returns action descriptors.
- `storage.rs` -- `RustStorage` for Take operator state.

### Serialization Pattern

All operators use **JSON string serialization** at the napi boundary:

- Input: JSON strings for rows, predicates, keys
- Output: JSON strings for results
- Internal: `HashMap<String, Value>` where `Value` is the custom enum (Null/Bool/Number/String)

### What a Unified Rust Advance Needs

For a full Rust advance loop, each pipeline's operator chain must be described as a data structure Rust can interpret:

1. **Source mapping:** Which tables feed which pipelines
2. **Operator chain:** Sequence of operators (Filter predicate, Join keys, Take bounds, Exists relationship)
3. **Connection config:** Sort order, filters, split edit keys per connection

The existing operators are implemented as **individual napi calls** -- they'd need to be composed into a single pipeline executor in Rust that chains them together without crossing the FFI boundary per operator.

---

## 5. Existing Rust SQLite

### Database (`zqlite-rs/src/database.rs`)

- Uses `rusqlite::Connection` wrapped in `Arc<RefCell<Connection>>`
- `RefCell` provides interior mutability (single-threaded borrow checking)
- Already has `changes_since_buf()` -- hardcoded changelog query returning binary buffer
- `get_row()`, `get_rows_buf()`, `query_all()` -- various query methods
- `prepare_cached()` used throughout for statement caching

### Statement (`zqlite-rs/src/statement.rs`)

- Wraps `Arc<RefCell<Connection>>` + SQL string
- `all()`, `get()`, `run()`, `all_buf()`, `iterate()` methods
- Uses `prepare_cached()` internally for each execution

### Thread Safety Analysis

- `rusqlite::Connection` is `Send` but NOT `Sync` -- it can be moved between threads but NOT shared
- `Arc<RefCell<Connection>>` is `!Send` and `!Sync` -- cannot be shared across threads at all
- Current design is single-threaded: `RefCell` will panic if borrowed from multiple threads
- **For Rayon:** Cannot share a connection. Must either:
  1. Open separate connections per Rayon thread (expensive, defeats purpose)
  2. Read all needed data on main thread first, share immutable data via `Arc<Vec<Change>>` with Rayon workers
  3. Use `rusqlite::Connection` with `Mutex` instead of `RefCell` (allows `Send` + `Sync` but serializes access)

---

## 6. Rayon + SQLite Thread Safety

### rusqlite Connection Threading

- `rusqlite::Connection` implements `Send` but NOT `Sync`
- This means: one thread can own the connection, but multiple threads cannot reference it simultaneously
- With `SQLITE_OPEN_NO_MUTEX` (already used), SQLite itself is in multi-threaded mode but expects serialized access per connection
- **Conclusion:** Cannot pass a connection to Rayon threads for concurrent querying

### Recommended Pattern

**Read diff on main thread, fan out immutable data via Rayon:**

1. Main thread reads all changelog entries from `_zero.changeLog2`
2. Main thread fetches prev/next row values from snapshots
3. Construct `Vec<Change>` -- fully materialized, immutable
4. Wrap in `Arc` and share with Rayon workers
5. Each Rayon worker processes changes through its pipeline's operator chain
6. Workers return `Vec<RowChange>` results
7. Main thread collects and merges results

This pattern matches D-65/D-66 decisions: single process reads diff once, no per-worker I/O duplication.

### Rayon Configuration

- Default thread pool: `num_cpus` threads
- Custom pool: `rayon::ThreadPoolBuilder::new().num_threads(N).build()`
- For this use case, 4-8 threads likely optimal (diminishing returns with more)
- `par_iter()` on connection set distributes work automatically
- Zero overhead if only 1 pipeline (degrades to serial)

### Data Sharing Pattern

```rust
let changes: Arc<Vec<Change>> = Arc::new(materialized_changes);
let results: Vec<Vec<RowChange>> = pipelines
    .par_iter()
    .map(|pipeline| {
        let changes = Arc::clone(&changes);
        process_pipeline(pipeline, &changes)
    })
    .collect();
```

---

## 7. Pipeline Topology Serialization

### What TS Must Describe to Rust

For each pipeline (query), Rust needs:

1. **Query ID** -- string identifier
2. **Source tables** -- which tables this pipeline reads from
3. **Operator chain** -- ordered list of operators:
   - **Filter:** Predicate AST (already has JSON format in `filter.rs`)
   - **Join:** Parent key, child key, child table, relationship name
   - **Take:** Sort spec, limit
   - **Exists:** Relationship name, not_exists flag
4. **Connection config per source:** Sort ordering, applied filters, split edit keys

### Suggested Serialization Format

```json
{
  "queryID": "q1",
  "sources": ["issues", "comments"],
  "operators": [
    {
      "type": "filter",
      "predicate": {"op": "eq", "field": "status", "value": "open"}
    },
    {
      "type": "join",
      "parentKey": ["id"],
      "childKey": ["issueId"],
      "childTable": "comments"
    },
    {"type": "take", "sort": [["created", "desc"]], "limit": 10}
  ]
}
```

### Complexity Assessment

This is the **hardest part** of the phase. The TS pipeline builder (`buildPipeline` in `packages/zql/src/builder/`) creates a tree of operators with complex relationships (joins create sub-pipelines, exists wraps filters). Serializing this faithfully requires:

- Walking the operator tree after construction
- Extracting config from each operator type
- Handling nested pipelines (joins have child pipelines)
- Handling companion pipelines (scalar subqueries)

**Alternative approach:** Instead of serializing the full topology, keep the TS operator chain but parallelize at the TableSource fan-out level. Each connection's `output.push()` already runs independently -- Rayon can parallelize across connections without Rust needing to understand the full pipeline topology.

---

## 8. napi-rs Threading

### Can napi-rs Functions Spawn Rayon Threads?

Yes, with constraints:

- napi functions run on the main V8 thread
- They CAN spawn Rayon threads for CPU-bound work
- Results must be collected back on the main thread before returning to JS
- `ThreadsafeFunction` is needed ONLY for callbacks back to JS from worker threads -- not needed if Rust collects all results internally

### Sync vs Async Pattern

Two options:

1. **Sync napi with internal parallelism:** Rust function blocks the Node.js event loop while Rayon does parallel work. Acceptable if advance is already blocking (it is -- the current TS advance is synchronous generator).
2. **Async napi:** Returns a Promise, does work on a thread pool. More complex, less necessary since advance is already synchronous.

**Recommendation:** Option 1 (sync with internal Rayon). The advance loop is already a blocking synchronous operation. Adding async would change the calling contract unnecessarily.

### Key Constraint

- `JsObject`, `Env`, and all napi types are `!Send` -- cannot be passed to Rayon threads
- All JS <-> Rust marshalling must happen on the main thread
- Rayon workers operate on pure Rust data structures only
- Pattern: deserialize on main thread -> Rayon parallel processing -> serialize results on main thread

### Cargo Dependencies Needed

```toml
rayon = "1.10"
```

No special napi features needed since we're using sync functions with internal parallelism.

---

## Key Risks and Mitigations

### Risk 1: Pipeline Topology Serialization Complexity (HIGH)

The full TS pipeline is a complex operator tree with joins creating sub-pipelines. Serializing this to Rust is a major undertaking.
**Mitigation:** Consider a staged approach:

- Phase 13a: Parallelize at the connection fan-out level (within `genPushAndWriteWithSplitEdit`), keeping TS operators but running connection processing in parallel via a Rust coordinator
- Phase 13b: Move operator chains fully into Rust (if needed for further speedup)

### Risk 2: Overlay/State Mutation During Push (MEDIUM)

The push path modifies overlay state and writes to SQLite. These must remain serial.
**Mitigation:** Overlay is set once before connection iteration (immutable during fan-out). SQLite writes happen after all connections complete. State mutations in operators (Take) must use per-pipeline storage (already the case).

### Risk 3: Yield/Abort Semantics (MEDIUM)

The TS advance loop supports yielding for time-slicing and aborting on timeout. Rayon workers can't yield to JS.
**Mitigation:** Check timeout before and after Rayon parallel section. Within Rayon, use `AtomicBool` to signal abort, checked periodically by workers.

### Risk 4: Error Propagation (LOW)

`ResetPipelinesSignal` can be thrown during diff iteration. Must propagate correctly from Rust to TS.
**Mitigation:** Rust returns error codes that TS maps to appropriate signal types.

### Risk 5: Correctness Regression (MEDIUM)

Row change ordering and deduplication semantics must match exactly.
**Mitigation:** pipeline-driver.test.ts is the primary gate. Run in CI before merging.

---

## Recommended Plan Structure

### Plan 1: Rust Diff Reader

Port the snapshot diff logic to Rust. Create a Rust function that reads `_zero.changeLog2`, fetches prev/next values, and returns materialized `Vec<Change>`. This replaces the TS `Diff[Symbol.iterator]()` with a single Rust call.

- Reuse existing `changes_since_buf()` infrastructure
- Add row fetching (`getRow`, `getRows`) in Rust
- Handle RESET/TRUNCATE signals
- Return changes as JSON array or binary buffer to TS

### Plan 2: Rayon Fan-out Coordinator

Create a Rust napi function `rust_advance()` that:

1. Takes: db path, prev version, curr version, pipeline topology configs
2. Opens own SQLite connection(s) for reading diff
3. Materializes all changes
4. Uses `par_iter` to fan out changes across pipeline operator chains
5. Returns aggregated `RowChange[]` to TS

### Plan 3: TS Integration + Fallback

Wire `pipeline-driver.ts#advance()` to call `rust_advance()`:

- Serialize pipeline topology when pipelines are added
- Call Rust advance instead of TS generator
- Fallback to TS path for unsupported pipeline configurations
- Handle error codes -> `ResetPipelinesSignal`

### Alternative: Simpler Fan-out Only

If full topology serialization proves too complex, a simpler approach:

1. Keep TS diff reading and operator chains
2. Create a Rust napi function that takes pre-computed changes and pipeline connection configs
3. Parallelize only the connection fan-out within `genPush` -- each connection's `output.push()` runs on a Rayon thread
4. Collect results and return to TS

This gives parallelism benefits with much less serialization complexity, but still requires Rust to understand operator evaluation (already partially done with filter/join/take/exists).

---

## RESEARCH COMPLETE
