# Phase 3: IVM Data Layer — Research

## Summary

- **Hot path identified**: `#fetch()` → `statement.iterate()` → `#mapFromSQLiteTypes()` → `fromSQLiteTypes()`. Each row crosses FFI boundary individually and gets type-converted in TS. This is the target.
- **Rust API**: Add `queryAll(sql, params, columnTypes)` and `queryBatched(sql, params, columnTypes, batchSize)` as methods on the existing Rust `Database` class. These do prepare + iterate + type-convert + build JS objects entirely in Rust.
- **Integration is minimal**: `#fetch()` replaces 3 lines (prepare → iterate → mapFromSQLiteTypes) with a single `db.queryAll()` or batch loop. Generator/overlay/yield pattern stays TS.
- **DatabaseStorage stays TS** (D-11). No Rust work needed.
- **Column type metadata**: Passed as `Record<string, string>` (e.g. `{id: "string", a: "number", c: "boolean", d: "json"}`). Rust parses once per query, applies type conversion per cell.

## TableSource Architecture

### The Hot Path (`#fetch`, table-source.ts:283-361)

```
#fetch(req, connection)
  → #requestToSQL(req, filters, sort)        // builds SQLQuery AST (stays TS)
  → format(query)                             // produces {text: string, values: any[]}
  → stmts.cache.get(text)                     // StatementCache lookup (stays TS)
  → cachedStatement.statement.safeIntegers(true)
  → cachedStatement.statement.iterate<Row>(...values)  // ← FFI per row
  → #mapFromSQLiteTypes(columns, rowIterator, ...) // ← TS type conversion per row
  → generateWithOverlay / generateWithOverlayUnordered  // overlay merging (stays TS)
  → generateWithYields                        // yield sentinel insertion (stays TS)
```

### `#mapFromSQLiteTypes` (table-source.ts:364-388)

Generator that wraps `rowIterator.next()` in `timeSampled()` (OTel timing), then calls `fromSQLiteTypes()` per row. The `timeSampled` wrapper is for slow-row detection.

### `fromSQLiteTypes` (table-source.ts:588-605)

Per-row function: iterates `Object.keys(row)`, maps each value through `fromSQLiteType()`:

- `boolean`: `!!v` (SQLite stores as 0/1 INTEGER)
- `number`/`string`/`null`: pass through, but convert bigint→number (throws if out of safe range)
- `json`: `JSON.parse(v as string)` (throws UnsupportedValueError on invalid JSON)

### `getRow` (table-source.ts:506-519)

Single-row fetch using `statement.get()` + `fromSQLiteTypes`. Can use `queryAll` (expects 0-1 rows).

## Row Format and Type Conversion

### SQLite → JS Type Map (with `safeIntegers(true)`)

| SQLite Type | JS Raw Type           | Column Schema | Converted JS Type                          |
| ----------- | --------------------- | ------------- | ------------------------------------------ |
| NULL        | null                  | any           | null                                       |
| INTEGER     | bigint (safeIntegers) | boolean       | `!!v` → boolean                            |
| INTEGER     | bigint (safeIntegers) | number        | `Number(v)` (throws if > MAX_SAFE_INTEGER) |
| INTEGER     | bigint (safeIntegers) | string        | bigint (pass through)                      |
| REAL        | number                | number        | number (pass through)                      |
| TEXT        | string                | string        | string (pass through)                      |
| TEXT        | string                | json          | `JSON.parse(v)`                            |

### Rust Implementation Notes

