# Phase 20: Research — Rust Operator Trait & Pipeline Builder

## Executive Summary

Phase 20 ports the full IVM operator tree to Rust as a unified `Operator` trait with `fetch()` and `push()` methods, covering all 6 operators (Filter, Join, Take, Exists, Skip, Cap) plus a pipeline builder that constructs operator trees from TS-extracted JSON config. Significant Rust code already exists from v2.0/v3.0 milestones (filter evaluator, join matcher, exists helper, take state manager) — these become internal helpers wrapped inside new Operator trait implementations. The dual-execution harness from Phase 19.5 provides the correctness validation backbone.

## Current TypeScript Implementation

### Operator Trait/Interface

**File:** `packages/zql/src/ivm/operator.ts`

The TS operator model uses two paired interfaces:

- **`Input`** — has `fetch(req: FetchRequest) -> Stream<Node>` and cleanup/schema methods
- **`Output`** — has `push(change: Change, input: Input)` to receive changes from upstream
- **`Operator`** — combines both: an operator is an Output to its upstream and an Input to its downstream

Key types:

- **`FetchRequest`** = `{constraint?: Constraint, start?: {row: Row, basis: 'at'|'after'}, reverse?: boolean}`
- **`Node`** = `{row: Row, relationships: Record<string, Stream<Node>>}` — relationships are lazy streams
- **`Change`** = AddChange(type=0) | RemoveChange(type=1) | ChildChange(type=2) | EditChange(type=3)
- **`Stream<T>`** = `Iterable<T>` — lazy forward-only iterables
- **`SourceSchema`** = `{tableName, columns, primaryKey, sort, comparator, relationships, system, isHidden}`

**Rust mapping (per D-02):** `fetch()` returns `Vec<Node>`, `push()` returns `Vec<Change>`. No lazy streams — results fully materialized before FFI boundary.

### Filter Operator

**File:** `packages/zql/src/ivm/filter.ts` (~80 lines)

- Delegates evaluation to a `FilterOperator` interface (3 variants in `filter-operators.ts`: simple comparison, compound AND/OR, custom function)
- `fetch()`: Passes constraint to input's fetch, then filters resulting nodes via `filterNode()`
- Attempts **constraint pushdown** — if the filter is a simple equality on a column, it can be pushed into the FetchRequest constraint
- `push()`: Delegates to `filterPush()` in `filter-push.ts` which evaluates the predicate on the change's row and only propagates matching changes
- For EditChange: checks both old and new row — may convert to Add (old didn't match, new does) or Remove (old matched, new doesn't)

**Rust asset:** `packages/zero-ivm-rs/src/filter.rs` — `evaluate_filter()` function with 15 comparison operators, `Value` enum, JSON handling. 139 cargo tests cover this. Becomes `FilterOperator`'s internal evaluator.

### Join Operator

**File:** `packages/zql/src/ivm/join.ts` (~500 lines) + `flipped-join.ts` (~400 lines)

Most complex operator. Manages parent-child relationships.

- **`fetch()`**: Iterates parent nodes from input, attaches lazy child relationship closures. Each child relationship becomes a `Stream<Node>` fetched from a child source with a constraint derived from the parent row's join key columns.
- **`push()`**: Two paths:
  - **Parent change**: Re-fetches children for the changed parent row, propagates as combined change
  - **Child change**: Uses `flipped-join.ts` logic — finds affected parent rows via sorted array + binary search, propagates as ChildChange
- Join key matching: Compares parent columns to child columns based on `parentKey`/`childKey` configuration
- Overlay system: During push, uses a HashMap overlay to handle self-join correctness (reading own writes)

**Rust asset:** `packages/zero-ivm-rs/src/join.rs` — `parent_matches_child()` helper, `build_join_constraint()`. Becomes `JoinOperator`'s push-side helper.

**Complexity notes:**

- The child relationship closures in TS are lazy — in Rust (per D-02) these will be eagerly materialized as `Vec<Node>`
- The flipped-join push path involves sorted array state + binary search, which maps well to Rust's `Vec` + `binary_search_by`
- Overlay/self-join correctness is critical for advance path (Phase 24) but must be structurally supported here

