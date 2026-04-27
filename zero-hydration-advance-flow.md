# Zero: Hydration & Advance Flow

## Overview

Zero's query pipeline has two phases:

|               | **Hydration**                                    | **Advance**                       |
| ------------- | ------------------------------------------------ | --------------------------------- |
| **Purpose**   | Build initial query result + operator state      | Propagate row diffs incrementally |
| **Model**     | Pull-based (`fetch()`)                           | Push-based (`push()`)             |
| **TS path**   | TS operator tree `fetch()`                       | TS operator tree `push()`         |
| **Rust path** | Separate Rust operator tree (parallel via Rayon) | Stateless filter-only fan-out     |

Both hydration paths use real IVM operators — hydration is NOT a raw SQL bypass.

---

## Architecture Diagram

```
                          ┌──────────────────────────────────────────────────┐
                          │              PipelineDriver                      │
                          │  addQuery() / addQueries()   advance()          │
                          └──────────┬──────────────────────┬───────────────┘
                                     │                      │
                    ┌────────────────┴────────────┐         │
                    │  Hydration Decision          │         │
                    │  (Rust or TS?)               │         │
                    │                              │         │
                    │  Rust if:                    │         │
                    │   - bindings loaded           │         │
                    │   - no CSQ in WHERE           │         │
                    │   - env not disabled          │         │
                    └───────┬──────────┬───────────┘         │
                            │          │                     │
                     ┌──────┴──┐  ┌────┴─────┐        ┌─────┴──────┐
                     │ Rust    │  │ TS       │        │ Advance    │
                     │ Hydrate │  │ Hydrate  │        │ (push)     │
                     └────┬────┘  └────┬─────┘        └─────┬──────┘
                          │            │                     │
                          ▼            ▼                     ▼
```

---

## 1. TS Hydration Path

### Flow

```
addQuery()
  │
  ├─ buildPipeline(resolvedQuery)  →  constructs TS operator tree
  │
  └─ hydrateInternal(input, ...)
       │
       └─ input.fetch({})          →  PULL through entire tree
            │
            ▼
     ┌─────────────┐
     │    Take      │  ◄── top of tree; fetch() counts up to limit,
     │  fetch()     │      stores {size, bound} in storage
     └──────┬──────┘
            │ pulls from
            ▼
     ┌─────────────┐
     │   Exists     │  ◄── FilterOperator; checks child relationship
     │  filter()    │      counts, caches by join key
     └──────┬──────┘
            │ pulls from
            ▼
     ┌─────────────┐
     │    Join      │  ◄── for each parent row, fetches matching
     │  fetch()     │      children via child.fetch({constraint})
     └──────┬──────┘
            │ pulls from
            ▼
     ┌─────────────┐
     │   Filter     │  ◄── FilterStart → Filter → FilterEnd chain;
     │  fetch()     │      runs predicate, passes matching rows
     └──────┬──────┘
            │ pulls from
            ▼
     ┌─────────────┐
     │ TableSource  │  ◄── builds SQL SELECT, executes against
     │  fetch()     │      SQLite, streams Row objects
     └─────────────┘
```

### Key Code Locations

| Component              | File                                  | Lines     |
| ---------------------- | ------------------------------------- | --------- |
| `hydrateInternal()`    | `pipeline-driver.ts`                  | 2237-2250 |
| `TableSource.fetch()`  | `packages/zqlite/src/table-source.ts` | 81-120    |
| `Filter.filter()`      | `packages/zql/src/ivm/filter.ts`      | 18-57     |
| `Join.fetch()`         | `packages/zql/src/ivm/join.ts`        | 119-127   |
| `Exists.filter()`      | `packages/zql/src/ivm/exists.ts`      | 80-99     |
| `Take.fetch()`         | `packages/zql/src/ivm/take.ts`        | 93-156    |
| `Take.#initialFetch()` | `packages/zql/src/ivm/take.ts`        | 158-199   |

### What Each Operator Does During fetch()

