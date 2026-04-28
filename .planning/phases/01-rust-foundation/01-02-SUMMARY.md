---
phase: 01-rust-foundation
plan: 02
subsystem: database
tags: [napi-rs, rusqlite, sqlite, ffi, rust]

requires:
  - phase: 01-01
    provides: Cargo workspace scaffold, napi build toolchain
provides:
  - Rust Database class with open/exec/pragma/prepare/close/transaction/compact/unsafeMode
  - Rust Statement class with run/get/all/iterate/safeIntegers
  - Rust RowIterator class with lazy JS conversion
  - SQLite→JS type coercion (NULL/INTEGER/REAL/TEXT/BLOB)
affects: [01-03, step-2-statements, step-3-database-storage]

tech-stack:
  added: [rusqlite 0.33 bundled, napi 3 compat-mode]
  patterns:
    [
      Arc<RefCell<Connection>> shared ownership,
      prepare_cached for statement reuse,
      materialize-then-convert for iterators,
    ]

key-files:
  created:
    - packages/zqlite-rs/src/types.rs
    - packages/zqlite-rs/src/database.rs
    - packages/zqlite-rs/src/statement.rs
    - packages/zqlite-rs/src/row_iterator.rs
  modified:
    - packages/zqlite-rs/src/lib.rs
    - packages/zqlite-rs/Cargo.toml
    - packages/zqlite-rs/package.json

key-decisions:
  - 'Used napi v3 compat-mode for JsObject/JsBigInt — raw napi::sys calls for transaction callback and pragma returns'
  - 'Statement re-prepares via prepare_cached on each call (no borrow lifetime issues)'
  - 'RowIterator materializes all rows as Vec<Vec<Value>> in Rust, converts lazily to JS on next()'
  - "Removed 'type: module' from package.json — napi-rs generates CJS index.js"

patterns-established:
  - 'Database owns Arc<RefCell<Connection>>, Statement clones the Arc'
  - 'Error handling: Result objects via napi::Result, no throws from Rust'
  - 'Type coercion: INTEGER→f64 default, INTEGER→BigInt with safeIntegers flag'

requirements-completed: [FOUND-01, FOUND-02, FOUND-03, TEST-02]

duration: ~120min
completed: 2026-04-20
---

# Plan 01-02: Database + Statement Classes Summary

**Rust napi-rs Database/Statement/RowIterator classes with rusqlite, 14 passing unit tests, full SQLite type coercion**

## Performance

- **Duration:** ~120 min (across sessions, including debugging export issue)
- **Tasks:** 5/5
- **Files created:** 4
- **Files modified:** 3

## Accomplishments

- Database class with constructor, exec, pragma, prepare, close, transaction, compact, unsafeMode, name/inTransaction/readonly getters
- Statement class with run, get, all, iterate, safeIntegers — uses prepare_cached for zero-cost re-preparation
- RowIterator with materialize-in-Rust, convert-lazily-to-JS pattern
- Complete SQLite→JS type coercion: NULL→null, INTEGER→number/bigint, REAL→number, TEXT→string, BLOB→Buffer
- 14 Rust unit tests all passing (types, database, statement)

## Files Created/Modified

- `packages/zqlite-rs/src/types.rs` — SqliteValue helpers, row_to_js_object, js_array_params_to_sqlite
- `packages/zqlite-rs/src/database.rs` — Database #[napi] class (~350 lines)
- `packages/zqlite-rs/src/statement.rs` — Statement #[napi] class (~150 lines)
- `packages/zqlite-rs/src/row_iterator.rs` — RowIterator #[napi] class (~50 lines)
- `packages/zqlite-rs/src/lib.rs` — Module declarations (updated)
- `packages/zqlite-rs/Cargo.toml` — Added rusqlite dependency
- `packages/zqlite-rs/package.json` — Removed "type: module" (fix for CJS exports)

## Decisions Made

- Used raw `napi::sys` calls for transaction callback and pragma JsObject returns (napi v3 compat-mode types don't implement FromNapiValue/ToNapiValue)
- Statement stores SQL string + Arc<RefCell<Connection>>, re-prepares via `prepare_cached()` on each call — avoids borrow lifetime issues across FFI
- RowIterator materializes all rows upfront as `Vec<Vec<Value>>` — safe approach avoiding raw pointer/lifetime complexity

## Deviations from Plan

### Auto-fixed Issues

**1. "type: module" in package.json broke CJS exports**

- **Found during:** Task 5 (integration testing after build)
- **Issue:** napi-rs generates CJS `index.js` with `module.exports`, but `"type": "module"` in package.json caused Node.js to treat it as ESM — `require()` returned empty object
- **Fix:** Removed `"type": "module"` from package.json
- **Verification:** `require('./index.js')` now returns Database, Statement, RowIterator correctly

**2. napi v3 type compatibility required raw sys calls**

- **Found during:** Tasks 2-3 (Database and Statement implementation)
- **Issue:** `JsObject`, `JsBigInt` (compat-mode) can't be used in `#[napi]` method signatures — codegen doesn't recognize them
- **Fix:** Used raw `napi::sys` for complex return types (pragma, transaction, run result), pure `#[napi]` derive for struct exports and simple methods
- **Verification:** `cargo build` succeeds, `npm run build` produces working binary

---

**Total deviations:** 2 auto-fixed
**Impact on plan:** Both necessary for correctness. No scope creep.

## Issues Encountered

- 36 deprecation warnings from `JsObject` usage (acceptable, napi v3 transition)
- Initial confusion about why exports were empty — turned out to be ESM/CJS mismatch, not napi codegen issue

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- Database, Statement, RowIterator all exported and usable from Node.js
- Ready for Plan 01-03: TS wrapper swap (replace db.ts internals with zqlite-rs)
- `db.test.ts` can be run against the Rust backend once wrapper is in place

---

_Phase: 01-rust-foundation_
_Completed: 2026-04-20_
