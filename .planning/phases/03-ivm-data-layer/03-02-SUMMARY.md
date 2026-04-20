---
plan: 03-02
status: completed
started: 2026-04-20
completed: 2026-04-20
---

# Plan 03-02 Summary: TS Integration of Rust queryAll into TableSource

## What Was Built

Wired the Rust `queryAll` method into the TypeScript `TableSource` hot-path, replacing per-row iteration with batch Rust conversion:

1. **`Database.queryAll()` wrapper** in `db.ts` — delegates to Rust `Database.queryAll`
2. **`TableSource.#queryAllTyped()`** error-conversion wrapper — catches Rust errors (bounds, JSON) and re-throws as `UnsupportedValueError` with proper JS-compatible messages and causes
3. **`#fetch` generator** non-debug path now uses `queryAll` instead of statement iteration + `#mapFromSQLiteTypes`
4. **`getRow`** method uses `queryAll` for single-row lookups
5. **`#columnTypes` getter** — lazily builds column type map from schema for Rust

## Key Fixes

- **safeIntegers behavior**: Changed Rust to only return BigInt for values `>= MAX_SAFE_INTEGER` or `<= -MAX_SAFE_INTEGER` (matching better-sqlite3's actual behavior in this codebase)
- **JSON error cause**: Rust now passes raw string value in error; TS re-parses with `JSON.parse()` to produce proper `SyntaxError` cause matching test expectations
- **extract_params array flattening**: Fixed rest-params double-wrapping issue in Rust

## Key Files

- `packages/zqlite/src/table-source.ts` — Major modifications (queryAllTyped, #fetch, getRow)
- `packages/zqlite/src/db.ts` — Added queryAll method
- `packages/zqlite-rs/src/types.rs` — safeIntegers fix, JSON error format change

## Verification

- 34/34 table-source.test.ts pass
- 3/3 db.test.ts pass
- 1/1 statement-cache.test.ts pass
- 31 cargo tests pass
- Committed as `f6371c97a`
