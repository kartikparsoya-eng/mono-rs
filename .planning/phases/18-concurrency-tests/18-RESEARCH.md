# Phase 18: Concurrency Tests — Research

## Summary

`rust_advance()` is a stateless, synchronous `#[napi]` function. The only true concurrency is **Rayon `par_iter` over pipelines within a single call** (line 488-491). Each pipeline is processed independently against a shared `Arc<Vec<Change>>` — read-only. There is zero mutable shared state. This makes the concurrency model inherently safe by construction, so tests focus on proving **determinism** (same inputs → identical outputs regardless of thread scheduling) and **pipeline mutation between calls** (TS-level concern).

## Rust-Level Concurrency (advance.rs)

### How Rayon Is Used

```rust
// advance.rs:487-491
let changes_arc = Arc::new(changes);
let row_changes: Vec<RowChange> = pipelines
    .par_iter()
    .flat_map(|pipeline| process_pipeline(pipeline, &changes_arc))
    .collect();
```

- `pipelines` is a `Vec<PipelineConfig>` — owned, immutable during iteration
- `changes_arc` is `Arc<Vec<Change>>` — shared read-only reference across threads
- `process_pipeline()` takes `&PipelineConfig` and `&[Change]` — pure function, no mutation
- Each Rayon thread produces an independent `Vec<RowChange>`, collected by `flat_map().collect()`

### Shared State Analysis

| State                               | Access                       | Thread Safety                                                    |
| ----------------------------------- | ---------------------------- | ---------------------------------------------------------------- |
| `changes_arc` (`Arc<Vec<Change>>`)  | Read-only                    | Safe — Arc provides shared ownership, inner Vec is never mutated |
| `pipelines` (`Vec<PipelineConfig>`) | Read-only via `par_iter()`   | Safe — Rayon borrows immutably                                   |
| `prev_conn` / `curr_conn`           | Not accessed during par_iter | Already consumed before fan-out                                  |
| No global/static state              | N/A                          | No `lazy_static`, no `thread_local`, no `static mut`             |

### What Could Go Wrong (Theoretically)

1. **Non-deterministic output ordering** — `par_iter().flat_map().collect()` does NOT guarantee order matches sequential iteration. Rayon's `collect()` preserves the index order of the original iterator, so results from pipeline[0] appear before pipeline[1]. However, within `flat_map`, the per-pipeline results maintain their order. **Key insight: Rayon's `par_iter().flat_map().collect()` actually preserves input order** — it splits work but joins results in original order. This is a documented Rayon guarantee.
2. **Allocator contention** — Many threads allocating `Vec<RowChange>` simultaneously. Not a correctness issue, but could affect timing.
3. **Predicate evaluation side effects** — `evaluate_predicate()` is purely functional (pattern match on immutable data). No risk.

### Critical Finding: Rayon Preserves Order

Rayon's `par_iter().flat_map().collect()` into a `Vec` preserves the sequential order. The `IndexedParallelIterator` trait guarantees this. This means even without sorting, results should be deterministic. However, testing this is still valuable because:

- It validates the guarantee holds for this specific usage pattern
- Future refactors might break the ordering guarantee (e.g., switching to `par_bridge()` or unordered collection)
- The 50x stability test catches any subtle issues we haven't anticipated

## Test Strategy: Rust Unit Tests

### Location

Add tests inside the existing `#[cfg(test)] mod tests` block in `advance.rs` (starts at line 509). All existing tests follow a pattern of constructing `PipelineConfig`, `Change`, and calling `process_change_for_pipeline()` or `process_pipeline()` directly.

### Determinism Test Pattern

```rust
#[test]
fn test_par_iter_determinism_many_pipelines() {
    // Create 20+ pipelines with varied filters
    let pipelines: Vec<PipelineConfig> = (0..20).map(|i| PipelineConfig {
        query_id: format!("q{}", i),
        source_tables: vec!["users".to_string()],
        operators: vec![/* varied filters per pipeline */],
        primary_key: vec!["id".to_string()],
    }).collect();

    // Create changes that hit multiple pipelines
    let changes: Vec<Change> = /* 50+ changes across different rows */;
    let changes_arc = Arc::new(changes);

    // Run once to get baseline
    let baseline: Vec<RowChange> = pipelines
        .par_iter()
        .flat_map(|p| process_pipeline(p, &changes_arc))
        .collect();

    // Run 50x, compare each to baseline
    for iteration in 0..50 {
        let result: Vec<RowChange> = pipelines
            .par_iter()
            .flat_map(|p| process_pipeline(p, &changes_arc))
            .collect();

        // Compare as sets: sort by (query_id, row_key) for order-independent comparison
        let baseline_set = sorted_by_key(&baseline);
        let result_set = sorted_by_key(&result);
        assert_eq!(baseline_set, result_set, "Mismatch at iteration {}", iteration);
    }
}
```

### Key Design Choices for Rust Tests

1. **Pipeline count**: Use 20+ pipelines. Rayon's default thread pool matches CPU cores (typically 4-16). With <4 pipelines, Rayon may not parallelize at all.
2. **Filter variety**: Mix filters (eq, gt, in, like, and/or combos) so different pipelines produce different subsets — exercises that parallel processing doesn't cross-contaminate results.
3. **Change volume**: 50+ changes with a mix of add/edit/remove to stress the fan-out. Each change should be relevant to multiple pipelines.
4. **Comparison helper**: Need `PartialEq` on `RowChange` (or derive it) for assertion. Currently `RowChange` only derives `Debug, Clone, Serialize`. Add `PartialEq` to the derive list (or compare serialized JSON).
5. **No DB needed**: `process_pipeline()` operates on in-memory `PipelineConfig` + `Change` data. No SQLite setup required for the determinism test.