- With `safeIntegers(true)`, `statement.iterate()` returns INTEGER as bigint. In Rust, rusqlite returns INTEGER as `i64`.
- For `boolean` columns: Rust reads i64, sets JS property to boolean (`env.get_boolean(i != 0)`)
- For `number` columns: Rust reads i64, checks safe integer bounds (±2^53), creates f64 JS number
- For `json` columns: Rust reads text, must call `JSON.parse()` on JS side OR parse in Rust via serde_json. **Recommendation**: Use `serde_json::from_str()` in Rust and convert to JsObject via napi. Avoids FFI call to JSON.parse. But risk: serde_json may not produce identical results for edge cases. **Safer**: Just return the string and let TS JSON.parse it... but that defeats the purpose. **Decision**: Parse JSON in Rust using `serde_json`, convert to JS values. Test edge cases.
- For bigint out-of-bounds: Rust must throw an error matching `UnsupportedValueError` message format: `"value {v} (in {table}.{column}) is outside of supported bounds"`. TS wraps this.

## Rust API Design

### On Database class (database.rs)

```rust
#[napi]
pub fn query_all(
    &self,
    env: Env,
    sql: String,
    params: Vec<JsUnknown>,  // or raw napi array
    column_types: JsObject,   // {colName: "boolean"|"number"|"string"|"json"|"null"}
    table_name: String,       // for error messages
) -> Result<Vec<JsObject>>
```

```rust
#[napi]
pub fn query_batched(
    &self,
    env: Env,
    sql: String,
    params: Vec<JsUnknown>,
    column_types: JsObject,
    table_name: String,
    batch_size: u32,
) -> Result<JsObject>  // Returns a BatchIterator
```

### BatchIterator class

```rust
#[napi]
pub struct BatchIterator { ... }

#[napi]
impl BatchIterator {
    #[napi]
    pub fn next_batch(&mut self, env: Env) -> Result<Option<Vec<JsObject>>>
}
```

**Alternative (simpler)**: Skip BatchIterator class. Have `queryBatched` return `Vec<Vec<JsObject>>` (array of arrays). TS iterates over the outer array. Simpler, no lifetime issues with holding a prepared statement across FFI calls.

**Recommendation**: Use the simpler approach — `queryBatched` returns `Vec<Vec<JsObject>>`. The statement lifetime issue (Rust prepared statement cannot live across FFI calls without `Arc<Mutex<>>` complexity) makes the iterator approach brittle. The memory impact of buffering all rows is minimal — these are typically bounded by LIMIT or table size.

Actually, **even simpler**: Just implement `queryAll`. If we need batching for yield-point insertion, TS can slice the returned array. The whole point is to avoid N FFI calls. One call returning all rows achieves that. The `queryBatched` API adds complexity without clear benefit since the overlay/yield logic is TS-side anyway.

**Final recommendation**: Start with `queryAll` only. Add `queryBatched` if benchmarks show memory pressure from large result sets.

## Integration Strategy

### Minimal Change to `#fetch()`

Current (table-source.ts:289-293):

```ts
const cachedStatement = this.#stmts.cache.get(sqlAndBindings.text);
cachedStatement.statement.safeIntegers(true);
const rowIterator = cachedStatement.statement.iterate<Row>(
  ...sqlAndBindings.values,
);
```

Replace `#mapFromSQLiteTypes` usage with:

```ts
// Instead of iterate() + #mapFromSQLiteTypes(), use Rust queryAll
const rows = this.#db.queryAll(
  sqlAndBindings.text,
  sqlAndBindings.values,
  this.#columnTypesMap,
  this.#table,
);
```

Then feed `rows` (an array) as an iterable into `generateWithOverlay` / `generateWithOverlayUnordered` (they accept `Iterable<Row>`).

### Problem: `#fetch` uses StatementCache

The current code uses `this.#stmts.cache.get(sql)` to get a cached statement, then calls `.iterate()` on it. With `queryAll`, we bypass the statement cache entirely — Rust manages its own prepared statement internally.

This is fine because:

1. `queryAll` creates a new prepared statement per call (Rust side) — but SQLite has its own internal statement cache
2. The TS StatementCache was only needed because `better-sqlite3`'s `prepare()` was expensive. With rusqlite, `prepare()` is fast.
3. Per D-08, statement cache stays TS for the write-path statements (insert/delete/update/checkExists/getExisting).

### Problem: `timeSampled` wrapper