### Take Operator

**File:** `packages/zql/src/ivm/take.ts` (~500 lines)

Enforces LIMIT semantics with stateful bound tracking.

- Maintains per-correlation-key `TakeState = {size: number, bound: Row | undefined}`
- **`fetch()`**: Fetches from input, stops after `limit` rows using bound-based optimization. If a bound exists, uses it as `start` in the FetchRequest to avoid re-scanning
- **`push()`**: Maintains sorted state. When a new row is added:
  - If within the limit window, emit AddChange
  - If it displaces a row at the boundary, emit RemoveChange for displaced + AddChange for new
  - Updates bound accordingly
- Uses `Storage` interface for persistent state across push cycles

**Rust asset:** `packages/zero-ivm-rs/src/take_state.rs` — `TakeStateManager` napi class with HashMap-based sorted state, bound tracking. Becomes `TakeOperator`'s internal state manager.

### Exists Operator

**File:** `packages/zql/src/ivm/exists.ts` (~300 lines)

Filters parent rows based on whether a child relationship has any rows (EXISTS) or no rows (NOT EXISTS).

- Tracks `parentSizes: Map<string, number>` — count of child rows per parent key
- **`fetch()`**: Fetches parent nodes, filters based on child relationship existence
- **`push()`**: On child changes:
  - AddChange on child: increment parent size. If 0->1 transition, emit AddChange for parent (EXISTS) or RemoveChange (NOT EXISTS)
  - RemoveChange on child: decrement. If 1->0 transition, emit opposite
  - Parent changes: check child existence and propagate or filter accordingly

**Rust asset:** `packages/zero-ivm-rs/src/exists.rs` — `compute_exists_changes()` batch decision logic. Becomes `ExistsOperator`'s push helper.

### Skip Operator

**File:** `packages/zql/src/ivm/skip.ts` (~250 lines)

Implements OFFSET semantics.

- **`fetch()`**: Modifies the `start` bound by skipping N rows from the input, then delegates remainder to input
- **`push()`**: Tracks position changes. When rows are added/removed before the skip boundary, adjusts which rows are visible and emits corresponding changes
- Maintains state about the current skip boundary position

**Rust note:** No existing Rust code for Skip. Per context D-decisions, can be a thin operator that modifies FetchRequest bounds and delegates. Push side needs position tracking logic.

### Cap Operator

**File:** `packages/zql/src/ivm/cap.ts` (~200 lines)

Limit enforcement for correlated subqueries — simpler than Take.

- Like Take but without per-correlation-key state complexity
- Used internally by the pipeline builder for correlated subquery limits
- Simpler state management — single bound, not per-key

**Rust note:** No existing Rust code for Cap. Can be a thin wrapper similar to Take but without correlation key complexity.

## Existing Rust Infrastructure

### Current Rust Crates

Two crates in the workspace:

1. **`packages/zero-ivm-rs/`** — Pure IVM logic, no SQLite dependency
   - `lib.rs`: napi module registration
   - `filter.rs`: Predicate evaluator (15 comparison ops, Value enum, JSON handling)
   - `join.rs`: Join hot-path helpers (isJoinMatch, buildJoinConstraint)
   - `exists.rs`: Exists batch push decisions
   - `take_state.rs`: TakeStateManager napi class
   - `storage.rs`: Generic key-value RustStorage (HashMap-based)

2. **`packages/zqlite-rs/`** — SQLite-touching code
   - `advance.rs`: Pipeline topology types (`PipelineOperator` enum with Filter variant only), `rust_fan_out()`, `process_change_for_pipeline()`
   - Currently only supports Filter in the pipeline topology — Phase 20 expands this to all operators

### Existing Operator Implementations

**These are NOT full operators** — they are helper functions exposed via napi:

