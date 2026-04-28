# Phase 5: High-Impact Services - Research

**Completed:** 2026-04-20
**Scope:** Snapshotter read-path optimization via Rust methods (D-15 skips ChangeProcessor)

## Key Findings

### 1. Snapshotter Architecture (snapshotter.ts — 590 lines)

**Classes:**

- `Snapshotter` — manages two leapfrogging `Snapshot` connections
- `Snapshot` — wraps `StatementRunner`, holds BEGIN CONCURRENT transaction
- `Diff` — implements `SnapshotDiff` / `Iterable<Change>`, the hot-path iterator

**Hot-path flow (`Diff[Symbol.iterator]`):**

1. `curr.changesSince(prevVersion)` — iterates change log entries
2. For each entry: `v.parse(value, schema)` validates the row
3. If SET_OP: `curr.getRow(tableSpec, rowKey)` → get current value
4. If SET_OP with nextValue: `prev.getRows(tableSpec, uniqueKeys, nextValue)` → find conflicting rows
5. If DEL: `prev.getRow(tableSpec, rowKey)` → get prev value
6. Apply `fromSQLiteTypes(zqlSpec, row, table)` to each result row
7. Yield `Change` object

### 2. Target Methods — Current TS Implementation

#### `Snapshot.getRow(table, rowKey)` (line 339)

```typescript
getRow(table: LiteTableSpecWithKeysAndVersion, rowKey: JSONValue) {
  const key = normalizedKeyOrder(rowKey as RowKey);
  const conds = Object.keys(key).map(c => `${id(c)}=?`);
  const cols = Object.keys(table.columns);
  // Builds: SELECT col1,col2,... FROM tableName WHERE k1=? AND k2=?
  // Uses statementCache.get() + .get<any>(Object.values(key))
  // safeIntegers(true) enabled
}
```

Returns: single row object or undefined.

#### `Snapshot.getRows(table, keys, row)` (line 357)

```typescript
getRows(table: LiteTableSpecWithKeysAndVersion, keys: PrimaryKey[], row: RowValue) {
  // Filter out keys where any column is NULL (320x perf fix)
  const validKeys = keys.filter(key => key.every(column => row[column] !== null && row[column] !== undefined));
  // Builds: SELECT cols FROM table WHERE (k1=? AND k2=?) OR (k3=? AND k4=?) ...
  // Uses statementCache.get() + .all<any>(flatMap of values)
  // safeIntegers(true) enabled
}
```

Returns: array of row objects (usually 0-3 rows for unique constraint checks).

#### `Snapshot.changesSince(prevVersion)` (line 326)

```typescript
changesSince(prevVersion: string) {
  // SELECT stateVersion, table, rowKey, op FROM _zero.changeLog2
  //   WHERE stateVersion > ? ORDER BY stateVersion ASC, pos ASC
  // Returns { changes: iterator, cleanup: function }
  // Uses statementCache + .iterate(prevVersion)
}
```

Returns: iterator of change log entries + cleanup function.

### 3. Type System Integration

**`LiteTableSpecWithKeysAndVersion`** contains:

- `name: string` — table name
- `columns: Record<string, {dataType: string}>` — column definitions
- `primaryKey: string[]`
- `uniqueKeys: PrimaryKey[]` (array of column-name arrays)
- `minRowVersion?: string`

**`LiteAndZqlSpec`** contains:

- `tableSpec: LiteTableSpecWithKeysAndVersion` — for SQL generation
- `zqlSpec: Record<string, SchemaValue>` — for `fromSQLiteTypes` type conversion

**`fromSQLiteTypes`** converts:

- `boolean` → `!!v` (SQLite int → JS boolean)
- `json` → `JSON.parse(v)` (SQLite text → JS object)
- `number/string/null` → BigInt→Number coercion if needed
- Operates per-column using `zqlSpec` type info

### 4. Design Decisions for Rust Methods

**Per D-16/D-17/D-18:**

