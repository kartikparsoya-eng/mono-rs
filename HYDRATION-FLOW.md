# Zero-Cache Hydration Flow

How data gets from SQLite to the client when they first connect.

---

## Layman's Explanation

Imagine a restaurant. A customer (client) sits down and orders 5 dishes (ZQL queries). Here's what happens:

1. **The host seats them** — the WebSocket connection is established. The customer hands the waiter a list of 5 dishes.

2. **The waiter gets the kitchen lock** — only one waiter can be in the kitchen at a time for this table (the `#lock` mutex). Other waiters for the same table wait in line.

3. **The waiter checks the menu** — the customer's order gets checked against permissions. "Are they allowed to order the secret menu item?" Each query gets rewritten with security rules baked in (e.g., `WHERE org_id = 'their-org'`).

4. **The waiter asks the kitchen, one dish at a time** — the waiter goes to the kitchen (SQLite) and says "give me everything for dish 1." The kitchen runs one big query, dumps ALL matching rows into a tray, hands it back. Then dish 2. Then dish 3. **Sequentially. No parallelism.**

5. **The kitchen has one window** — even if you had 5 waiters, the kitchen (SQLite connection) only has one serving window (`RefCell` — single borrow at a time). One query at a time.

6. **The waiter deduplicates** — if dish 1 and dish 3 both include the same row (e.g., same message appears in two query results), the waiter only sends it once. This is why dishes are prepared sequentially — to maximize dedup.

7. **The waiter carries trays to the table** — every 10,000 rows, the waiter delivers a batch to the customer. Each delivery is a `pokePart` message over the WebSocket.

8. **The waiter takes breaks** — after working for a few milliseconds, the waiter yields (`setImmediate`) so other tables (other ViewSyncers) can be served. There's a **global queue** — only one waiter works at a time across ALL tables.

9. **"Enjoy your meal"** — after all 5 dishes are served, the waiter sends a `pokeEnd` with a cookie (version stamp). The customer now has their initial data.

**Key takeaway**: Everything is sequential. One query at a time, one row batch at a time, one ViewSyncer at a time (due to global time-slice queue). The only parallelism is across different Node.js processes.

---

## Technical Flow

### Architecture Diagram

```
Client (WebSocket)
  |
  v
initConnection()                    view-syncer.ts:742
  |
  v
#runInLockForClient()               view-syncer.ts:1051
  |
  v
#runInLockWithCVR()                 view-syncer.ts:371
  |  - Acquires @rocicorp/lock mutex (one op per ViewSyncer at a time)
  |  - Lazy-loads CVR from Postgres on first call
  |
  v
#handleConfigUpdate()               view-syncer.ts:1142
  |  - Applies desiredQueriesPatch (put/del/clear ops)
  |  - Writes config changes to CVR in Postgres
  |
  v
#syncQueryPipelineSet()             view-syncer.ts:1572
  |  - Transforms each query with auth/permissions (row-level security)
  |  - Determines which queries to add/remove (by transformation hash)
  |
  v
#addAndRemoveQueries()              view-syncer.ts:1843
  |  - Creates CVRQueryDrivenUpdater for Postgres row tracking
  |  - Creates PokeHandler for all connected clients
  |  - SEQUENTIAL LOOP over queries (generator pattern)
  |
  v
PipelineDriver.*addQuery()          pipeline-driver.ts:492
  |  - Resolves scalar subqueries (each is a mini-pipeline + SQLite query)
  |  - buildPipeline() — creates IVM operator tree (Filter, Join, Take, etc.)
  |  - *hydrateInternal() — calls input.fetch({})
  |
  v
TableSource.*#fetch()               table-source.ts:337
  |  - Builds SQL via buildSelectQuery()
  |  - Calls #queryAllTyped() — single synchronous FFI call
  |  - Merges with overlay (for self-join correctness during push)
  |  - Wraps in generateWithYields() — cooperative time-slicing
  |
  v
Database.query_all()                database.rs:255   (Rust / FFI)
  |  - conn.prepare_cached(&sql) — statement cache
  |  - Iterates ALL rows, converts each to JS object
  |  - Returns JS array of all results in one shot
  |
  v
#processChanges()                   view-syncer.ts:2077
  |  - Accumulates RowChanges in CustomKeyMap (dedup by RowID)
  |  - Every 10,000 rows: flush to CVR in Postgres + send pokePart
  |
  v
ClientHandler.addPatch()            client-handler.ts:219
  |  - Batches row patches into pokePart messages
  |  - Flushes every 100 patches
  |
  v
WebSocket: pokeStart -> N x pokePart -> pokeEnd
```

---

### File-by-File Breakdown

