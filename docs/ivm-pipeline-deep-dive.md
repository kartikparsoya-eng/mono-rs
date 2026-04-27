# Zero IVM Pipeline — Deep Dive

A complete walkthrough of Incremental View Maintenance in zero-cache: how queries
become live pipelines, how data flows during hydration and advance, and how the
Rust and TypeScript paths diverge and reconnect.

---

## Table of Contents

1. [High-Level Architecture](#1-high-level-architecture)
2. [Lifecycle: From WebSocket to Pipeline](#2-lifecycle-from-websocket-to-pipeline)
3. [Pipeline Construction](#3-pipeline-construction)
4. [Operator Tree](#4-operator-tree)
5. [Hydration (Initial Materialization)](#5-hydration-initial-materialization)
6. [Advance (Incremental Maintenance)](#6-advance-incremental-maintenance)
7. [Post-Rust-Hydration Fixup](#7-post-rust-hydration-fixup)
8. [NAPI Boundary & Serialization](#8-napi-boundary--serialization)
9. [Snapshotter & Diff Computation](#9-snapshotter--diff-computation)
10. [Operator Deep Dive](#10-operator-deep-dive)
11. [Rust vs TS — Side-by-Side](#11-rust-vs-ts--side-by-side)
12. [File Reference](#12-file-reference)

---

## 1. High-Level Architecture

```
  PostgreSQL (source of truth)
       |
       | CDC (logical replication)
       v
  ChangeStreamer ──────────────────────────────┐
       |                                       |
       | replication stream                    |
       v                                       |
  Replicator                                   |
       |                                       |
       | writes to SQLite replica              |
       v                                       |
  ┌─────────────────────────┐                  |
  │   SQLite Replica        │                  |
  │   (WAL2 mode)           │                  |
  └────────┬────────────────┘                  |
           |                                   |
           |  snapshot pairs                   |
           v                                   |
  ┌─────────────────────────┐                  |
  │   Snapshotter           │                  |
  │   prev DB  |  curr DB   │                  |
  │   (read)   |  (read)    │                  |
  └────┬───────┴────┬───────┘                  |
       |            |                          |
       |  diff      |  direct read             |
       v            v                          |
  ┌──────────────────────────────────────────┐ |
  │            PipelineDriver                │ |
  │                                          │ |
  │  ┌─ Pipeline 1 ──────────────────────┐   │ |
  │  │  TableSource -> Filter -> Take    │   │ |
  │  └──────────────────────────────────-┘   │ |
  │  ┌─ Pipeline 2 ──────────────────────┐   │ |
  │  │  TableSource -> Join -> Exists    │   │ |
  │  │                  -> Take          │   │ |
  │  └──────────────────────────────────-┘   │ |
  │  ...per query from this client           │ |
  └──────────┬───────────────────────────────┘ |
             |                                 |
             | RowChange stream                |
             v                                 |
  ┌──────────────────────────────────────────┐ |
  │          ViewSyncer                      │ |
  │   (one per client group / browser tab)   │ |
  │   updates CVR, sends patches over WS     │ |
  └──────────────────────────────────────────┘ |
             |                                 |
             | WebSocket                       |
             v                                 |
         Client (Replicache)                   |
```

**Key idea**: Each connected client gets its own `ViewSyncer` holding its own
`PipelineDriver` with its own set of IVM pipelines. A single PostgreSQL commit
fans out to every connected ViewSyncer.

---

## 2. Lifecycle: From WebSocket to Pipeline

```
Client connects via WebSocket
       |
       v
ViewSyncer created (one per clientGroup)
  file: view-syncer.ts
       |
       | client sends queries via initConnectionMessage / changeDesiredQueriesMessage
       v
ViewSyncer.#syncQueryPipelineSet()
  file: view-syncer.ts
       |
       | resolves ASTs, applies permissions (query planner rewrites if enabled)
       v
PipelineDriver.addQueries(queries[])
  file: pipeline-driver.ts:825
       |
       | Phase 1: build TS operator trees
       | Phase 2: batch Rust hydration (NAPI)
       | Phase 3: yield initial RowChanges
       v
RowChanges -> CVR diff -> WebSocket patches to client
       |
       v
Client has initial data. Pipeline is now LIVE.
       |
       | PostgreSQL commits arrive...
       v
Snapshotter.advance() -> SnapshotDiff
       |
       v
PipelineDriver.advance(diff) -> incremental RowChanges
       |
       v
ViewSyncer sends delta patches to client
```

### Connection flow in detail

1. **Client connects** — `ViewSyncer` is created or resumed for the `clientGroupID`
   (`view-syncer.ts`)
2. **Queries arrive** — `ViewSyncer.#syncQueryPipelineSet()` computes the diff
   between current and desired query sets
3. **Permissions applied** — If query planner is enabled (`ZERO_ENABLE_QUERY_PLANNER`),
   queries are rewritten with permission predicates and optimized via cost model
4. **Pipeline creation** — `PipelineDriver.addQueries()` builds operator trees and
   hydrates them
5. **Hydration** — Initial data computed (Rust or TS path), sent to client
6. **Advance loop** — On each PG commit, `Snapshotter` produces a diff,
   `PipelineDriver.advance()` pushes changes through live pipelines

---

## 3. Pipeline Construction

When `addQueries()` is called, each query goes through:

```
AST (from client)
  |
  | permission rewrite (if query planner enabled)
  v
Transformed AST
  |
  v
buildPipeline(ast, delegate)       <-- zql/src/builder/builder.ts
  |
  | delegate provides:
  |   getSource(table) -> TableSource
  |   createStorage()  -> TakeStorage (Rust or TS)
  |   costModel        -> SQLiteCostModel (optional)
  |
  v
IVM Operator Tree (TS)
  |
  | simultaneously, for Rust-eligible queries:
  v
ast_to_operator_configs(ast)       <-- zqlite-rs/src/ast_to_config.rs
  |
  v
Vec<OperatorConfig> (Rust)         <-- used by rustHydrate + rustAdvance
```

### The delegate (`pipeline-driver.ts:596-642`)

| Delegate method       | What it does                                                           |
| --------------------- | ---------------------------------------------------------------------- |
| `getSource(table)`    | Lazily creates a `TableSource` backed by SQLite snapshot (line 601)    |
| `createStorage()`     | Returns `RustTakeStorage` (if Rust IVM on) or TS `Storage` (line 602)  |
| `decorateSourceInput` | Wraps source with `MeasurePushOperator` for metrics (line 612)         |
| `decorateFilterInput` | Wraps EXISTS filters with `RustExistsWrapper` if enabled (line 621)    |
| `costModel`           | `ConnectionCostModel` from SQLite stats, if planner enabled (line 641) |

### Rust eligibility check

A query is **Rust-eligible** for hydration if it has **no correlated subqueries
in the WHERE clause** (`hasCSQInWhere`). Queries with EXISTS/NOT EXISTS in WHERE
fall back to TS hydration in the batch path (pipeline-driver.ts:949).

---

## 4. Operator Tree

### TS Operator Tree (built by `buildPipeline`)

```
For: SELECT * FROM issues
       WHERE EXISTS labels AND priority > 3
       ORDER BY created DESC
       LIMIT 10

TableSource("issues")                     <-- reads SQLite
  |
  v
Join(parent=issues, child=labels,         <-- attaches child relationship
     parentKey=[id], childKey=[issueId],
     relationshipName="labels")
  |
  v
FilterStart                               <-- begins filter chain
  |
  v
ExistsOperator(relationship="labels")     <-- filters: has labels?
  |
  v
Filter(predicate: priority > 3)           <-- filters: priority check
  |
  v
FilterEnd                                 <-- ends filter chain
  |
  v
Take(limit=10, sort=[created DESC])       <-- windowed limit
  |
  v
Output (-> Streamer -> RowChanges)
```

### Rust Operator Tree (built by `build_operator_with_live_source`)

```
LiveTableSource("issues")                 <-- reads SQLite via RustTableSource
  |                                            (shared ConnectionPool, Rayon parallel)
  v
FilterOperator(priority > 3)              <-- predicate from AST WHERE
  |
  v
ParallelExistsOperator(                   <-- batch WHERE IN child fetch
     child=LiveTableSource("labels"),
     parentKey=[id], childKey=[issueId])
  |
  v
TakeOperator(limit=10,                    <-- windowed limit
     sort=[created DESC])
  |
  v
Vec<Node> result
```

**Key differences:**

- Rust uses `ParallelJoinOperator` / `ParallelExistsOperator` — batch `WHERE IN`
  queries instead of per-row child fetches
- Rust uses `LiveTableSource` which reads SQLite directly via a shared
  `ConnectionPool` (sized to `rayon::current_num_threads().max(4)`)
- TS uses lazy child streams in Join; Rust pre-fetches children in batches

---

## 5. Hydration (Initial Materialization)

Hydration computes the **full query result** from scratch — this is step 0 of IVM,
populating the materialized view.

### Two hydration paths

```
                    addQueries()
                        |
                        v
              ┌─ Rust eligible? ─┐
              │                  │
             YES                 NO
              │                  │
              v                  v
     Rust Hydration        TS Hydration
     (batch NAPI)          (operator fetch)
              │                  │
              v                  v
         Binary Buffer      RowChange stream
              │                  │
              └────────┬─────────┘
                       v
              Post-hydration fixup
              (initializeTakeState +
               warm-up fetch)
                       |
                       v
              Pipeline is LIVE
```

### 5a. TS Hydration — `hydrateInternal()` (pipeline-driver.ts:2237)

**Pull-based**. The output calls `input.fetch({})` on the root operator, which
recursively pulls data through the entire tree.

```
Output calls Take.fetch({})
  |
  Take calls FilterEnd.fetch({})
    |
    FilterEnd calls FilterStart.fetch({})
      |
      FilterStart calls Join.fetch({})
        |
        Join calls TableSource.fetch({})
          |
          TableSource reads SQLite rows via BTree index
          |
          v
        Join receives parent rows
          |
          for each parent: attaches lazy child stream
          (child stream = child_source.fetch({constraint}))
          |
          v
      FilterStart receives joined nodes
        |
        for each node: calls Exists.filter(node)
          |
          Exists checks: does relationship have children?
          (iterates the lazy child stream, counts)
          |
          if passes: calls Filter.filter(node)
            |
            Filter checks: priority > 3?
            |
            v
      FilterStart yields passing nodes
        |
        v
    Take.#initialFetch() counts up to limit
      |
      stores TakeState: { size: N, bound: lastRow }
      |
      v
  Output receives nodes, converts to RowChanges
```

**Critical side effect**: As data flows through operators during fetch, each
operator initializes its internal state. Take stores `{size, bound}`. Exists
caches relationship sizes. This state is what makes the pipeline reactive for
subsequent advance pushes.

### 5b. Rust Hydration — `rustHydrate()` (advance.rs:1197)

**Completely separate operator tree**. Does NOT use the TS operators at all.

```
TS side: pipeline-driver.ts
  |
  | serialize: [{query_id, ast, primary_key, column_types}]
  |
  v
NAPI call: rustHydrate(db_path, queries_json)
  |
  v
Rust side: advance.rs:1197
  |
  | 1. Parse Vec<HydrateQuery>
  | 2. ast_to_operator_configs() — AST -> Vec<OperatorConfig>
  | 3. Create ConnectionPool (sized to rayon threads)
  | 4. Create RustTableSource per table group
  |
  v
hydrate_pipelines()                        <-- hydrate.rs:517
  |
  | configs.into_par_iter()  <-- RAYON PARALLEL
  |
  | per pipeline (on separate Rayon thread):
  v
build_operator_with_live_source()          <-- hydrate.rs:549
  |
  | LiveTableSource -> Filter -> ParallelJoin -> Take
  |
  v
operator.fetch(FetchRequest::default())
  |
  | LiveTableSource.fetch()
  |   -> RustTableSource.fetch()
  |     -> build_select_query() -> SQL string
  |     -> SQLite query execution
  |     -> Vec<Node>
  |
  | ParallelJoinOperator.fetch()
  |   -> batch_fetch_children()
  |     -> single WHERE child_key IN (v1, v2, ...) query
  |     -> group results by parent key
  |     -> attach to parent nodes
  |
  | TakeOperator.fetch()
  |   -> take first N nodes
  |   -> record {size, bound}
  |
  v
Vec<Node> per pipeline
  |
  | flatten_nodes_to_row_changes()
  |   -> recursively walk Node tree
  |   -> emit one "add" RowChange per node per relationship
  |
  v
encode_advance_result_buf()                <-- binary Buffer
  |
  v
NAPI return -> Buffer back to TS
  |
  v
TS side: decodeAdvanceResultBuf()          <-- decode-advance-buf.ts
  |
  v
Vec<DecodedRowChange> -> yield to ViewSyncer
```

**Why Rust is 12x faster:**

1. Rayon parallelism — multiple pipelines hydrate simultaneously on thread pool
2. Direct SQLite access — no JS overhead, no generator/yield machinery
3. Batch child fetches — `WHERE IN` instead of per-row correlated subqueries
4. Binary encoding — avoids JSON serialization for the result buffer

---

## 6. Advance (Incremental Maintenance)

When PostgreSQL commits new data, the advance path pushes **only changed rows**
through live pipelines.

### Advance lifecycle

```
PostgreSQL commit
       |
       v
Replicator writes to SQLite replica
       |
       v
Snapshotter.advance()
  |
  | opens two SQLite connections:
  |   prev = snapshot before commit
  |   curr = snapshot after commit
  |
  | computes diff:
  |   for each table:
  |     scan rows where _0_version > prevVersion
  |     compare prev vs curr values
  |
  v
SnapshotDiff: [{table, prevValues[], nextValue}]
       |
       v
PipelineDriver.advance(diff) or PipelineDriver.advanceWithRustDispatch(diff)
       |
       v
  ┌─ useRustAdvance? ─┐
  │                    │
 YES                   NO
  │                    │
  v                    v
Rust fan-out       TS advance
(NAPI)             (operator push)
```

### 6a. TS Advance — `#advance()` (pipeline-driver.ts:1258)

**Push-based**. Changes propagate bottom-up through the operator tree.

```
SnapshotDiff entry: { table: "issues", prevValues: [old], nextValue: new }
  |
  | classify: ADD / REMOVE / EDIT
  |
  v
TableSource.genPush(sourceChange)
  |
  | sets overlay (pending change visible to fetch-during-push)
  | pushes to all connected operator trees
  |
  v
Join.pushParent(change)
  |
  | wraps node with lazy child relationship
  | (overlay-aware: children reflect pending state)
  |
  v
FilterStart.push(change)
  |
  v
Exists.push(change)
  |
  | For ADD/REMOVE/EDIT: check if relationship still satisfies exists
  | For CHILD change:
  |   CHILD ADD + size goes 0->1: emit ADD(parent) [exists], REMOVE(parent) [not exists]
  |   CHILD REMOVE + size goes 1->0: emit REMOVE(parent) [exists], ADD(parent) [not exists]
  |
  v
Filter.push(change)
  |
  | ADD: forward if predicate(new_row) passes
  | REMOVE: forward if predicate(old_row) passes
  | EDIT: split logic:
  |   both pass -> forward EDIT
  |   old passes, new fails -> convert to REMOVE
  |   old fails, new passes -> convert to ADD
  |   neither -> drop
  |
  v
Take.push(change)
  |
  | ADD:
  |   under limit -> increment size, forward
  |   at limit, row < bound -> DISPLACE: remove bound, add new, find new bound
  |   at limit, row >= bound -> drop (outside window)
  |
  | REMOVE:
  |   row > bound -> drop (outside window)
  |   row <= bound -> forward, fetch REPLACEMENT from after bound
  |     replacement found -> emit ADD(replacement), update bound
  |     no replacement -> shrink window
  |
  | EDIT:
  |   complex: may split into remove+add if row crosses bound
  |
  v
Output -> Streamer -> RowChanges -> ViewSyncer -> WebSocket -> Client
```

### 6b. Rust Advance — Three variants

#### Filter-only fan-out: `rust_fan_out()` / `rust_dispatch_poke()` (advance.rs:639/1067)

Used when ALL pipelines are filter-only (no Join/Take/Exists).

```
TS: changes[] + pipelineConfigs[]
       |
       v
NAPI: rust_dispatch_poke(changes_json, vs_pipelines_json)
       |
       | Level 1 Rayon: par_iter over ViewSyncers
       | Level 2 Rayon: par_iter over pipelines within each VS
       |
       | per pipeline per change:
       v
process_change_for_pipeline()              <-- advance.rs:349
  |
  | 1. Is change.table in pipeline.source_tables? No -> skip
  |
  | 2. ADD (no prev, has next):
  |    passes_filter(next_row)? -> emit "add"
  |
  | 3. EDIT (has prev AND next):
  |    find prev row matching next by PK
  |    4-way split:
  |      old passes + new passes -> emit "edit"
  |      old passes + new fails  -> emit "remove"
  |      old fails  + new passes -> emit "add"
  |      old fails  + new fails  -> drop
  |
  | 4. DELETE (has prev, no next):
  |    passes_filter(prev_row)? -> emit "remove"
  |
  v
Binary Buffer -> TS decode -> RowChanges
```

#### Full advance: `rust_advance_full()` (advance.rs:939)

Used when pipelines have Join/Take/Exists (currently gated off in production).

```
TS: changes[] + fullPipelineConfigs[] (includes operator_config)
       |
       v
NAPI: rust_advance_full_buf(db_path, changes_json, configs_json)
       |
       | par_iter over pipelines (Rayon)
       |
       | per pipeline:
       v
process_full_pipeline()                    <-- advance.rs:818
  |
  | 1. Create RustTableSource for this pipeline
  | 2. build_push_operator_chain()         <-- hydrate.rs:767
  |    (sequential operators: Filter -> JoinOp -> ExistsOp -> TakeOp)
  |
  | 3. For each diff change:
  |    diff_change_to_source_changes()     <-- advance.rs:697
  |    -> SourceChange (Add/Remove/Edit)
  |
  | 4. op_chain.push(ivm_change)
  |    -> propagates through operator tree via push()
  |    -> output Vec<IvmChange>
  |
  | 5. ivm_change_to_row_changes()
  |    -> Vec<RowChange>
  |
  v
Binary Buffer -> TS decode -> RowChanges
```

### When is Rust advance used? (`#reevaluateRustAdvance`, pipeline-driver.ts:1430)

ALL of these must be true:

1. `USE_RUST_ADVANCE` env flag is on
2. At least one pipeline exists
3. Every pipeline has a Rust config (no CSQ-in-WHERE queries)
4. Every pipeline is **filter-only** (no Take/Join/Exists) — OR dual-exec testing mode
5. No pipeline has companion (scalar subquery) pipelines
6. No table has more than 1 unique key

In practice: only simple `SELECT ... WHERE ... ` queries without LIMIT, JOIN,
or EXISTS use Rust advance in production.

---

## 7. Post-Rust-Hydration Fixup

After Rust hydration, the **TS operator tree has empty state**. Rust used its own
separate operator tree. But advance must flow through the TS tree. Two fixups bridge this gap:

### Fixup 1: `initializeTakeState()` (pipeline-driver.ts:2267)

```
Rust hydration computed: 10 rows, bound = {created: "2024-01-15", id: "issue-42"}
       |
       v
initializeTakeState(takeStorage, {size: 10, bound: {...}})
       |
       | writes to TS TakeStorage:
       |   key '["take"]' -> {size: 10, bound: {...}}
       |   key 'maxBound'  -> {created: "2024-01-15", id: "issue-42"}
       |
       v
TS Take operator now knows:
  - window has 10 rows
  - boundary row is {created: "2024-01-15", id: "issue-42"}
  - can correctly evaluate push() for adds/removes/edits
```

**Why needed**: Without this, `Take.push()` sees `size=0` and silently drops all
changes — the pipeline becomes non-reactive.

### Fixup 2: Warm-up fetch (pipeline-driver.ts:695-701)

Only in single `addQuery()`, NOT in batch `addQueries()`:

```
input.fetch({})   <-- runs full TS operator tree
  |
  | BUT: all results are DISCARDED
  |
  | Side effects that matter:
  |   - Child Take operators in EXISTS/JOIN subqueries
  |     run their #initialFetch(), populating partition state
  |   - Exists operator caches are warmed
  |
  v
TS operator tree is now fully initialized for advance
```

**Why needed**: `initializeTakeState()` only handles the top-level Take. Nested
Takes (inside related subqueries) need their `#initialFetch` triggered. The
warm-up fetch walks the full tree, triggering all nested initializations.

```
Pipeline with nested Take:

Take(limit=10)                     <-- fixed by initializeTakeState()
  |
  Join(issues -> labels)
    |
    child pipeline:
      Take(limit=5)                <-- fixed by warm-up fetch
        |
        FilterEnd
```

---

## 8. NAPI Boundary & Serialization

### Two NAPI binaries

```
┌─────────────────────────────────────────────┐
│  zqlite-rs.node                             │
│  (packages/zqlite-rs)                       │
│                                             │
│  Exports:                                   │
│    rustHydrate(db_path, queries_json)       │  -> Binary Buffer
│    rustFanOut(changes, configs)             │  -> JSON / Binary Buffer
│    rustDispatchPoke(changes, vs_pipelines)  │  -> Binary Buffer
│    rustAdvanceFull(db, changes, configs)    │  -> Binary Buffer
│    + SQLite Database operations             │
│                                             │
│  Contains: hydrate.rs, advance.rs,          │
│    ast_to_config.rs, query_builder.rs       │
│  Depends on: zero-ivm-rs (Cargo)            │
└─────────────────────────────────────────────┘

┌─────────────────────────────────────────────┐
│  zero-ivm-rs.node                           │
│  (packages/zero-ivm-rs)                     │
│                                             │
│  Exports:                                   │
│    Pipeline (build, fetch, push)            │  -> JSON strings
│    RustFilterPredicate (evaluate, batch)    │  -> direct JsObject
│    RustTakeState (get/set state, compare)   │  -> direct JsObject
│    RustExistsWrapper (decide, batch)        │  -> JSON strings
│    RustJoinHelper (match, batch)            │  -> JSON strings
│    RustStorage (get, set, scan)             │  -> strings
│                                             │
│  Contains: filter.rs, join.rs, exists.rs,   │
│    take_op.rs, pipeline.rs, storage.rs      │
└─────────────────────────────────────────────┘
```

### Binary result format (`encode_advance_result_buf`, advance.rs:957)

```
┌──────────────────────────────────────────────────────────┐
│  [u32 LE]  change_count                                  │
│  [u8]      has_error (0=ok, 1=error)                     │
│                                                          │
│  If has_error:                                           │
│    [u16 LE + bytes]  error message                       │
│    [u16 LE + bytes]  error type                          │
│                                                          │
│  Per change (repeated change_count times):               │
│    [u8]              change_type                         │
│                      0=add, 1=remove, 2=edit             │
│    [u16 LE + bytes]  query_id                            │
│    [u16 LE + bytes]  table name                          │
│    [value]           row_key (encoded)                   │
│    [u8]              has_row (0=no, 1=yes)               │
│                                                          │
│    If has_row:                                           │
│      [u16 LE]          column_count                      │
│      Per column:                                         │
│        [u16 LE + bytes]  column_name                     │
│        [value]           column_value (encoded)          │
└──────────────────────────────────────────────────────────┘

Value encoding tags:
  0 = null       (no payload)
  1 = i64        (8 bytes LE)
  2 = f64        (8 bytes LE)
  3 = text       (u32 LE len + UTF-8 bytes)
  4 = blob       (u32 LE len + raw bytes)
  5 = bool       (u8: 0 or 1)
  6 = json       (u32 LE len + JSON string)
```

### `rust_dispatch_poke` multi-VS format

```
┌──────────────────────────────────────────────────────────┐
│  [u32 LE]  vs_count                                      │
│                                                          │
│  Per ViewSyncer:                                         │
│    [u16 LE + bytes]  vs_id                               │
│    [u8]              has_error (0=ok, 1=error)           │
│                                                          │
│    If has_error:                                         │
│      [u16 LE + bytes]  error message                     │
│    Else:                                                 │
│      [u32 LE]          change_count                      │
│      Per change:       (same as above)                   │
└──────────────────────────────────────────────────────────┘
```

### Data serialization by path

| Path                  | Input format          | Output format   | Notes                                |
| --------------------- | --------------------- | --------------- | ------------------------------------ |
| `rustHydrate`         | JSON string (queries) | Binary Buffer   | Fastest; avoids JSON output overhead |
| `rustFanOut`          | JSON strings          | JSON or Binary  | Binary for dispatch_poke             |
| `rustAdvanceFull`     | JSON strings          | Binary Buffer   | Full operator push                   |
| `Pipeline.fetch/push` | JSON strings          | JSON strings    | Slower; used for testing/compat      |
| `RustFilterPredicate` | Direct JsObject       | Direct JsObject | Fastest NAPI; raw field extraction   |
| `RustTakeState`       | Direct JsObject       | Direct JsObject | State management, row comparison     |

---

## 9. Snapshotter & Diff Computation

```
SQLite Replica (WAL2 mode)
       |
       | Snapshotter maintains TWO read connections:
       |   prev: last known state
       |   curr: latest state after replicator writes
       |
       v
Snapshotter.advance()
  |
  | 1. Open new read snapshot (curr)
  | 2. For each table:
  |    SELECT * FROM table WHERE _0_version > prevVersion
  |    (only reads rows changed since last advance)
  |
  | 3. For changed rows:
  |    prevValues = rows from prev snapshot matching same PKs
  |    nextValue  = row from curr snapshot
  |
  | 4. Produce SnapshotDiff entries:
  |    { table, prevValues: Row[], nextValue: Row | null }
  |
  |    prevValues with no nextValue = DELETE
  |    nextValue with no matching prev = INSERT
  |    both present with same PK = UPDATE
  |
  v
SnapshotDiff -> PipelineDriver.advance()

Configuration:
  - journal_mode = wal2 (custom SQLite extension)
  - synchronous = OFF (changes are ephemeral, rebuilt from PG)
  - Read connections use snapshot isolation
```

**WAL2 advantage**: Allows concurrent reads while the replicator writes.
Standard WAL only allows one writer and blocks during checkpoint. WAL2
supports begin-concurrent for multiple readers without blocking.

---

## 10. Operator Deep Dive

### 10a. TableSource / MemorySource (zql/src/ivm/memory-source.ts)

The root of every pipeline. Stores all rows for a table in sorted BTree indexes.

```
TableSource
  |
  | State:
  |   primary BTree index (sorted by PK)
  |   secondary BTree indexes (created on-demand per sort order)
  |   overlay: pending change during push (for consistent fetch-during-push)
  |   connections: SourceInput[] (one per pipeline using this table)
  |
  | fetch(req):
  |   1. Pick BTree index matching sort order
  |   2. Scan from start cursor
  |   3. Apply constraint filter
  |   4. Splice in overlay (pending push change)
  |   5. Yield rows
  |
  | push(sourceChange):
  |   1. Set overlay
  |   2. For each connection:
  |      - Push change to downstream operators
  |      - Yield between connections (time-slicing)
  |   3. Clear overlay
  |   4. Write change to all BTree indexes
```

**Overlay mechanism**: When a push is in progress, if a downstream operator calls
`fetch()` back into the source (e.g., Take looking up the bound), the overlay
ensures the pending change is visible. Without it, the data would be inconsistent:
the push says "row added" but fetch wouldn't find it yet.

### 10b. Join Operator (zql/src/ivm/join.ts)

Attaches child relationships to parent rows. The core of hierarchical queries.

```
Join
  |
  | State:
  |   parent: Input (upstream operator)
  |   child: Input (child pipeline — separate source)
  |   parentKey, childKey: correlation columns
  |   relationshipName: where to attach children on Node
  |
  | fetch():
  |   for each parent node:
  |     attach LAZY child generator as relationship
  |     (child.fetch({constraint: parentKey -> childKey}) called on demand)
  |
  | pushParent(change):
  |   wrap changed node with child relationship, forward
  |
  | pushChild(change):
  |   1. Build constraint from child row's FK values
  |   2. Fetch ALL matching parents via parent.fetch({constraint})
  |   3. For each parent: wrap as CHILD change, push downstream
  |   4. Track inProgressChildChangePosition for overlay consistency
```

**Key insight**: During push, a child change triggers a **reverse lookup** —
find all parents matching this child's FK, then push a CHILD change for each.
This is how `INSERT INTO labels` propagates to all affected issue pipelines.

### 10c. Exists Operator (zql/src/ivm/exists.ts)

Filters rows based on whether a relationship has children.

```
Exists
  |
  | State:
  |   cache: Map<parentKey, boolean>   (cleared per filter pass)
  |   not: boolean                     (NOT EXISTS mode)
  |
  | filter(node):  [during fetch]
  |   count children in relationship
  |   EXISTS: pass if count > 0
  |   NOT EXISTS: pass if count == 0
  |
  | push(change):  [during advance]
  |   ADD/REMOVE/EDIT: check if relationship still satisfies exists
  |
  |   CHILD ADD for this relationship:
  |     fetch current size
  |     size 0 -> 1:  EXISTS: emit ADD(parent)
  |                   NOT EXISTS: emit REMOVE(parent)
  |     size > 1:     forward change
  |
  |   CHILD REMOVE for this relationship:
  |     fetch current size
  |     size 1 -> 0:  EXISTS: emit REMOVE(parent)
  |                   NOT EXISTS: emit ADD(parent)
  |     size > 0:     forward change
```

**The 0<->1 transition** is the critical event. When a parent goes from having
no children to having one (or vice versa), the EXISTS predicate flips, and the
parent row enters or leaves the result set.

### 10d. Take Operator (zql/src/ivm/take.ts)

Implements `LIMIT`. The most complex operator due to window management.

```
Take
  |
  | State (per partition):
  |   size: number            (current rows in window)
  |   bound: Row | undefined  (last row in window by sort order)
  |   maxBound: Row           (max bound across all partitions)
  |
  | #initialFetch():  [first fetch only]
  |   pull from upstream, count up to limit
  |   store {size, bound}
  |
  | push ADD:
  |   ┌─ size < limit ──────────────────┐
  |   │  increment size                  │
  |   │  update bound if this is new max │
  |   │  forward ADD                     │
  |   └─────────────────────────────────-┘
  |   ┌─ size == limit, row >= bound ───┐
  |   │  DROP (outside window)           │
  |   └─────────────────────────────────-┘
  |   ┌─ size == limit, row < bound ────┐
  |   │  DISPLACE:                       │
  |   │  1. emit REMOVE(current_bound)   │
  |   │  2. emit ADD(new_row)            │
  |   │  3. fetch to find new bound      │
  |   └─────────────────────────────────-┘
  |
  | push REMOVE:
  |   ┌─ row > bound ──────────────────-┐
  |   │  DROP (outside window)           │
  |   └─────────────────────────────────-┘
  |   ┌─ row <= bound ─────────────────-┐
  |   │  emit REMOVE(row)               │
  |   │  fetch replacement after bound   │
  |   │  found? -> emit ADD(replacement) │
  |   │  not found? -> shrink window     │
  |   └─────────────────────────────────-┘
  |
  | push EDIT:
  |   compare old/new positions vs bound
  |   may split into REMOVE + ADD if row crosses boundary
```

**Displacement example**: Window is `[A, B, C]` (limit=3, bound=C). New row `B2`
arrives where `B2 < C`. Take emits `REMOVE(C)`, `ADD(B2)`. Window becomes
`[A, B, B2]`, new bound = `B2`.

### 10e. Filter Operator (zql/src/ivm/filter.ts)

Stateless predicate evaluation.

```
Filter
  |
  | filter(node):  [during fetch, via FilterStart]
  |   return predicate(node.row) && yield* downstream.filter(node)
  |
  | push(change):  [during advance]
  |   ADD:    forward if predicate(row) passes
  |   REMOVE: forward if predicate(row) passes
  |   EDIT:   4-way split:
  |     old passes + new passes -> forward EDIT
  |     old passes + new fails  -> convert to REMOVE(old)
  |     old fails  + new passes -> convert to ADD(new)
  |     old fails  + new fails  -> drop
```

---

## 11. Rust vs TS — Side-by-Side

### Hydration comparison

```
                  TS Hydration                    Rust Hydration
                  ──────────                      ──────────────
Trigger:          addQuery() fallback             addQueries() batch / #rustHydrate()
Parallelism:      Sequential                      Rayon par_iter (thread pool)
SQLite access:    via TS TableSource              via RustTableSource + ConnectionPool
Join strategy:    Lazy per-row fetch              Batch WHERE IN query
Exists strategy:  Per-row count                   Batch WHERE IN + count
Result format:    Generator<RowChange>            Binary Buffer (decoded in TS)
State init:       Automatic (fetch side effects)  Manual (initializeTakeState + warm-up)
Speed:            1x baseline                     12-13.5x at scale
```

### Advance comparison

```
                  TS Advance                      Rust Advance (filter-only)
                  ──────────                      ─────────────────────────
Trigger:          always available                only if all pipelines filter-only
Parallelism:      Sequential per pipeline         Rayon par_iter
Operators:        Full tree (Join/Take/Exists)    Filter evaluation only
State:            Full operator state             Stateless (no Take/Join state)
Speed:            ~1x (trivially fast per row)    ~1x through PipelineDriver
                                                  (NAPI JSON overhead cancels Rayon gain)
Production use:   Yes — all queries               Limited — filter-only queries
```

### Operator implementation comparison

| Operator | TS                              | Rust (pipeline)                       | Rust (NAPI helper)                         |
| -------- | ------------------------------- | ------------------------------------- | ------------------------------------------ |
| Source   | `MemorySource` (BTree, overlay) | `SourceOperator` (in-memory Vec)      | `LiveTableSource` (direct SQLite)          |
| Filter   | `filter.ts` (stateless)         | `filter_op.rs` (stateless)            | `RustFilterPredicate` (raw JsObject batch) |
| Join     | `join.ts` (lazy child, overlay) | `join_op.rs` (sequential)             | `ParallelJoinOperator` (batch WHERE IN)    |
| Exists   | `exists.ts` (0<->1 detection)   | `exists_op.rs` (parent_sizes HashMap) | `ParallelExistsOperator` (batch)           |
| Take     | `take.ts` (Storage-backed)      | `take_op.rs` (HashMap state)          | `RustTakeState` (napi state manager)       |
| Skip     | (in builder)                    | `skip_op.rs` (bound filter)           | —                                          |
| Cap      | (in builder)                    | `cap_op.rs` (PK tracking)             | —                                          |

---

## 12. File Reference

### Pipeline orchestration

| File                                                        | Key contents                                                                                    |
| ----------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `zero-cache/src/services/view-syncer/pipeline-driver.ts`    | `PipelineDriver` class, `addQueries()`, `#advance()`, `#rustHydrate()`, `initializeTakeState()` |
| `zero-cache/src/services/view-syncer/view-syncer.ts`        | `ViewSyncer` — per-client lifecycle, `#syncQueryPipelineSet()`, advance loop                    |
| `zero-cache/src/services/view-syncer/snapshotter.ts`        | `Snapshotter` — SQLite snapshot pairs, diff computation                                         |
| `zero-cache/src/services/view-syncer/decode-advance-buf.ts` | `decodeAdvanceResultBuf()` — binary buffer decoding                                             |

### TS IVM operators

| File                              | Operator                                                |
| --------------------------------- | ------------------------------------------------------- |
| `zql/src/ivm/operator.ts`         | `Input`, `Output`, `Operator`, `Storage` interfaces     |
| `zql/src/ivm/memory-source.ts`    | `MemorySource` — BTree-backed table source with overlay |
| `zql/src/ivm/take.ts`             | `Take` — windowed LIMIT with bound management           |
| `zql/src/ivm/filter.ts`           | `Filter` — stateless predicate                          |
| `zql/src/ivm/join.ts`             | `Join` — hierarchical parent-child with lazy streams    |
| `zql/src/ivm/exists.ts`           | `Exists` — EXISTS/NOT EXISTS with 0<->1 transition      |
| `zql/src/ivm/filter-operators.ts` | `FilterStart`, `FilterEnd` — filter chain adapters      |

### Rust hydration & advance (zqlite-rs)

| File                             | Key contents                                                                                                                    |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `zqlite-rs/src/hydrate.rs`       | `hydrate_pipelines()`, `build_operator_with_live_source()`, `LiveTableSource`, `ParallelJoinOperator`, `ParallelExistsOperator` |
| `zqlite-rs/src/advance.rs`       | `rust_hydrate()`, `rust_fan_out()`, `rust_dispatch_poke()`, `rust_advance_full()`, binary encoding, NAPI entry points           |
| `zqlite-rs/src/ast_to_config.rs` | `ast_to_operator_configs()` — AST to Rust operator config translation                                                           |
| `zqlite-rs/src/query_builder.rs` | `build_select_query()` — SQL generation for SQLite                                                                              |

### Rust IVM operators (zero-ivm-rs)

| File                            | Operator                                                       |
| ------------------------------- | -------------------------------------------------------------- |
| `zero-ivm-rs/src/operator.rs`   | `Operator` trait (`fetch`, `push`, `op_type`)                  |
| `zero-ivm-rs/src/pipeline.rs`   | `SourceOperator`, `build_operator()`, `Pipeline` napi class    |
| `zero-ivm-rs/src/filter.rs`     | `RustFilterPredicate` napi class, `evaluate()`, `like_match()` |
| `zero-ivm-rs/src/filter_op.rs`  | `FilterOperator` (pipeline operator)                           |
| `zero-ivm-rs/src/join_op.rs`    | `JoinOperator` (sequential, for push path)                     |
| `zero-ivm-rs/src/exists_op.rs`  | `ExistsOperator` (with parent_sizes state)                     |
| `zero-ivm-rs/src/take_op.rs`    | `TakeOperator` (with HashMap state)                            |
| `zero-ivm-rs/src/take_state.rs` | `RustTakeState` napi class (TS Take's state manager)           |
| `zero-ivm-rs/src/skip_op.rs`    | `SkipOperator` (cursor-based pagination)                       |
| `zero-ivm-rs/src/cap_op.rs`     | `CapOperator` (PK-tracked limit)                               |
| `zero-ivm-rs/src/storage.rs`    | `RustStorage` napi class (HashMap key-value store)             |