- **TableSource**: Builds SQL query from schema + constraints, executes against SQLite, yields `Node` rows
- **Filter**: Stateless predicate evaluation; pass/reject each row
- **Join**: For each parent row, fetches children via `child.fetch({constraint: joinKey})`, attaches as `.relationships`
- **Exists**: Checks if a named relationship has children (count > 0 or == 0); caches by join key
- **Take**: Counts rows up to `limit`, records `{size, bound}` in persistent storage (critical for push correctness)

---

## 2. Rust Hydration Path

### Flow

```
addQuery()
  │
  ├─ buildPipeline(resolvedQuery)  →  constructs TS operator tree (for later push)
  │
  └─ rustHydrateFn(dbPath, queriesJson)
       │
       │  ┌─────────────────────────────────────────────────────┐
       │  │  NAPI: rust_hydrate()                               │
       │  │                                                     │
       │  │  1. Parse queries JSON → HydrateQuery[]             │
       │  │  2. ast_to_operator_configs() → Rust operator tree  │
       │  │  3. Create ConnectionPool (shared SQLite conns)     │
       │  │  4. Group by table → one RustTableSource per table  │
       │  │  5. hydrate_pipelines() via Rayon par_iter          │
       │  └─────────────────────────────────────────────────────┘
       │
       ▼  (per pipeline, in parallel via Rayon)
  ┌─────────────┐
  │ TakeOperator │  ◄── takes up to limit, stores TakeState
  │   fetch()    │
  └──────┬──────┘
         │
         ▼
  ┌──────────────────────┐
  │ ParallelExistsOperator│  ◄── batch fetches children via
  │      fetch()          │      single IN(...) query, filters
  └──────┬───────────────┘
         │
         ▼
  ┌──────────────────────┐
  │ ParallelJoinOperator  │  ◄── batch fetches ALL children
  │      fetch()          │      in one IN(...) query, groups
  └──────┬───────────────┘       by join key, attaches
         │
         ▼
  ┌─────────────────┐
  │ FilterOperator   │  ◄── predicate evaluation
  │    fetch()       │
  └──────┬──────────┘
         │
         ▼
  ┌─────────────────┐
  │ LiveTableSource  │  ◄── SQL SELECT via rusqlite
  │    fetch()       │      from ConnectionPool
  └─────────────────┘
```

### Rust Operator Key Optimizations

1. **Rayon parallelism**: Multiple pipelines hydrate concurrently across threads
2. **Batch child fetches**: `ParallelJoinOperator` and `ParallelExistsOperator` use `WHERE key IN (...)` — one SQL query for ALL children, grouped by parent key
3. **ConnectionPool**: Shared SQLite connections avoid per-query open/close overhead

### Key Code Locations

| Component                   | File                                           |
| --------------------------- | ---------------------------------------------- |
| `rust_hydrate()` NAPI entry | `packages/zqlite-rs/src/advance.rs:1197-1309`  |
| `hydrate_pipelines()`       | `packages/zqlite-rs/src/hydrate.rs:517-547`    |
| `LiveTableSource`           | `packages/zqlite-rs/src/hydrate.rs:18-80`      |
| `FilterOperator`            | `packages/zero-ivm-rs/src/filter_op.rs:29-36`  |
| `ParallelJoinOperator`      | `packages/zqlite-rs/src/hydrate.rs:292-391`    |
| `ParallelExistsOperator`    | `packages/zqlite-rs/src/hydrate.rs:394-502`    |
| `TakeOperator`              | `packages/zero-ivm-rs/src/take_op.rs:53-87`    |
| `RustTableSource` (SQLite)  | `packages/zqlite-rs/src/table_source.rs:55-97` |
| `Operator` trait            | `packages/zero-ivm-rs/src/operator.rs:1-13`    |

---

## 3. The Fixup Step (After Rust Hydration)

### The Problem

Rust hydration builds and uses its **own** operator tree. The TS operator tree (needed for `push()` during advance) never runs — so its internal state is empty.

Critical consequence: **Take's storage is empty**. Without `{size, bound}`, `Take.push()` silently drops all changes because it thinks the window is empty.

