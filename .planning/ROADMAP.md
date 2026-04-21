# Roadmap — v4.0 Parallel IVM Runtime

## Mandatory Verification Gate (All Phases)

Every phase MUST pass these checks before commit:

1. `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — 28 pass, 2 pre-existing failures
2. `ZERO_DUAL_EXEC=strict npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — dual-execution shadow mode (TS vs Rust) with no mismatches
3. `npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — property-based fuzz (1k iterations default)
4. `FUZZ_NUM_RUNS=10000 npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — extended fuzz (run periodically, mandatory before merge)
5. All v3.0 integration tests (not-exists, join-topology, json, null, unicode, numbers, concurrency, edit-semantics)
6. `cargo test` in both `packages/zqlite-rs/` and `packages/zero-ivm-rs/`

As Rust operators expand beyond filter-only, update `fuzz-ivm.test.ts` to cover new operator types (Join, Take, Exists).

---

## Phase 19.5: Dual-Execution Correctness Harness (INSERTED) ✅ COMPLETE

**Goal:** Build a dual-execution comparator and property-based fuzz harness to ensure Rust IVM correctness throughout v4.0 development.

**Status:** Complete (commit 84504f36b)

**Deliverables:**

1. `dual-executor.ts` — shadow mode runs TS + Rust, compares results (`ZERO_DUAL_EXEC=strict|1|log`)
2. `fuzz-ivm.test.ts` — fast-check property tests (10k iterations pass)
3. Pipeline-driver integration — `#dualExecAdvance()` wired into advance path

---

## Phase 20: Rust Operator Trait & Pipeline Builder

**Goal:** Port the IVM operator tree to Rust — Filter, Join, Take, Exists, Skip, Cap as a unified trait with `fetch()` and `push()` methods. Build a pipeline builder that constructs operator trees from ZQL ASTs.

**Requirements:** OPR-01, OPR-02, OPR-03

**Success Criteria:**

1. `Operator` trait with `fetch(FetchRequest) -> Stream<Node>` and `push(Change) -> Stream<Change>` in Rust
2. Filter operator: evaluates WHERE predicates, pushes filters down to source when possible
3. Join operator: `fetch()` attaches lazy child closures, `push()` propagates changes through parent/child relationship
4. Take operator: `fetch()` stops after limit rows (initial + bound-based), `push()` maintains sorted state
5. Exists operator: filter based on child relationship existence/non-existence
6. Skip operator: modifies start bound, delegates to input
7. Cap operator: limit enforcement for correlated subqueries
8. All operators pass existing vitest pipeline-driver tests (no TS test changes)

---

## Phase 21: Rust TableSource with Multi-Connection Pool

**Goal:** Port TableSource to Rust with a connection pool that enables parallel SQLite reads. Each fetch() opens a read-only connection from the pool, all at the same WAL snapshot.

**Requirements:** SRC-01, SRC-02, SRC-03

**Success Criteria:**

1. `RustTableSource` wraps a connection pool (N read-only SQLite connections at same WAL snapshot)
2. `connect()` accepts sort ordering and filter pushdown, builds SQL query
3. `fetch()` executes SQL via pool connection, streams rows through operator tree
4. Overlay system ported — pending changes visible to re-fetches during push (self-join correctness)
5. `genPush()` fans out changes to connected pipelines with overlay management
6. Connection pool correctly handles WAL snapshot pinning (all connections see same data)
7. Existing table-source.test.ts tests pass via napi bridge

---

## Phase 22: Parallel Multi-Pipeline Hydration

**Goal:** When a client subscribes to N queries, hydrate all N pipelines in parallel using Rayon. Each pipeline runs on a separate thread with its own SQLite connection.

**Requirements:** HYD-01, HYD-02, HYD-03

**Depends on:** Phase 20, Phase 21

**Success Criteria:**

1. `rust_hydrate(db_path, queries: Vec<AST>) -> Vec<Vec<RowChange>>` napi function
2. Each pipeline built and executed on a Rayon worker thread with its own read-only SQLite connection
3. Results returned as serialized RowChanges (not napi JS objects — Env is not Send)
4. Row deduplication happens after all results are in (hash set merge in TS or Rust)
5. Scalar subquery resolution works within each pipeline thread
6. Wall-clock hydration time for N queries approaches `max(single query)` not `sum(all queries)`
7. TS `addQuery()` loop in `#addAndRemoveQueries()` replaced with single Rust call + result distribution
8. Benchmark: 5-query hydration at least 3x faster than sequential baseline

---

## Phase 23: Within-Pipeline Child Parallelism (Join Fan-Out)

**Goal:** During hydration, when a Join operator needs to fetch child data for N parent rows across M relationships, fire independent child queries in parallel using separate connections from the pool.

**Requirements:** JFO-01, JFO-02

**Depends on:** Phase 22

**Success Criteria:**

1. Join.fetch() fires M child relationship fetches per parent row in parallel via Rayon
2. Each child fetch uses its own connection from the pool
3. Child results are collected and attached to parent node before yielding
4. Correct ordering maintained (parent row order preserved, child results per-relationship)
5. Benchmark: query with 100 rows x 3 relationships executes child queries ~3x faster

---

## Phase 24: Parallel Advance (Cross-Pipeline Push Propagation)

**Goal:** When a Postgres change arrives and affects table X, push the change through all connected pipelines in parallel. This extends the existing `rust_fan_out()` to handle the full operator tree, not just filter-only evaluation.

**Requirements:** ADV-01, ADV-02, ADV-03

**Depends on:** Phase 20, Phase 21

**Success Criteria:**

