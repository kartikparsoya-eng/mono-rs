# Phase 13: Rayon Parallelism for Fan-out - Context

**Gathered:** 2026-04-20
**Status:** Ready for planning

<domain>
## Phase Boundary

Add Rayon-based parallelism to the IVM advance loop. Rust owns the full advance cycle:
read snapshot diff directly from SQLite, fan out each change across multiple pipelines
via Rayon `par_iter`, and return aggregated row changes to TS. Goal is 2-4x speedup on
multi-pipeline workloads.

</domain>

<decisions>
## Implementation Decisions

### Parallelism Granularity
- **D-62:** Per-pipeline fan-out — main thread reads diff once, Rayon `par_iter` over
  pipelines that consume each changed table. Each pipeline's operator chain runs on a
  Rayon thread. No I/O duplication since diff is pre-read.
- **D-63:** NOT intra-operator batch splitting — parallelism is across pipelines, not
  within a single operator's batch.

### Rayon Integration Point
- **D-64:** Rust-owned fan-out — new napi function that takes pipeline configurations
  and drives the full advance loop. Rust owns the Rayon thread pool and pipeline topology.
  TS does not orchestrate individual pipeline pushes.

### Snapshot Diff Sharing
- **D-65:** Rust reads snapshot diff directly from SQLite via `zqlite-rs`. No TS
  involvement for diff reads. Rust owns the full advance loop including snapshot
  diff iteration. Requires Rust to understand snapshotter diff logic.
- **D-66:** Worker threads POC failure (D-26) is avoided because: single Rust process
  reads diff once, shares via Arc/reference across Rayon threads. No per-worker I/O
  duplication.

### Constraints (carried forward)
- **D-29:** NEVER modify test files
- **D-44:** Zero modifications to `packages/zql/`
- **D-31:** Batch per push — single napi call per operator per push cycle

### Claude's Discretion
- Pipeline topology serialization format (how TS describes pipeline configs to Rust)
- Snapshotter diff porting scope — which parts of `snapshotter.ts` to replicate in Rust
- Rayon thread pool sizing strategy
- Error handling and fallback when Rust advance fails

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Advance Loop (primary port target)
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` lines 635-734 — `#advance()` generator, the hottest code path
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` lines 404-590 — `addQuery()`, pipeline construction and topology

### Snapshot Diff (Rust must replicate)
- `packages/zqlite/src/snapshotter.ts` — `advance()`, `SnapshotDiff` iterator, diff SQL queries
- `packages/zqlite/src/table-source.ts` — `genPush()`, how changes flow from diff to pipelines

### Existing Rust Crates
- `packages/zero-ivm-rs/src/` — Filter, Join, Take, Exists operators already in Rust
- `packages/zqlite-rs/src/` — Rust SQLite access (database.rs, statement.rs)

### IVM Operator Interfaces
- `packages/zql/src/ivm/operator.ts` — Input/Output/Storage interfaces
- `packages/zql/src/ivm/change.ts` — Change types

### Test Files (DO NOT MODIFY)
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — 30 tests (1 pre-existing failure)

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `packages/zero-ivm-rs/` — Filter, Join, Take, Exists already ported to Rust
- `packages/zqlite-rs/` — Rust SQLite foundation with DB handle, statement cache
- Rayon crate — standard Rust parallelism, `par_iter` for data parallelism

### Established Patterns
- napi-rs batch function pattern (JSON serialization at boundary)
- Delegate-based injection via `pipeline-driver.ts`
- Per-operator push batching (D-31)

### Integration Points
- `pipeline-driver.ts#advance()` — replace with single Rust napi call
- `pipeline-driver.ts#addQuery()` — must serialize pipeline topology to Rust
- `snapshotter.ts#advance()` — diff generation moves to Rust

</code_context>

<specifics>
## Specific Ideas

- Rust advance function signature: `rust_advance(db_path, prev_version, curr_version, pipeline_configs) -> RowChange[]`
- Pipeline config includes: table sources, operator chain (filter predicates, join keys, take bounds, exists relationships)
- Each Rayon thread runs a pipeline's operator chain independently — no shared mutable state
- TS receives flat array of row changes, same format as current `#advance()` yields

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 13-rayon-parallelism-for-fan-out*
*Context gathered: 2026-04-20*