#### 1. view-syncer.ts — Orchestration

| Function                  | Line | Role                                                                                                                     |
| ------------------------- | ---- | ------------------------------------------------------------------------------------------------------------------------ |
| `initConnection()`        | 742  | Entry point. Creates ClientHandler + Subscription, kicks off async hydration                                             |
| `#runInLockForClient()`   | 1051 | Wraps work in the ViewSyncer lock, validates wsID freshness                                                              |
| `#runInLockWithCVR()`     | 371  | **THE LOCK**. Acquires `@rocicorp/lock` mutex. Lazy-loads CVR from Postgres. All hydration + advancement serialized here |
| `#handleConfigUpdate()`   | 1142 | Applies client's desired query patches to CVR                                                                            |
| `#updateCVRConfig()`      | 996  | Flushes config to Postgres, triggers `#syncQueryPipelineSet` if pipelines are synced                                     |
| `#syncQueryPipelineSet()` | 1572 | Transforms queries with permissions, determines add/remove sets                                                          |
| `#addAndRemoveQueries()`  | 1843 | **THE SEQUENTIAL LOOP**. Generator that hydrates each query one at a time                                                |
| `#processChanges()`       | 2077 | Consumes RowChange iterable, deduplicates, batches to CVR + clients                                                      |
| `yieldProcess()`          | 2437 | Global time-slice yield: `timeSliceQueue.withLock(() => new Promise(setImmediate))`                                      |

**Serialization points:**

- `#lock` (line 237): Per-ViewSyncer mutex. All clients in the same client group share this.
- `timeSliceQueue` (line 2435): **Process-wide** mutex. Only one IVM time slice runs per event loop tick.

#### 2. pipeline-driver.ts — Pipeline Construction + Hydration

| Function                     | Line | Role                                                                      |
| ---------------------------- | ---- | ------------------------------------------------------------------------- |
| `*addQuery()`                | 492  | Generator. Resolves scalar subqueries, builds pipeline, hydrates          |
| `#resolveScalarSubqueries()` | 426  | For queries with subqueries: builds mini-pipelines, fetches scalar values |
| `buildPipeline()`            | 528  | Creates IVM operator tree from AST. Delegate provides sources + storage   |
| `*hydrateInternal()`         | 1270 | Calls `input.fetch({})` — triggers SQLite query through the pipeline      |
| `#getSource()`               | 994  | Gets/creates TableSource per table. All share the same DB connection      |
| `#shouldYield()`             | 1018 | Checks elapsed time, returns true if should yield to event loop           |

**Key detail**: `buildPipeline()` takes a delegate (line 528-566) that controls:

- `getSource` — which TableSource to use (cached per table name)
- `createStorage` — Rust (`RustTakeStorage`) or JS (`MemoryStorage`) for operator state
- `decorateFilterInput` — may wrap with Rust-accelerated exists operators

#### 3. table-source.ts — SQLite Query Execution

| Function                | Line       | Role                                                                    |
| ----------------------- | ---------- | ----------------------------------------------------------------------- |
| `constructor()`         | 102        | Takes DB reference, table name, schema spec, shouldYield callback       |
| `setDB()`               | 141        | Sets the SQLite Database reference (called when snapshot changes)       |
| `*#fetch()`             | 337        | Builds SQL, executes query, merges overlay, wraps with yield checks     |
| `#queryAllTyped()`      | 156        | Calls `this.#db.queryAll()` — the Rust FFI boundary                     |
| `*genPush()`            | 490        | For advancement: pushes changes through connected pipelines             |
| `generateWithOverlay()` | (imported) | Merges in-memory overlay with DB results for self-join correctness      |
| `generateWithYields()`  | 764        | After each row, checks `shouldYield()`. Yields `'yield'` string if true |

**The overlay system**: During IVM push (advancement), a change to table X needs to be pushed through pipelines that may JOIN back to table X. But the change isn't written to SQLite yet. The overlay holds the pending change in memory so re-fetches see it. Set before push, cleared after.

#### 4. database.rs — Rust FFI (SQLite)

| Function                   | Line          | Role                                                                         |
| -------------------------- | ------------- | ---------------------------------------------------------------------------- |
| `Database::new()`          | 37            | Opens SQLite connection. Sets `busy_timeout(5000ms)`                         |
| `query_all()`              | 255           | **THE QUERY**. `prepare_cached()` + iterate all rows + convert to JS objects |
| `prepare()`                | 107           | Prepares SQL statement. Intercepts `BEGIN CONCURRENT` -> `BEGIN IMMEDIATE`   |
| `close()`                  | 74            | Actually closes the SQLite connection (was a no-op before our fix)           |
| `row_to_typed_js_object()` | (in types.rs) | Converts SQLite row to napi JS object with type coercion                     |