1. `rust_advance_full(db_path, changes, pipelines) -> Vec<(pipeline_id, Vec<RowChange>)>` napi function
2. Each pipeline evaluates the change through its full operator tree (Filter -> Join -> Take -> Exists) on a Rayon thread
3. Overlay system works correctly per-thread (self-join re-fetches see pending changes)
4. Pipeline state (Take bounds, Exists cache) correctly maintained across pushes
5. Replaces both `rust_fan_out()` and the TS genPush() path
6. ResetPipelinesSignal equivalent: returns Err variant that TS interprets as reset
7. Existing pipeline-driver advance tests pass unchanged

---

## Phase 25: Serialization Format & FFI Optimization

**Goal:** Design an efficient binary serialization format for passing results between Rust and TS. Eliminate the overhead of creating napi JS objects on the main thread for large result sets.

**Requirements:** SER-01, SER-02

**Depends on:** Phase 22, Phase 24

**Success Criteria:**

1. Binary row format (column-oriented or row-oriented) for Rust -> TS results
2. TS decoder that produces JS objects from binary buffer (similar to existing `decodeBuf()` in snapshotter)
3. Benchmark: encoding + decoding overhead < 10% of total hydration time
4. Format supports all SQLite types: null, integer, float, text, blob, JSON
5. Optional: streaming decode (process rows as they arrive, not all-at-once)

---

## Phase 26: Pipeline-Driver TS Integration

**Goal:** Modify pipeline-driver.ts and view-syncer.ts to use the new Rust hydration and advance paths. These are deletion-heavy changes — removing JS pipeline logic that Rust now owns.

**Requirements:** INT-01, INT-02, INT-03

**Depends on:** Phase 22, Phase 24, Phase 25

**Success Criteria:**

1. `pipeline-driver.ts` `addQuery()` calls `rust_hydrate()` instead of `buildPipeline()` + `input.fetch()`
2. `pipeline-driver.ts` `#advance()` calls `rust_advance_full()` instead of `TableSource.genPush()`
3. `view-syncer.ts` `#addAndRemoveQueries()` simplified — no generator gymnastics, receives pre-computed RowChanges
4. `table-source.ts` becomes thin wrapper (or removed) — Rust owns SQLite reads
5. All existing view-syncer and pipeline-driver tests pass unchanged
6. Feature flag: `ZERO_DISABLE_RUST_HYDRATION` to fall back to TS path
7. E2E test against xyne-spaces passes (same as v3.0 baseline)

---

## Phase 27: Cross-ViewSyncer Poke Dispatch

**Goal:** When a Postgres change arrives, process ALL affected ViewSyncer instances in parallel in a single Rust call, instead of each ViewSyncer processing sequentially on the Node event loop.

**Requirements:** XVS-01, XVS-02

**Depends on:** Phase 24, Phase 26

**Success Criteria:**

1. `rust_dispatch_poke(change, viewsyncer_pipelines: Vec<(vs_id, pipelines)>) -> Vec<(vs_id, Vec<RowChange>)>` napi function
2. All ViewSyncer pipeline evaluations run in parallel via Rayon
3. TS `run()` loop's `version-ready` handler calls one Rust function, distributes results to CVR + poke per ViewSyncer
4. Global `timeSliceQueue` becomes unnecessary for advance path (CPU work is off main thread)
5. Benchmark: 50 concurrent ViewSyncers process a poke at least 5x faster than sequential baseline
6. Cooperative yielding still works for hydration path (or becomes unnecessary with Rust parallelism)

---

## Phase 28: E2E Validation & Benchmark Suite

**Goal:** Comprehensive end-to-end testing and performance benchmarking of the full Rust IVM runtime against the TS baseline.

**Requirements:** E2E-01, E2E-02, BEN-01

**Depends on:** Phase 26, Phase 27

**Success Criteria:**

1. All existing vitest suites pass with Rust IVM enabled
2. Docker E2E test against xyne-spaces: identical pass/fail pattern as stock zero
3. Hydration benchmark: N-query initial load latency vs sequential TS baseline
4. Advance benchmark: per-change processing time under M concurrent ViewSyncers
5. Memory benchmark: RSS under sustained load (no Rust-side memory leaks)
6. Stress test: 100+ concurrent clients, sustained changes, no panics or deadlocks
7. Results documented with before/after comparisons

---

## Summary

| Phase | Name                                   | Requirements           | Depends On | Criteria   |
| ----- | -------------------------------------- | ---------------------- | ---------- | ---------- |
| 20    | Rust Operator Trait & Pipeline Builder | 4/4                    | Complete   | 2026-04-21 |
| 21    | Rust TableSource + Connection Pool     | SRC-01, SRC-02, SRC-03 | —          | 7          |
| 22    | Parallel Multi-Pipeline Hydration      | HYD-01, HYD-02, HYD-03 | 20, 21     | 8          |
| 23    | Within-Pipeline Child Parallelism      | JFO-01, JFO-02         | 22         | 5          |
| 24    | Parallel Advance (Full Operator Tree)  | ADV-01, ADV-02, ADV-03 | 20, 21     | 7          |
| 25    | Serialization Format & FFI             | SER-01, SER-02         | 22, 24     | 5          |
| 26    | Pipeline-Driver TS Integration         | INT-01, INT-02, INT-03 | 22, 24, 25 | 7          |
| 27    | Cross-ViewSyncer Poke Dispatch         | XVS-01, XVS-02         | 24, 26     | 6          |
| 28    | E2E Validation & Benchmarks            | E2E-01, E2E-02, BEN-01 | 26, 27     | 7          |

**9 phases** | **22 requirements** | Dependency graph has two parallel tracks (hydration: 20→21→22→23, advance: 20→21→24) converging at Phase 26