| Rust file       | What it does                      | How it maps to Phase 20                        |
| --------------- | --------------------------------- | ---------------------------------------------- |
| `filter.rs`     | `evaluate_filter()` + comparisons | Becomes `FilterOperator`'s internal evaluator  |
| `join.rs`       | `parent_matches_child()`          | Becomes `JoinOperator`'s push-side row matcher |
| `exists.rs`     | `compute_exists_changes()`        | Becomes `ExistsOperator`'s push helper         |
| `take_state.rs` | `TakeStateManager` class          | Becomes `TakeOperator`'s state manager         |
| `storage.rs`    | `RustStorage` HashMap             | Becomes operator tree's intermediate storage   |

**139 cargo tests** already validate evaluation logic across these modules — per D-03, wrap don't rewrite.

### NAPI Bridge

Current pattern:

- `#[napi]` attribute on structs/functions for JS interop
- `serde_json::Value` for row data interchange
- napi classes (e.g., `RustFilterPredicate`, `RustTakeStorage`) hold Rust state, expose methods to JS
- `Env` parameter available in napi functions for JS object creation

**Phase 20 new napi surface (per context):**

- `build_pipeline(config_json: String) -> Pipeline` — returns opaque napi handle
- `pipeline.fetch(request_json: String) -> Buffer` — returns serialized results
- `pipeline.push(change_json: String) -> Buffer` — returns serialized change results

**Key constraint:** `napi::Env` is NOT `Send` — cannot use on Rayon threads. Phase 20 doesn't need parallelism, but the trait design must not depend on `Env` so Phase 22/24 can parallelize.

## Pipeline Builder Architecture

### Current TS Pipeline Construction

**File:** `packages/zql/src/builder/builder.ts` (~800 lines)

- Takes a ZQL `AST` + `BuilderDelegate` (provides `createSource()` for leaf nodes)
- Walks the AST and constructs an operator tree bottom-up
- `BuilderDelegate` interface: `createSource(tableName, columns, primaryKey, sort, relationships) -> Source`

### AST-to-Operator Mapping

| AST Node                  | TS Operator | Notes                                              |
| ------------------------- | ----------- | -------------------------------------------------- |
| `WHERE` clause            | Filter      | Predicate extracted, constraint pushdown attempted |
| `JOIN` / relationship     | Join        | Parent/child key pairs, relationship name          |
| `LIMIT`                   | Take        | Numeric limit, optional correlation key            |
| `EXISTS` subquery         | Exists      | Child relationship reference, EXISTS vs NOT EXISTS |
| `OFFSET`                  | Skip        | Numeric offset value                               |
| Correlated subquery limit | Cap         | Simpler than Take                                  |

**Phase 20 approach (per D-04, D-05):**

- TS continues to parse AST (builder.ts already does this well)
- TS extracts operator config as JSON array of descriptors, ordered leaf-to-root
- Each descriptor: `{type: "filter"|"join"|"take"|..., ...operator-specific params, children: [...]}`
- Rust `build_pipeline()` receives this JSON, constructs `Box<dyn Operator>` tree
- Minimal new TS code — just serialization of what builder.ts already computes

## Data Types & Structures

### Core Types (TS)

```typescript
type Row = Record<string, Value>;
type Value = string | number | boolean | null;
type Node = {row: Row; relationships: Record<string, Stream<Node>>};
type Change = {
  type: 0 | 1 | 2 | 3;
  node: Node;
  oldNode?: Node;
  child?: {relationshipName: string; change: Change};
};
type FetchRequest = {
  constraint?: Constraint;
  start?: {row: Row; basis: 'at' | 'after'};
  reverse?: boolean;
};
type Constraint =
  | {key: string; value: Value}
  | {type: 'and' | 'or'; children: Constraint[]};
```

### Core Types (Rust)

Existing in `zero-ivm-rs`:

```rust
// filter.rs
enum Value { Null, Bool(bool), Number(f64), String(String) }

// storage.rs
type Row = serde_json::Value; // JSON object
```

### Type Mapping Strategy