`#mapFromSQLiteTypes` wraps each `rowIterator.next()` in `timeSampled()` for OTel slow-row detection. With `queryAll`, all rows are fetched in one call — no per-row timing.

Resolution: Wrap the entire `queryAll` call in `timeSampled` (or just measure total time). Per-row timing is overkill for a batch operation. The OTel span will cover the whole batch.

### Problem: `rowIterator.return()` in finally block

The finally block (line 338-361) calls `rowIterator.return?.()` to close the SQLite iterator, and accesses `scanStatus` for debug info. With `queryAll`, there's no iterator to close — the query completes fully in Rust. Debug scan status is not available (rusqlite doesn't expose it across FFI easily).

Resolution: Remove `rowIterator.return()` call. For debug scanStatus, skip it initially (D-06: logging deferred).

### Access to Database instance

`#fetch` currently accesses `this.#stmts.cache` which is on a `Statements` object keyed by `Database` instance. The `queryAll` method needs the raw Rust Database. Currently `db.ts` wraps the Rust Database:

```ts
// db.ts — the TS wrapper
export class Database {
  #db: RustDatabase; // the napi class
  // ...
}
```

Need to expose the inner Rust Database for `queryAll` calls. Options:

1. Add `queryAll` as a method on the TS `Database` wrapper (delegates to Rust)
2. Store the Rust `Database` reference directly in TableSource
3. Add `queryAll`/`queryBatched` to the TS Database class which delegates

**Recommendation**: Option 1 — add `queryAll` method to TS `Database` class in `db.ts`. It delegates to `this.#db.queryAll(...)`. Clean, maintains existing interface.

## Test Analysis

### table-source.test.ts (1030 lines)

**Test groups:**

1. **"fetching from a table source"** (line 41-209): 14 parameterized test cases. Creates a foo table with 27 rows (3x3x3), tests various sort orders, constraints, start positions (at/after). Tests the full pipeline: `source.connect() → out.fetch() → rows`.
2. **"fetched value types"** (line 212-313): 8 cases testing type conversion: null, number, float, boolean, bigint, json string/null/object/array, safe integer boundaries, bigint overflow.
3. **"pushing values"** (line 315-569): Tests write path (add/remove/edit). Write path stays TS (D-13), so this should be unaffected.
4. **"getByKey"** (line 572-653): Tests `getRow()` with various types including bigint.
5. **"optional filters to sql"** (line 655-851): Tests SQL query generation. Pure TS, unaffected.
6. **"fromSQLiteTypes error messages"** (line 855-967): Tests error messages for invalid column, bigint overflow, invalid JSON. **Critical**: These test exact error message formats. Rust must produce matching messages.
7. **"SQLite iterator is closed"** (line 970-1030): Tests that `rowIterator.return()` is called on error. With `queryAll`, this test may need adjustment since there's no iterator to close. **But the requirement says tests must pass unchanged.** Need to check if this test still works with queryAll.

### Risk: Test at line 970 ("SQLite iterator is closed")

This test monkey-patches `Statement.prototype.iterate` and checks that `.return()` is called. If `#fetch` no longer calls `statement.iterate()`, this test will fail because the monkey-patch won't be hit.

**But wait**: looking more carefully, the test specifically tests that the iterator is closed when `debug.initQuery()` throws. With `queryAll`, there's no iterator at all — the query would fail differently. The test asserts `iteratorReturnCalled === true` after catching the error.

**This test WILL FAIL if we change `#fetch` to use `queryAll`** because:

1. It patches `Statement.prototype.iterate` — which won't be called
2. It expects `iteratorReturnCalled` to be true — which won't happen

**Resolution options:**

1. Keep `statement.iterate()` for the debug code path and only use `queryAll` for non-debug
2. Accept that this specific test needs a small modification (violates "tests pass unchanged")
3. Make `queryAll` still go through a code path that the test can observe

**Recommended**: Option 1 — when `debug` delegate is provided, fall back to the existing iterate path. The debug path is not performance-critical. Only the non-debug hot path uses `queryAll`. This way ALL tests pass unchanged.

