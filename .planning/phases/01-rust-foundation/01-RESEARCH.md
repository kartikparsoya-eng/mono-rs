# Phase 1: Rust Foundation - Research

**Researched:** 2026-04-20
**Status:** Complete

## 1. Current Implementation Analysis

### Database class (`packages/zqlite/src/db.ts`, 329 lines)

**Constructor:** `Database(lc: LogContext, path: string, options?: SQLite3Database.Options, slowQueryThreshold = 100)`

- Opens SQLite via `new SQLite3Database(path, options)` (better-sqlite3 fork)
- Reads `pragma page_size` immediately
- Wraps init errors in `DatabaseInitError`
- Stores `#db`, `#threshold`, `#lc`, `#pageSize` as private fields

**Methods:**
| Method | Signature | Returns | Notes |
|--------|-----------|---------|-------|
| `prepare(sql)` | `string → Statement` | Statement | Wraps in timing + logging |
| `exec(sql)` | `string → void` | void | Direct SQL execution |
| `pragma<T>(sql)` | `string → T[]` | Array of objects | Returns array (e.g., `[{page_size: 4096}]`) |
| `compact(threshold)` | `number → void` | void | Incremental vacuum if freeable > threshold |
| `unsafeMode(unsafe)` | `boolean → void` | void | Passthrough to better-sqlite3 |
| `close()` | `void → void` | void | Runs `pragma optimize` first if not readonly |
| `transaction<T>(fn)` | `() => T → T` | T | Wraps in `#db.transaction(fn)()` |
| `name` | getter | string | Database filename |
| `inTransaction` | getter | boolean | Whether in active transaction |
| `[Symbol.dispose]()` | | void | Calls `close()` |

**Key behavior in `#run<T>(method, sql, fn)`:**

- Times with `performance.now()`
- If `SqliteError`, appends SQL to message: `e.message += ': ${sql}'`
- Always calls `logIfSlow()` in finally block

### Statement class (`db.ts`, lines 171-242)

**Constructor:** `Statement(lc, attrs, stmt: SQLite3Statement, threshold)`

- Binds `scanStatusV2` and `scanStatusReset` from underlying statement

**Methods:**
| Method | Signature | Returns | Notes |
|--------|-----------|---------|-------|
| `safeIntegers(useBigInt)` | `boolean → this` | this | Chainable |
| `run(...params)` | `unknown[] → RunResult` | `{changes: number, lastInsertRowid: number\|bigint}` | |
| `get<T>(...params)` | `unknown[] → T` | Single row or undefined | |
| `all<T>(...params)` | `unknown[] → T[]` | Array of row objects | |
| `iterate<T>(...params)` | `unknown[] → IterableIterator<T>` | Lazy row iterator | Uses `LoggingIterableIterator` |
| `scanStatus` | bound method | | Direct passthrough |
| `scanStatusReset` | bound method | | Direct passthrough |

### LoggingIterableIterator (lines 244-307)

- Wraps `IterableIterator<T>` from better-sqlite3
- Tracks per-row timing (`#sqliteRowTimeSum`)
- On `done` or `return()/throw()`: logs total time and sqlite time separately
- Implements `[Symbol.iterator]()`, `next()`, `return()`, `throw()`

### logIfSlow function (lines 309-322)

- Fires if `elapsed >= threshold`
- Logs via `lc.warn('Slow SQLite query', elapsed)`
- Creates OTel span via `manualSpan(tracer, 'db.slow-query', elapsed, attrs)`

### StatementCache (`packages/zqlite/src/internal/statement-cache.ts`, 131 lines)

**Design:** Pool-based cache (not LRU). Map<string, Statement[]> where each SQL key can have multiple prepared statements.

**Key behaviors:**

- `get(sql)` — normalizes whitespace, pops from array (removes from cache while in use), prepares new if empty
- `return(statement)` — pushes back to array
- `use<T>(sql, cb)` — auto get+return around callback
- `drop(n)` — removes n statements from cache (FIFO from map iteration order)
- `size` getter — total count across all keys
- `normalizeWhitespace(sql)` — `sql.replaceAll(/\s+/g, ' ')`

**Important:** This is NOT an LRU cache. It's a statement pool. Multiple identical SQL strings can coexist for concurrent use.

## 2. Test Coverage Analysis (`db.test.ts`, 183 lines)

Three test cases:

### Test 1: "slow queries are logged" (lines 9-119)

- Uses `vi.useFakeTimers()` — tests rely on mocking `performance.now()`
- Creates DB with threshold=0 (all queries logged)
- Tests: exec, prepare, run, get, all, iterate
- Verifies exact log message structure including context attributes
- **Critical:** Tests the LogContext/logging behavior, NOT just SQLite correctness
- Iterate test: advances timers by 100ms per row, expects total=200ms