| TS Type        | Rust Type                                                                                                                  | Notes                                      |
| -------------- | -------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------ |
| `Row`          | `serde_json::Map<String, serde_json::Value>`                                                                               | JSON object, consistent with existing code |
| `Node`         | `struct Node { row: Row, relationships: HashMap<String, Vec<Node>> }`                                                      | Eagerly materialized per D-02              |
| `Change`       | `enum Change { Add(Node), Remove(Node), Child{node: Node, rel: String, change: Box<Change>}, Edit{old: Node, new: Node} }` | Rust enum maps naturally                   |
| `FetchRequest` | `struct FetchRequest { constraint: Option<Constraint>, start: Option<Start>, reverse: bool }`                              | Direct mapping                             |
| `Stream<T>`    | `Vec<T>`                                                                                                                   | Per D-02, fully materialized               |
| `Value`        | Existing `Value` enum in filter.rs                                                                                         | Already implemented                        |

## Testing & Validation

### Existing Test Suite

**Cargo tests (139):** Cover filter evaluation, join matching, exists decisions, take state management. These continue to pass as internal helpers are wrapped.

**Vitest pipeline tests:**

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — Integration tests for pipeline fetch and push
- Individual operator tests co-located with each operator (e.g., `filter.test.ts`, `join.test.ts`, `take.test.ts`)
- Success criterion: "All operators pass existing vitest pipeline-driver tests (no TS test changes)"

### Dual Execution Harness

**File:** `packages/zero-cache/src/services/view-syncer/dual-executor.ts`

- Phase 19.5 deliverable, already complete
- `ZERO_DUAL_EXEC=1` — log mismatches between TS and Rust paths
- `ZERO_DUAL_EXEC=strict` — throw on mismatch
- Normalizes results before comparison (sort order, JSON key order)
- **Primary validation strategy for Phase 20:** Run both paths, compare results

### Fuzz Testing

**File:** `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts`

- Property-based testing of IVM correctness
- Generates random schemas, queries, and mutations
- Validates that incremental (push) results match full re-fetch results
- Can be run against Rust path via dual-exec mode

## Key Risks & Considerations

1. **Join complexity (~900 lines TS):** Most complex operator. The flipped-join push path with sorted arrays, binary search, and overlay system requires careful porting. Risk: subtle ordering bugs in child change propagation.

2. **Lazy to eager tradeoff:** TS uses lazy streams for child relationships in Join. Rust materializes eagerly (Vec). For large fan-outs this could increase peak memory. Acceptable per D-02 since results cross FFI anyway, but worth monitoring.

3. **State lifetime across push cycles:** Take and Exists maintain mutable state that persists across multiple push() calls. Rust ownership model handles this naturally (operator owns its state), but the napi Pipeline handle must keep the operator tree alive across calls.

4. **Config JSON schema design:** The JSON descriptor format (D-05) is new — needs careful design to capture all operator parameters without being fragile. Schema changes in TS must stay in sync with Rust deserialization.

5. **Overlay/self-join correctness:** Join uses an overlay HashMap during push to handle reading-own-writes in self-join scenarios. This must be correctly implemented even though parallelism comes later (Phase 24), because the data structures must support it.

6. **Skip and Cap from scratch:** No existing Rust code for these operators. Lower risk since they're simpler (~200-250 lines TS each), but they still need correct push-side behavior.

7. **EditChange semantics:** Edit changes carry both old and new node. Filter must check both to decide whether to convert to Add/Remove. All operators must handle all 4 change types correctly.

## Validation Architecture

### Critical Test Paths

1. **Unit tests (Cargo):** Each new Rust operator gets unit tests. Existing 139 tests continue passing.
2. **Integration (Vitest):** Pipeline-driver tests run unchanged — Rust pipeline produces same results as TS.
3. **Dual execution:** `ZERO_DUAL_EXEC=strict` mode catches any divergence during development.
4. **Fuzz testing:** `fuzz-ivm.test.ts` with dual-exec validates correctness on random inputs.
5. **E2E:** zbugs app with dual-exec enabled validates real-world query patterns.

### Correctness Invariants

- `fetch()` output must be in the same sort order as TS for identical inputs
- `push()` must emit the same set of changes (order may vary, but set must match after normalization)
- Take state must produce identical bound values across push cycles
- Join child relationships must contain the same rows as TS lazy evaluation would produce
- Exists parent size tracking must produce identical add/remove transitions

## RESEARCH COMPLETE