### The Solution (Two Parts)

```
After rust_hydrate() returns:
  │
  ├─ Part A: initializeTakeState(storage, size, bound)
  │    │
  │    └─ Writes {size, bound} into top-level Take's storage
  │       Same state that Take.#initialFetch() would have written
  │       (pipeline-driver.ts:2267-2279, called at 687-689)
  │
  └─ Part B: Warm-up fetch
       │
       └─ for (const node of input.fetch({})) { /* discard */ }
            │
            └─ Runs the FULL TS operator tree
               Results are thrown away
               Side effect: child Take operators in subquery
               pipelines (EXISTS/JOIN) run their #initialFetch()
               and populate their own storage
               (pipeline-driver.ts:690-701)
```

### Why Two Parts?

- **Part A** handles the top-level Take (we know its size/bound from Rust results)
- **Part B** handles nested Takes inside EXISTS/JOIN subqueries — we can't easily extract their state from Rust, so we let the TS tree run to initialize them naturally

---

## 4. Advance (Push) Flow

### Decision Tree

```
advance() called with new snapshot
  │
  ├─ rustDispatchPokeFn available + useRustAdvance?
  │    └─ YES → #rustDispatchAdvance()     (cross-ViewSyncer batch, Rust)
  │
  ├─ useRustAdvance?
  │    └─ YES → #rustAdvance()             (Rust fan-out, single VS)
  │
  └─ else → TS #advance()                 (full TS push path)
```

`useRustAdvance` requires: all pipelines filter-only (no join/exists/take in Rust advance yet), no companion pipelines, no multi-unique-key tables.

### TS Advance (Push) Path

```
#advance(diff, timer, changes)
  │
  │  For each changed row in the snapshot diff:
  │    - Classify as Add / Edit / Remove
  │    - Push into affected TableSource(s)
  │
  └─ TableSource.push(change)
       │
       ▼ propagates through each connected pipeline
  ┌─────────────┐
  │   Filter     │  ◄── push(): checks predicate on old+new row;
  │   push()     │      may convert edit→add/remove if filter
  └──────┬──────┘       status changed
         │
         ▼
  ┌─────────────┐
  │   Exists     │  ◄── push(): for CHILD changes, checks if
  │   push()     │      relationship count crossed 0↔1 boundary;
  └──────┬──────┘       converts to add/remove on parent
         │
         ▼
  ┌─────────────┐
  │    Join      │  ◄── push(): parent change → wrap with fetched
  │   push()     │      children; child change → wrap as CHILD
  └──────┬──────┘       change on matching parent rows
         │
         ▼
  ┌─────────────┐
  │    Take      │  ◄── push(): complex window maintenance;
  │   push()     │      checks if row is within current {bound};
  └──────┬──────┘       may fetch replacement rows, emit
         │              compensating adds/removes
         ▼
  ┌─────────────┐
  │  Output CB   │  ◄── streamer.accumulate(queryID, schema, changes)
  │              │      → RowChange[] sent to client
  └─────────────┘
```

### Push Operator Behaviors

| Operator   | Push Behavior                                                                                                                      |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| **Filter** | Evaluates predicate on old/new row. Edit where filter status changes → converted to add or remove                                  |
| **Exists** | Child add/remove: recounts relationship. If count crosses 0↔1, emits add/remove on parent                                          |
| **Join**   | Parent change: attaches fetched children. Child change: wraps as CHILD change on matching parents                                  |
| **Take**   | Checks if change is within window (row <= bound). May fetch next/prev rows to maintain `size <= limit`. Emits compensating changes |

### Rust Advance Accelerators

Even in TS push mode, Rust accelerates hot paths:

```
TS Push Path with Rust Wrappers
  │
  ├─ rust-exists.ts  → rustExistsPushBatch() NAPI
  │    Batch-computes exists actions for CHILD changes
  │
  └─ rust-join.ts    → rustBuildJoinConstraint(), rustJoinPushChildBatch()
       Accelerates constraint building and match checking
```

