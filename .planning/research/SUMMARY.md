# Research Summary

## Stack Decision
- **napi-rs v3** + **rusqlite 0.39** with `bundled` feature (embeds SQLite)
- Eliminates JS↔C++ FFI entirely — Rust talks to SQLite directly
- Auto-generates TypeScript types from `#[napi]` annotations
- Cross-platform distribution via `@napi-rs/cli` platform packages

## Key Findings

### What Works Well
- napi-rs supports classes, sync methods, iterators, error handling — matches existing Database/Statement API perfectly
- rusqlite's `CachedStatement` maps directly to the existing statement cache pattern
- Batch row return (Vec → JS Array) eliminates per-row FFI crossing overhead
- Direct JS object construction via napi-rs avoids JSON serialization

### Table Stakes
- Exact API compatibility with existing `Database` and `Statement` classes
- Support for `:memory:` databases (testing)
- WAL/WAL2 mode, BEGIN CONCURRENT
- Prepared statement caching
- Transaction/Savepoint support
- Iterator support for `stmt.iterate()`

### Critical Risks
1. **Statement lifetime** — Statement borrows Connection; must use Arc<Connection> or CachedStatement
2. **Thread safety** — rusqlite Connection is Send but not Sync; one Connection per Database instance
3. **Behavioral divergence** — NULL handling, type coercion must exactly match better-sqlite3
4. **Iterator invalidation** — JS holding iterators across state changes
5. **Panics** — must catch at FFI boundary, never crash Node.js

### Architecture
- New `packages/zero-cache-rs/` Rust crate in the monorepo
- Exports: Database, Statement, TableSource, DatabaseStorage, Snapshotter, etc.
- TS imports switch from `./db.ts` to the napi-rs `.node` binary
- Existing tests run unchanged as primary correctness gate

### Performance Strategy
- Batch operations (return Vec<Row> not one-at-a-time)
- Keep iteration loops in Rust (don't cross FFI per iteration)
- Direct napi object construction (no JSON intermediary)
- Zero-copy buffers for bulk data (initial sync)

## Recommendation
Proceed with the 9-step incremental rewrite starting from `db.ts` + `statement-cache.ts`. The napi-rs + rusqlite stack is mature, well-documented, and maps cleanly to the existing API surface.