| Method       | Protocol               | Type Conversion                                        | Statement Cache             |
| ------------ | ---------------------- | ------------------------------------------------------ | --------------------------- |
| getRow       | Direct napi (0-1 rows) | Inline in Rust                                         | TS statementCache unchanged |
| getRows      | allBuf                 | Inline in Rust (via decodeBuf + fromSQLiteTypes in TS) | TS statementCache unchanged |
| changesSince | allBuf batches of 500  | No conversion (raw strings)                            | TS statementCache unchanged |

**Critical insight:** The statement cache stays in TS (D-08). Rust methods will NOT use the statement cache. Instead, they operate directly on the underlying Rust Database connection. This means:

- Rust methods accept the SQL string and params directly
- OR Rust methods accept table spec info and generate SQL internally
- The latter is preferred (D-16: "Rust exposes getRow, getRows, changesSince as methods that accept column type metadata")

### 5. Implementation Approach

**Option A (Chosen per D-16):** Add methods to `Database` class in Rust:

- `Database.getRow(sql, params, columnTypes, tableName)` → returns typed JS object or undefined
- `Database.getRows(sql, params, columnTypes, tableName)` → returns Buffer (allBuf protocol)
- `Database.changesSinceBatch(sql, params, batchSize)` → returns Buffer (allBuf protocol, raw — no type conversion)

**Why Database-level, not Statement-level:**

- Avoids statement cache complexity in Rust
- TS caller generates SQL (using existing `id()` quoting and column logic)
- Rust does: prepare → bind → execute → serialize result
- This mirrors `queryAll` pattern exactly

**Alternative considered:** Making Rust generate SQL from table spec. Rejected — too much logic duplication, and the SQL generation is cheap (string concat, not in hot path relative to I/O).

### 6. fromSQLiteTypes in Rust vs TS

Per D-17, type conversion is inline in Rust for getRow/getRows. This means:

- `getRow` needs `columnTypes: Record<string, string>` (same as queryAll)
- Returns already-converted object (boolean fields are true/false, json fields are parsed)
- `getRows` uses allBuf → TS decodes buffer → TS calls fromSQLiteTypes on decoded rows

**Wait — re-reading D-17 and D-18:**

- D-17 says "Type conversion happens inline in Rust read methods"
- D-18 says "getRows: allBuf protocol"

These are compatible: Rust can do type conversion BEFORE serializing to the buffer. The buffer would contain already-converted values (booleans as 0/1 integers mapping to JS true/false, JSON as parsed... no, JSON can't go through binary buffer easily).

**Resolution:** For allBuf paths (getRows, changesSince):

- Rust serializes raw SQLite values to buffer
- TS decodes buffer, then calls fromSQLiteTypes on each row
- This is the same pattern as existing `allBuf` + `decodeBuf`

For direct napi path (getRow):

- Rust does full type conversion inline (same as queryAll)
- Returns a ready-to-use JS object

### 7. Risks & Mitigations

1. **Statement cache bypass:** Rust methods prepare statements fresh each call. For the Diff iterator that calls getRow/getRows hundreds of times with the SAME SQL, this could be slower than cached TS.
   - **Mitigation:** Rust can maintain its own internal prepared statement (same SQL used repeatedly within a batch). OR accept that SQLite's own statement cache handles this.
   - **Better:** Since getRow/getRows SQL is deterministic per-table, we can cache the prepared statement in Rust's Database for the duration of usage.

2. **BEGIN CONCURRENT compatibility:** Rust methods must work within an already-open BEGIN CONCURRENT transaction started by TS.
   - **Confirmed safe:** Rust Database shares the same `Connection` (Arc<RefCell<Connection>>). The TS StatementRunner calls `beginConcurrent()` which runs `BEGIN CONCURRENT` on the same connection. Subsequent Rust queries on that connection will see the snapshot.

3. **NULL filtering for getRows (320x fix):** The NULL key filtering logic (lines 368-373) stays in TS per the context. Rust method receives already-filtered params.

### 8. Validation Architecture

**Test gate:** `snapshotter.test.ts` must pass unchanged.

**Verification approach:**

1. Rust unit tests for getRow/getRows buffer serialization
2. Integration: swap Snapshot.getRow/getRows/changesSince to call Rust Database methods
3. Run `snapshotter.test.ts` — must pass as-is
4. Benchmark: compare per-iteration time of Diff with TS vs Rust methods

## RESEARCH COMPLETE