---

## 5. Complete Lifecycle: Query Registration Through Updates

```
┌─────────────────────────────────────────────────────────────────────┐
│                        FULL LIFECYCLE                                │
│                                                                     │
│  1. CLIENT sends query AST                                          │
│       │                                                             │
│  2. PipelineDriver.addQuery()                                       │
│       │                                                             │
│       ├─ Resolve scalar subqueries                                  │
│       ├─ buildPipeline() → TS operator tree                         │
│       ├─ Capture Take storage via createStorage callback            │
│       │                                                             │
│  3. HYDRATION (initial state)                                       │
│       │                                                             │
│       ├─ [Rust path]                                                │
│       │    ├─ rust_hydrate(dbPath, queries) → binary RowChange[]    │
│       │    ├─ initializeTakeState() → top-level Take                │
│       │    └─ warm-up fetch() → child Takes in subqueries           │
│       │                                                             │
│       └─ [TS path]                                                  │
│            └─ input.fetch({}) → pull through full tree              │
│                                                                     │
│       → Send initial RowChange[] to client                          │
│                                                                     │
│  4. ADVANCE (incremental updates)                                   │
│       │                                                             │
│       │  PostgreSQL change → new SQLite snapshot                    │
│       │                                                             │
│       ├─ Compute diff (prev snapshot vs new snapshot)               │
│       │                                                             │
│       ├─ [TS path] push changes through operator tree               │
│       │    (with optional Rust accelerators for exists/join)        │
│       │                                                             │
│       └─ [Rust path] stateless filter fan-out                       │
│            (only for filter-only pipelines currently)               │
│                                                                     │
│       → Send incremental RowChange[] to client                      │
│                                                                     │
│  5. CLIENT applies changes to Replicache local store                │
│       → UI reactively updates                                       │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 6. Rust Eligibility

### Hydration: Nearly Complete

Rust hydration handles **all query types** except one edge case:

| Query Feature                                | Rust Hydration?                |
| -------------------------------------------- | ------------------------------ |
| Filters (WHERE clauses)                      | Yes                            |
| Joins (`.related()`)                         | Yes — `ParallelJoinOperator`   |
| EXISTS/NOT EXISTS (as relationship includes) | Yes — `ParallelExistsOperator` |
| LIMIT/TAKE                                   | Yes — `TakeOperator`           |
| Correlated subqueries in WHERE               | **No** — falls back to TS      |

The sole exclusion: queries with `type === 'correlatedSubquery'` anywhere in the WHERE condition tree (`pipeline-driver.ts:1422-1428`). This is when EXISTS/NOT EXISTS appears directly as a WHERE filter (not as a `.related()` include). These can't be serialized to the Rust AST format.

In practice this is rare — most EXISTS usage is via `.related()` which becomes a join/exists operator (Rust-eligible), not a WHERE-level correlated subquery.

### Advance: Filter-Only (in production)

Rust advance is much more restricted (`pipeline-driver.ts:1430-1449`):

- ALL pipeline operators must be filter-only (no join/exists/take)
- No companion pipelines (scalar subquery monitoring)
- No tables with multiple unique keys

---

## 7. Key Insight: Why Both Trees Exist

The Rust operator tree exists purely for **hydration performance** — parallel SQLite reads, batch child fetches, and native-speed row processing. But Rust advance only supports filter-only pipelines today.

For advance (push), the TS operator tree handles the full complexity of incremental state maintenance — Take window management, Exists count tracking, Join relationship bookkeeping. These stateful operations require the operator tree to persist between pushes.

The fixup step bridges the gap: Rust hydrates fast, then TS state is backfilled so push works correctly.

```
         HYDRATION                          ADVANCE
    ┌──────────────────┐            ┌──────────────────┐
    │   Rust Tree       │            │   TS Tree         │
    │   (parallel,      │──fixup──▶ │   (stateful,      │
    │    batch, fast)   │            │    incremental)   │
    └──────────────────┘            └──────────────────┘
```