### Potential Issue: PartialEq for RowChange

`RowChange` currently derives `Debug, Clone, Serialize` but NOT `PartialEq`. Options:

- **Add `#[derive(PartialEq)]`** to `RowChange` — minimal change, test-friendly
- **Compare via JSON serialization** — `serde_json::to_string(&a) == serde_json::to_string(&b)` — no production code changes but slower
- **Recommended**: Add `PartialEq` derive — it's a benign addition

## Test Strategy: Vitest Integration

### Setup Pattern (from edge-cases test)

The existing `pipeline-driver.edge-cases.test.ts` provides the exact pattern needed:

1. Create `DbFile` + in-memory `DatabaseStorage`
2. Construct `PipelineDriver` with `Snapshotter`, `shardID`, storage
3. `pipelines.init(clientSchema)` with table schemas
4. `pipelines.addQuery(hash, queryId, ast, timer)` to register pipelines
5. `replicator.processTransaction(version, ...messages)` to simulate data changes
6. `[...pipelines.advance(timer).changes]` to get results

### Concurrency Test File

`pipeline-driver.concurrency.test.ts` — new file alongside existing test files.

### 50x Stability Loop in Vitest

**Important caveat**: Each `advance()` call moves the version forward. We can't replay the exact same advance. Options:

- **Option A**: Process 50 identical insert patterns at incrementing versions, verify all produce same change shapes (query_id + table + change_type match). Row keys will differ since they're different rows.
- **Option B**: Focus the 50x loop in Rust unit tests (where we can replay identical inputs). Use vitest for pipeline mutation scenarios only.
- **Recommended**: Option B — the 50x determinism loop is more natural in Rust where we control inputs directly. Vitest tests focus on CON-02 (pipeline mutation).

### Fixture Reuse

From `pipeline-driver.fixtures.ts`:

- Reuse `numberItems` / `numberClientSchema` and `NUMBER_ITEMS_QUERY` / `NUMBER_GT_QUERY` for simple filter-only pipelines
- Create additional filter queries inline for variety (e.g., filter on `label`, range filters on `val`)
- Reuse `createSchema`, `table`, `string`, `number` builders

## Pipeline Mutation Scenarios (CON-02)

All tested at the vitest/TS level since pipeline management is a TS concern.

### Scenario 1: Add Pipeline Between Calls

```
1. init + addQuery(q1) + advance  -> baseline
2. addQuery(q2)                   -> hydration results for q2
3. advance                        -> q1 + q2 both get correct results
```

Already partially covered by `pipeline-driver.edge-cases.test.ts:199` ("adding a pipeline after initial advance"). Phase 18 should add a Rust-specific variant: ensure the **added** pipeline is filter-only (Rust-eligible) and verify it appears in Rust advance output.

### Scenario 2: Remove Pipeline Between Calls

```
1. init + addQuery(q1) + addQuery(q2) + advance
2. removeQuery(q1)
3. advance -> only q2 results, no errors, no stale q1 output
```

Already partially covered by edge-cases test (line 225). Phase 18 variant: both q1 and q2 are Rust-eligible filter pipelines.

### Scenario 3: Add + Remove in Same Interval

```
1. init + addQuery(q1, q2, q3) + advance
2. removeQuery(q1) + addQuery(q4)
3. advance -> q2, q3, q4 results only
```

New scenario — verifies the pipeline set is reconstructed fresh each cycle.

### Scenario 4: Remove All Pipelines

```
1. init + addQuery(q1) + advance
2. removeQuery(q1)
3. advance -> empty results, no errors
```

Edge case — ensures Rust path handles empty pipeline list gracefully.

### Scenario 5: Rapid Pipeline Churn

```
1. init + advance with 10 pipelines
2. Remove 5, add 5 different ones
3. advance -> only the current 10 pipelines produce output
```

Stress test for pipeline set management.

## Risks and Mitigations

| Risk                                  | Likelihood | Mitigation                                                                                                                                   |
| ------------------------------------- | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| **50x loop too slow**                 | Low        | Budget 5s total. 50 iterations of 20 pipelines x 50 changes is microseconds each in-memory. If slow, reduce to 25x.                          |
| **Rayon thread pool initialization**  | Low        | First `par_iter` call initializes the global thread pool. Subsequent calls reuse it. No issue for 50x loops.                                 |
| **Test flakiness from timing**        | Very low   | No timing dependencies — tests compare outputs, not timing. The 10s timeout is a deadlock detector only.                                     |
| **PartialEq derivation on RowChange** | None       | Adding `#[derive(PartialEq)]` is backward-compatible. Alternative: compare via JSON serialization.                                           |
| **Vitest advance state accumulation** | Medium     | Each advance moves version forward. Pipeline mutation tests must account for cumulative state. Use fresh `DbFile` per test via `beforeEach`. |
| **CI environment differences**        | Low        | Rayon adapts to available cores. Fewer cores = less parallelism but same correctness. Tests pass on 1-core too.                              |

## RESEARCH COMPLETE