### Test 2: "sql errors are annotated with sql" (lines 121-153)

- Verifies error message format: `SqliteError: {message}: {sql}`
- Tests exec, prepare, pragma with bad SQL

### Test 3: "compaction" (lines 155-183)

- Tests `compact()` with incremental vacuum
- Inserts 10 pages of data, deletes, compacts
- Verifies page_count changes

**Key insight:** Tests are tightly coupled to LogContext + OTel behavior. The Rust module doesn't need to reproduce logging — a TS wrapper will handle that. But the tests test the WRAPPER, not raw SQLite. This means:

- Phase 1 Rust module = raw SQLite operations
- Phase 1 TS wrapper = wraps Rust module with LogContext + OTel (matching db.ts behavior)
- db.test.ts tests the TS wrapper

## 3. API Surface & Consumers

**170+ import references** across the monorepo:

- `zqlite` internal: table-source, database-storage, statement-cache, options, query, cost-model, etc.
- `zero-cache`: snapshotter, pipeline-driver, change-processor, write-worker, replication-state, change-log, column-metadata, table-metadata, mutagen, litestream, statz, inspect-handler
- `zql-integration-tests`: various test helpers
- `zql-benchmarks`: benchmark setup

**Exported from `zqlite/src/mod.ts`:** `export {Database} from './db.ts'`

**Types consumed from db.ts:**

- `Database` (class) — used as both value and type
- `Statement` (class) — used as both value and type
- `DatabaseInitError` (class) — used in error handling

## 4. napi-rs Patterns for This Use Case

### Class Export Pattern

```rust
#[napi]
pub struct Database {
    conn: rusqlite::Connection,
}

#[napi]
impl Database {
    #[napi(constructor)]
    pub fn new(path: String) -> napi::Result<Self> { ... }

    #[napi]
    pub fn exec(&self, sql: String) -> napi::Result<()> { ... }

    #[napi]
    pub fn prepare(&self, sql: String) -> napi::Result<Statement> { ... }
}
```

### Statement Lifetime Challenge

rusqlite `Statement` borrows `Connection` — can't return Statement that outlives the method call. Solutions:

1. **Arc<Connection> shared pattern** — Database and Statement both hold Arc<Connection>. Statement uses `conn.prepare_cached(sql)` on each call.
2. **Prepare-on-use pattern** — Statement stores SQL string, re-prepares on each run/get/all call. Uses rusqlite's `CachedStatement` internally.
3. **Raw pointer pattern** — Unsafe, store raw sqlite3_stmt pointer. Not recommended.

**Recommended:** Option 2 (prepare-on-use with rusqlite CachedStatement). Matches the context decision D-03 while being safe. The TS StatementCache already handles cross-call caching.

### Type Coercion (SQLite → JS)

| SQLite Type | JS Type                    | napi-rs            |
| ----------- | -------------------------- | ------------------ |
| NULL        | null                       | `Null` or `JsNull` |
| INTEGER     | number (if fits) or bigint | `Either<i64, f64>` |
| REAL        | number                     | `f64`              |
| TEXT        | string                     | `String`           |
| BLOB        | Buffer                     | `Buffer`           |

**Critical:** better-sqlite3 returns INTEGER as `number` (f64) if value fits in safe integer range (-2^53+1 to 2^53-1), otherwise bigint. Must match this behavior exactly.

### Row Return Pattern

Rows must be returned as JS objects (column_name → value). Use napi-rs `Object`:

```rust
let obj = env.create_object()?;
for (i, col) in columns.iter().enumerate() {
    let val = row.get_ref(i)?;
    obj.set(col, convert_value(env, val)?)?;
}
```

### Iterator Pattern for `iterate()`

napi-rs supports `Generator` via `#[napi(iterator)]` or manual implementation. For row iteration:

- Option A: Return all rows eagerly (defeats purpose of iterate)
- Option B: Use napi-rs `Generator` trait
- Option C: Return a custom iterator class with `next()` method

**Recommended:** Option C — custom class with `next()` returning `{value, done}`.

## 5. rusqlite Patterns

### Opening with Options

```rust
let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
let conn = Connection::open_with_flags(path, flags)?;
conn.pragma_update(None, "journal_mode", "WAL")?;
```

### WAL2 Support

WAL2 is not in stock SQLite. Requires patched SQLite source. With `bundled` feature:

- Override `build.rs` to apply WAL2 patch to bundled SQLite source
- Or use `bundled-sqlcipher` customization pattern

### Transaction Support

