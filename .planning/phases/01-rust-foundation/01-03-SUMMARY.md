---
phase: 1
plan: 03
status: complete
started: 2026-04-20
completed: 2026-04-20
---

# Summary 01-03: TS Wrapper Swap + Test Verification

## What was built

Replaced `packages/zqlite/src/db.ts` internals to use the Rust `zqlite-rs` native module instead of `@rocicorp/zero-sqlite3` (better-sqlite3 fork). The TS wrapper preserves the exact same public API surface while all SQLite I/O now goes through Rust/rusqlite.

## Key changes

### packages/zqlite/src/db.ts

- Imports `Database as RustDatabase`, `Statement as RustStatement`, `RowIterator as RustRowIterator` from `zqlite-rs`
- `Database` class wraps `RustDatabase` with OTel tracing, slow-query logging, error augmentation
- `Statement` class wraps `RustStatement`, passes variadic params as array to Rust
- `LoggingIterableIterator` wraps `RustRowIterator` with timing instrumentation
- Created local `SqliteError` class (replaces import from `@rocicorp/zero-sqlite3`)
- Exported `RunResult` type
- Stubbed `scanStatus`/`scanStatusReset` (Phase 1 — full impl in later step)
- Error messages cleaned: strips rusqlite's " in ... at offset N" suffix to match better-sqlite3 format

### packages/zqlite-rs/src/database.rs

- Removed SQL from error messages (TS wrapper handles augmentation)
- Removed `PRAGMA optimize` from `close()` (TS wrapper handles it with logging)

### packages/zqlite-rs/src/statement.rs

- `run()` now handles SELECT statements (catches `ExecuteReturnedResults`, falls back to `query()`)

### packages/zqlite/package.json

- Replaced `@rocicorp/zero-sqlite3` dependency with `zqlite-rs: "0.0.0"`

## Verification

- `db.test.ts`: 3/3 tests pass (slow query logging, error annotation, compaction)
- `statement-cache.test.ts`: 1/1 test passes
- `cargo test` in zqlite-rs: 28/28 Rust unit tests pass
- Zero modifications to any test files

## key-files

### key-files.modified

- packages/zqlite/src/db.ts
- packages/zqlite/package.json
- packages/zqlite-rs/src/database.rs
- packages/zqlite-rs/src/statement.rs

## Deviations

- **Error message cleanup**: Rusqlite includes " in {sql} at offset N" in syntax errors, which better-sqlite3 doesn't. Added regex strip in TS `#run()` method.
- **Statement.run() on SELECT**: Rusqlite's `execute()` rejects SELECTs. Added fallback to `query()` + drain to match better-sqlite3 behavior.
- **compact()**: Kept entirely in TS (uses `this.pragma()` calls) rather than delegating to Rust `compact()`, preserving all logging behavior.