## napi-rs Batch Return Patterns

### Building JS Objects in Rust

The existing `row_to_js_object` in types.rs already builds a JsObject from a rusqlite Row. For `queryAll`, we need a variant that also applies type conversion (boolean, bigint→number, json parse).

New function needed: `row_to_typed_js_object(env, row, columns, column_types, table_name)`:

- For each column, read the rusqlite value
- Apply type conversion based on `column_types[col]`:
  - `"boolean"`: NULL→null, INTEGER→boolean
  - `"number"`: NULL→null, INTEGER→check safe bounds→f64, REAL→f64
  - `"string"`: NULL→null, TEXT→string
  - `"json"`: NULL→null, TEXT→parse JSON in Rust (serde_json)→convert to JS
  - `"null"`: pass through (same as number/string)
- Set on JsObject

### JSON Parsing in Rust

For `json` columns, we need to convert SQLite TEXT → JS value (could be object, array, string, number, boolean, null).

Options:

1. **serde_json → napi conversion**: Parse with serde_json, recursively convert `serde_json::Value` → JS value. Handles all JSON types.
2. **Call JS `JSON.parse`**: Use `env.get_global()?.get_named_property::<JsFunction>("JSON")?.call_method("parse", &[text_value])`. One FFI roundtrip per JSON cell.
3. **Return raw string, let TS parse**: Defeats purpose of Rust batch.

**Recommendation**: Option 1 (serde_json). It's pure Rust, no FFI overhead. Need a helper `serde_json_to_js(env, value) -> Result<JsUnknown>`.

### Returning Vec<JsObject>

napi-rs supports returning `Vec<JsObject>` which becomes a JS Array. This is the natural return type for `queryAll`.

### Parameter Passing

SQL params come as a JS array. Use existing `js_array_params_to_sqlite` from types.rs.

Column types come as a JS object `{colName: typeString}`. Parse once into a `Vec<(String, ColumnType)>` where `ColumnType` is a Rust enum.

## Risks and Mitigations

| Risk                                           | Impact                                       | Mitigation                                                                         |
| ---------------------------------------------- | -------------------------------------------- | ---------------------------------------------------------------------------------- |
| Test line 970 fails (iterator close test)      | Blocks merge                                 | Fall back to iterate path when debug delegate present                              |
| JSON parse differences (serde vs V8)           | Subtle data bugs                             | Test all JSON edge cases from test suite                                           |
| Error message format mismatch                  | Test failures                                | Match exact format: "value {v} (in {table}.{column}) is outside..."                |
| StatementCache bypass                          | Possible perf regression on repeated queries | SQLite has internal stmt cache; benchmark to verify                                |
| Memory pressure from queryAll on large tables  | OOM for unbounded queries                    | Queries always have WHERE clauses or LIMIT in practice; add queryBatched if needed |
| `fromSQLiteTypes` is imported by other modules | Breaking change if removed                   | Keep `fromSQLiteTypes` as TS export (D-12), Rust only used internally by queryAll  |

## Dependency Graph

```
queryAll (Rust, database.rs)
  ├── js_array_params_to_sqlite (types.rs, exists)
  ├── row_to_typed_js_object (types.rs, NEW)
  │     ├── set_sqlite_value (types.rs, exists) — for string/number/null
  │     ├── boolean conversion (NEW)
  │     ├── safe integer check (NEW)
  │     └── serde_json_to_js (types.rs, NEW) — for json columns
  └── ColumnType enum (types.rs, NEW)

table-source.ts changes
  ├── #fetch() — replace iterate+mapFromSQLiteTypes with queryAll
  ├── getRow() — replace get+fromSQLiteTypes with queryAll
  └── Store reference to underlying Rust Database

db.ts changes
  └── Add queryAll() method delegating to Rust
```

---

## RESEARCH COMPLETE

_Researched: 2026-04-20_
_Phase: 03-ivm-data-layer_