**Connection model**: `conn: Arc<RefCell<Connection>>` — single connection, single-threaded borrow. All TableSource instances within a ViewSyncer share one connection through one snapshot.

#### 5. snapshotter.ts — SQLite Connection Lifecycle

| Function                  | Line | Role                                                                                      |
| ------------------------- | ---- | ----------------------------------------------------------------------------------------- |
| `Snapshotter.init()`      | 116  | Creates first Snapshot (opens DB, begins transaction)                                     |
| `Snapshot.create()`       | 276  | Opens new Database, sets `synchronous=OFF`, starts `BEGIN CONCURRENT`, reads stateVersion |
| `Snapshotter.advance()`   | 175  | Leapfrog: resets prev snapshot to HEAD, swaps prev/curr, returns Diff                     |
| `Diff[Symbol.iterator]()` | 424  | Queries `_zero.changeLog2` for changes between snapshots                                  |

**Leapfrog pattern**: Two connections alternate. `conn_1` holds snapshot at `t1`, `conn_2` advances to `t2`. Changes between `t1->t2` are read from `conn_2`'s changeLog. Changes are pushed through `conn_1`'s IVM pipelines (but never committed — rolled back). Then `conn_1` resets to HEAD for the next advance.

#### 6. client-handler.ts — Wire Protocol

| Function                    | Line | Role                                                          |
| --------------------------- | ---- | ------------------------------------------------------------- |
| `startPoke()` (free fn)     | 85   | Creates PokeHandler that fans out to all ClientHandlers       |
| `ClientHandler.startPoke()` | 184  | Returns closure-based PokeHandler for one client              |
| `addPatch()`                | 219  | Accumulates row/query/mutation patches into pokePart messages |
| `end()`                     | 326  | Sends `pokeEnd` with cookie. Client commits the poke          |

**Wire format**: `pokeStart{pokeID, baseCookie}` -> N x `pokePart{rowsPatch, gotQueriesPatch, ...}` -> `pokeEnd{pokeID, cookie}`

Parts are flushed every 100 patches (PART_COUNT_FLUSH_THRESHOLD).

---

### The CVR (Client View Record)

Stored in **Postgres** (not SQLite). Tracks per client group:

| What                    | Why                                                             |
| ----------------------- | --------------------------------------------------------------- |
| Desired queries         | What the client wants to subscribe to                           |
| Got queries             | What the server has successfully hydrated                       |
| Row references          | Which rows are referenced by which queries (refcount)           |
| Row contents + versions | For catching up reconnecting clients without re-querying SQLite |
| CVR version             | `{stateVersion, configVersion}` — advances with each change     |

Two updater types:

- **CVRConfigDrivenUpdater** (line 1004): For config changes — adding/removing queries
- **CVRQueryDrivenUpdater** (line 1860): For data changes — hydration results, row patches

Both flushed via `#flushUpdater()` (line 941) which writes to Postgres.

---

### Why It's All Sequential (Summary)

| Constraint               | Location                                  | Reason                                                                               |
| ------------------------ | ----------------------------------------- | ------------------------------------------------------------------------------------ |
| Per-ViewSyncer lock      | `#lock` at line 237                       | CVR consistency — can't have two ops modifying CVR state concurrently                |
| Sequential query loop    | `#addAndRemoveQueries` line 1902          | Row deduplication — wrapping all queries in one generator maximizes dedup            |
| Single SQLite connection | `Arc<RefCell<Connection>>` in database.rs | One borrow at a time. All TableSources share one snapshot connection                 |
| Global time-slice queue  | `timeSliceQueue` at line 2435             | Prevents one ViewSyncer from starving others. Only one IVM slice per event loop tick |

### Where Parallelization Could Help

1. **Multi-query hydration**: Open N read-only SQLite connections at the same WAL snapshot. Execute all N hydration queries via Rayon in parallel. Dedup after all results are in (hash set merge is cheap vs N serial SQLite queries).

2. **Cross-ViewSyncer poke processing**: When a Postgres change arrives, ALL affected ViewSyncers process it sequentially (plus the global timeSliceQueue serializes yields). Moving "which pipelines care + compute diffs" into one Rust call with Rayon would remove the biggest scaling bottleneck.

3. **Patch encoding**: After computing diffs, serializing rows into CVR patches + wire format is CPU-bound and embarrassingly parallel.

4. **Eliminating timeSliceQueue**: Moving CPU work to Rust threads removes the need for cooperative yielding entirely. Node event loop stays free for I/O.