```rust
let tx = conn.transaction()?;
// ... operations
tx.commit()?;
```

For the `transaction(fn)` pattern: Rust receives a JS callback, runs it inside transaction:

```rust
#[napi]
pub fn transaction(&self, callback: JsFunction) -> napi::Result<JsUnknown> {
    let tx = self.conn.transaction()?;
    let result = callback.call_without_args(None)?;
    tx.commit()?;
    Ok(result)
}
```

### Pragma

```rust
conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
// For pragma returning rows:
let mut stmt = conn.prepare("PRAGMA page_size")?;
let rows = stmt.query_map([], |row| { ... })?;
```

## 6. Type Coercion Contract (better-sqlite3 compatibility)

better-sqlite3 (and @rocicorp/zero-sqlite3) behavior:

- `INTEGER` → `number` by default, `bigint` if `safeIntegers(true)` called
- `REAL` → `number` (f64)
- `TEXT` → `string`
- `BLOB` → `Buffer` (Node.js)
- `NULL` → `null`

**safeIntegers flag:** Per-statement toggle. When true, all INTEGER values returned as bigint. When false (default), returned as number (may lose precision for >2^53).

**RunResult:** `{changes: number, lastInsertRowid: number|bigint}` — lastInsertRowid follows safeIntegers flag.

## 7. Risk Areas & Edge Cases

1. **Statement lifetime** — rusqlite Statement borrows Connection. Must use prepare-on-use or Arc pattern.
2. **NULL handling** — NULL must map to JS `null`, not `undefined`. napi-rs `Null` type.
3. **INTEGER precision** — Must handle safeIntegers flag per-statement. Default = number (f64 coercion).
4. **Iterator invalidation** — If DB closes while iterator active, must not segfault.
5. **Error message format** — Tests check exact error string format: `SqliteError: {msg}: {sql}`
6. **Logging bridge** — db.test.ts tests LogContext integration. Rust doesn't do logging. TS wrapper must replicate the logging/timing behavior for tests to pass.
7. **unsafeMode** — better-sqlite3 specific feature. May need raw SQLite pragma equivalent.
8. **scanStatusV2/scanStatusReset** — Statement analysis features. May need to be stubbed or implemented via raw SQLite API.
9. **WAL2** — Requires patched SQLite. Complex build.rs setup needed.
10. **performance.now() timing** — Tests use `vi.useFakeTimers()`. TS wrapper must use `performance.now()` (not Rust timing) for test compatibility.

## 8. Recommended Implementation Approach

### Architecture

```
packages/zqlite-rs/
├── Cargo.toml          # napi-rs + rusqlite deps
├── package.json        # npm package with napi binary
├── build.rs            # SQLite build customization (WAL2 patch)
├── src/
│   ├── lib.rs          # napi exports
│   ├── database.rs     # Database class
│   ├── statement.rs    # Statement class
│   ├── row_iterator.rs # IterableIterator impl
│   └── types.rs        # SQLite→JS type conversion
└── __test__/
    └── index.test.ts   # Integration tests
```

### Phase 1 Deliverables

1. **Rust `Database` class** — open, exec, pragma, close, transaction, compact, unsafeMode, name, inTransaction
2. **Rust `Statement` class** — stores SQL + Arc<Connection>, prepare-on-use. run, get, all, safeIntegers
3. **Rust `RowIterator` class** — for Statement.iterate(), with next()/return() support
4. **TS wrapper** (`packages/zqlite/src/db.ts` replacement) — imports from `zqlite-rs`, wraps with LogContext + OTel timing + error augmentation
5. **StatementCache stays in TS** — wraps Rust Database.prepare()

### Plan Split (3 plans as per ROADMAP)

- **Plan 01-01:** Scaffold crate (Cargo.toml, package.json, build.rs, napi config, basic hello-world export to verify build works)
- **Plan 01-02:** Implement Database + Statement + RowIterator in Rust with full SQLite operations
- **Plan 01-03:** TS wrapper that matches db.ts API surface, import path wiring, test verification

## Validation Architecture

### Testability Analysis

- Existing `db.test.ts` (183 lines, 3 tests) is the primary correctness gate
- Tests verify logging behavior, error formatting, and compaction — all mediated through TS wrapper
- Additional Rust unit tests needed for: type coercion, NULL handling, statement lifecycle, edge cases

### Risk-Based Sampling

- **HIGH RISK:** Type coercion (INTEGER → number/bigint), NULL handling, error message format
- **MEDIUM RISK:** Iterator lifecycle (early termination, GC), transaction rollback on error
- **LOW RISK:** Basic CRUD operations, pragma queries

---

_Research completed: 2026-04-20_
_Phase: 01-rust-foundation_
