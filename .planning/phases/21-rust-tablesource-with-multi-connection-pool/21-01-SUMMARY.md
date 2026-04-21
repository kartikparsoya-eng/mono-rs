# Plan 21-01 Summary: Connection Pool, Source Types & Query Builder

**Status:** COMPLETE
**Date:** 2026-04-21

## Delivered

- `connection_pool.rs` — Lightweight `Arc<Mutex<Vec<Connection>>>` pool with WAL snapshot pinning via `BEGIN DEFERRED` transactions. `acquire()` / `release()` / `close_all()` API.
- `source.rs` — Source trait + types: `Row`, `Node`, `Change`, `Overlay`, `FetchRequest`, `FetchResult`, `Direction`, `Bound`. Foundational types for table source integration.
- `query_builder.rs` — SQL SELECT builder supporting column selection, WHERE constraints, ORDER BY with ASC/DESC, start-bound generation (exclusive/inclusive), LIMIT. Parameterized queries to prevent injection.

## Tests

- 97 cargo tests passing in zqlite-rs after this plan
- Connection pool: acquire/release, concurrent access, WAL snapshot consistency
- Query builder: simple select, where clauses, ordering, bounds, limits, parameterization

## Files Created/Modified

- `packages/zqlite-rs/src/connection_pool.rs` (NEW)
- `packages/zqlite-rs/src/source.rs` (NEW)
- `packages/zqlite-rs/src/query_builder.rs` (NEW)
- `packages/zqlite-rs/src/lib.rs` (MODIFIED — module declarations)
- `packages/zqlite-rs/Cargo.toml` (MODIFIED — tempfile dev-dep)
