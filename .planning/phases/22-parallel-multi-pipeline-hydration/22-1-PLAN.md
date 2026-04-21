# Phase 22: Parallel Multi-Pipeline Hydration

## Objective

Enable multiple IVM pipelines to be hydrated in parallel using Rayon, combining the operator tree (Phase 20) with the SQLite TableSource (Phase 21) into a unified hydration path.

## Implementation

### 22-1: LiveTableSource + hydrate_pipelines()

**Files changed:**

- `packages/zqlite-rs/src/hydrate.rs` — NEW: LiveTableSource (Operator impl wrapping RustTableSource), hydrate_pipelines() using Rayon par_iter, build_operator_with_live_source() recursive builder
- `packages/zqlite-rs/src/table_source.rs` — Added `unsafe impl Send/Sync for RustTableSource` with safety justification
- `packages/zqlite-rs/src/lib.rs` — Added `mod hydrate;`
- `packages/zqlite-rs/Cargo.toml` — Added `zero-ivm-rs` path dependency
- `packages/zero-ivm-rs/Cargo.toml` — Added `rlib` to crate-type for library use

**Architecture:**

- `LiveTableSource` wraps `Arc<RustTableSource>` and implements `zero_ivm_rs::operator::Operator`
- Converts between zqlite-rs source types and zero-ivm-rs IVM types
- `hydrate_pipelines()` accepts `Vec<HydratePipelineConfig>` and runs them via `rayon::par_iter`
- Each pipeline builds a full operator tree (Source→Filter→Join→Take→Exists→Skip→Cap) with live SQLite backing
- Thread safety: RustTableSource is `Send+Sync` because ConnectionPool uses `Arc<Mutex<>>` and connections are read-only during hydration

<threat_model>

- **unsafe Send/Sync**: RustTableSource contains rusqlite::Connection (RefCell inside). Safe because hydration only uses Mutex-protected pool, never write_conn. Mitigated by: (1) write_conn only called from push() which requires &mut self, (2) pool.get() is Mutex-guarded.
- **Connection exhaustion**: Many parallel pipelines could exhaust the pool. Mitigated by: pool.get() blocks on Mutex, natural backpressure.
- **Panic propagation**: Rayon propagates panics across threads. Mitigated by: hydrate_single_pipeline returns Result, errors are captured per-pipeline.
  </threat_model>

## Verification

- 133 zqlite-rs cargo tests pass (8 new hydrate tests)
- 102 zero-ivm-rs cargo tests pass
- 28/30 vitest pipeline-driver tests pass (2 pre-existing failures)
