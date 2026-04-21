# Phase 20: Rust Operator Trait & Pipeline Builder - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Port the full IVM operator tree to Rust — unified `Operator` trait with `fetch()` and `push()` methods, all 6 operators (Filter, Join, Take, Exists, Skip, Cap), and a pipeline builder that constructs operator trees from TS-extracted config. This phase does NOT include SQLite connection pooling (Phase 21) or parallelism (Phases 22-24) — operators work with a single connection passed in.

</domain>

<decisions>
## Implementation Decisions

### Trait Design

- **D-01:** Use `Box<dyn Operator>` trait objects for the operator tree. Matches TS class hierarchy, vtable cost negligible vs SQLite I/O.
- **D-02:** `fetch()` returns `Vec<Node>`, `push()` returns `Vec<Change>`. Collected results, not streaming. Results are always fully materialized before crossing FFI boundary.

### Code Reuse

- **D-03:** Wrap existing v2.0/v3.0 Rust code (filter.rs, join.rs, exists.rs, take_state.rs) inside new Operator trait impls. These become internal helpers. 139 cargo tests already validate this evaluation logic — don't rewrite.

### Pipeline Builder Boundary

- **D-04:** TS extracts operator config from ZQL AST (filter predicates, join keys, take limits, etc.) and sends as JSON to Rust. Rust builds the operator tree from this config. TS already has the AST parser (builder.ts) — no need to duplicate in Rust.
- **D-05:** The config format is a JSON array of operator descriptors, ordered leaf-to-root. Each descriptor has `type`, operator-specific params, and `children` references.

### State Lifetime

- **D-06:** Each operator owns its mutable state directly (Take owns sorted bounds, Join owns overlay HashMap). Pipeline is single-owner. No Arc<Mutex> — cross-pipeline parallelism comes in Phase 22/24 where each pipeline is independently owned by a Rayon thread.

### Claude's Discretion

- Exact Operator trait method signatures (error types, lifetime parameters)
- Internal data structures for each operator's state
- Config JSON schema details
- Whether Skip and Cap are standalone operators or thin wrappers

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### TS IVM Operators (source of truth for behavior)

- `packages/zql/src/ivm/operator.ts` — Storage, Input, Output, FetchRequest interfaces
- `packages/zql/src/ivm/filter.ts` — TS Filter operator
- `packages/zql/src/ivm/join.ts` — TS Join (~700 lines, most complex)
- `packages/zql/src/ivm/take.ts` — TS Take (~400 lines)
- `packages/zql/src/ivm/exists.ts` — TS Exists operator
- `packages/zql/src/ivm/data.ts` — Node type, compareValues
- `packages/zql/src/builder/builder.ts` — buildPipeline (AST to operator tree)

### Existing Rust Code (to wrap, not rewrite)

- `packages/zero-ivm-rs/src/filter.rs` — Predicate evaluator, RustFilterPredicate napi class
- `packages/zero-ivm-rs/src/join.rs` — Join hot-path (isJoinMatch, buildJoinConstraint)
- `packages/zero-ivm-rs/src/exists.rs` — Exists batch push decisions
- `packages/zero-ivm-rs/src/take_state.rs` — Take state machine
- `packages/zero-ivm-rs/src/storage.rs` — RustStorage HashMap
- `packages/zqlite-rs/src/advance.rs` — rust_fan_out(), process_change_for_pipeline()

### Correctness Infrastructure

- `packages/zero-cache/src/services/view-syncer/dual-executor.ts` — Dual-exec comparator
- `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — Property-based fuzz
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — Integration point

### Documentation

- `HYDRATION-FLOW.md` — Detailed hydration flow, bottlenecks, parallelization opportunities

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `filter.rs` Predicate evaluator: 15 comparison ops, JSON value handling — becomes FilterOperator's internal evaluator
- `join.rs` isJoinMatch/buildJoinConstraint: constraint building and row matching — becomes JoinOperator's push helper
- `exists.rs` batch decision logic: child existence check — becomes ExistsOperator's push helper
- `take_state.rs` HashMap-based sorted state: bound tracking — becomes TakeOperator's state
- `storage.rs` RustStorage: in-memory row store — becomes operator tree's intermediate storage

### Established Patterns

- napi classes expose Rust to TS (RustFilterPredicate, RustTakeStorage)
- serde_json::Value for row data interchange
- `#[napi]` functions for FFI entry points
- Separate crates: zero-ivm-rs (no SQLite), zqlite-rs (SQLite-touching)

### Integration Points

- New napi function: `build_pipeline(config_json: String) -> Pipeline` — returns opaque handle
- Pipeline methods: `pipeline.fetch(request_json) -> Buffer`, `pipeline.push(change_json) -> Buffer`
- Existing `rust_fan_out()` in advance.rs to be eventually replaced by full operator tree push (Phase 24)

</code_context>

<specifics>
## Specific Ideas

- Operator trait should closely mirror TS interfaces in operator.ts but adapted for Rust ownership
- Config JSON format should be derivable from TS builder.ts's internal AST walk — minimal new TS code
- Skip and Cap can be thin operators that modify FetchRequest bounds and delegate

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

_Phase: 20-rust-operator-trait-pipeline-builder_
_Context gathered: 2026-04-21_
