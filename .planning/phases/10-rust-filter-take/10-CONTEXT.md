# Phase 10: Rust Filter + Take Operators - Context

**Gathered:** 2026-04-20
**Status:** Ready for planning

<domain>
## Phase Boundary

Port the Filter and Take IVM operators to Rust via napi-rs, proving the operator-level napi boundary design. Each operator gets a thin TS wrapper class that delegates to Rust. Unsupported predicate types fall back to existing TS path at build time.

</domain>

<decisions>
## Implementation Decisions

### napi Boundary Design
- **D-31:** Batch per push — TS collects all input changes, sends as array to single Rust napi call, gets array of output changes back
- **D-32:** Relationships (lazy closures) passed through opaquely — Rust never evaluates them, just preserves them on output changes
- **D-33:** Generator yields happen in the TS wrapper (before/after Rust call), not inside Rust

### Filter Predicate Representation
- **D-34:** Hybrid AST eval — ZQL condition AST serialized to Rust at build time
- **D-35:** Rust evaluates structured operators: =, !=, >, <, >=, <=, IN, LIKE, AND, OR, NOT, IS NULL
- **D-36:** Unsupported predicates (custom JS functions, regex, relationship field refs) → entire Filter stays in TS path

### Take Storage Strategy
- **D-37:** In-memory Rust HashMap for bound tracking, never persisted to SQLite
- **D-38:** Bounds reconstructed from source fetch on pipeline hydration (existing behavior)
- **D-39:** No DatabaseStorage dependency in Rust Take

### Fallback Detection
- **D-40:** Build-time detection — at pipeline construction, wrapper decides Rust vs TS based on predicate AST analysis (Filter) or unconditionally (Take always Rust-eligible)
- **D-41:** No per-change runtime fallback; once a pipeline is built with Rust operator, all changes go through Rust
- **D-42:** Filter fallback triggers: unsupported AST node type, custom function predicate, relationship field reference

### Constraints (carried from v2.0)
- **D-29:** NEVER modify test files — tests are correctness oracle
- **D-30:** Unsupported cases fall back to existing TS path

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### IVM Operator Source (to be ported)
- `packages/zql/src/ivm/filter.ts` — Filter operator implementation
- `packages/zql/src/ivm/filter-push.ts` — filterPush helper function
- `packages/zql/src/ivm/take.ts` — Take operator (~400 lines, bound tracking, Storage interface)
- `packages/zql/src/ivm/operator.ts` — Storage, Input, Output, Operator interfaces

### Types and Data Model
- `packages/zql/src/ivm/change.ts` — Change type (Add, Remove, Edit, Child)
- `packages/zql/src/ivm/data.ts` — Node type (row + relationships), Comparator
- `packages/zql/src/ivm/schema.ts` — SourceSchema type
- `packages/zql/src/ivm/change-type.ts` — ChangeType enum

### Pipeline Builder (where Rust/TS decision happens)
- `packages/zql/src/builder/builder.ts` — buildPipeline() / buildPipelineInternal()

### Test Files (DO NOT MODIFY — correctness oracle)
- `packages/zql/src/ivm/filter.test.ts`
- `packages/zql/src/ivm/take.test.ts`
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts`

### Existing Rust Crate
- `packages/zqlite-rs/src/lib.rs` — napi-rs entry point
- `packages/zqlite-rs/Cargo.toml` — dependencies

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `packages/zqlite-rs/` — existing napi-rs crate with build pipeline, patched SQLite
- `packages/zqlite-rs/src/types.rs` — JS↔Rust value conversion already implemented
- `packages/zqlite-rs/src/database.rs` — Database handle pattern for napi

### Established Patterns
- napi-rs `#[napi]` export pattern established in v1.0
- `napi build --platform` in package.json scripts
- Value conversion: JS objects ↔ Rust via serde or manual napi::Env methods

### Integration Points
- `packages/zql/src/builder/builder.ts` — where RustFilter/RustTake wrapper would be instantiated instead of TS Filter/Take
- `packages/zqlite/src/table-source.ts` — genPush() calls operator.push() on the chain

</code_context>

<specifics>
## Specific Ideas

- Filter AST can be extracted from the ZQL query builder — it's available as structured data before being compiled to a JS closure
- Take's `compareRows` function uses the source schema's comparator — this needs to be replicated in Rust (UTF8 comparison per `compareUTF8` in data.ts)
- The TS wrapper class should implement the exact same `Output` interface so it's a transparent drop-in

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 10-rust-filter-take*
*Context gathered: 2026-04-20*
