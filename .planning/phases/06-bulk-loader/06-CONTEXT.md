# Phase 6: Bulk Loader - Context

**Gathered:** 2026-04-20
**Status:** Ready for planning

<domain>
## Phase Boundary

Replace the initial-sync SQLite INSERT path with a Rust bulk-insert method IF benchmarking shows napi crossing overhead is significant (>5% of total sync time). TransactionPool orchestration stays TS.

</domain>

<decisions>
## Implementation Decisions

### D-19: Benchmark-gated implementation

- Phase 6 is conditional on benchmark results
- Measure napi crossing overhead as % of total initial-sync time
- Decision rule:
  - **<5%:** Skip Phase 6 entirely, D-13 holds (write path stays TS)
  - **5-15%:** Implement Rust bulk INSERT for flush() loop only (one napi crossing per flush instead of N/50)
  - **>15%:** Full Rust bulk INSERT method (accept flat values buffer + schema, all INSERTs internal)
- This decision rule to be documented in CLAUDE.md

### D-20: TransactionPool stays TS

- Parallel worker orchestration is coordination logic, not compute
- Only the flush() SQLite INSERT hot loop is a candidate for Rust

### D-21: Benchmark methodology

- Must measure actual initial-sync with representative data (not micro-benchmark)
- Isolate: time in flush() vs time in PG COPY network + decode
- Use existing `initial-sync-bench.pg.test.ts` as basis

### Prior decisions that apply

- D-13: Write path stays TS (may be partially overridden by D-19 rule)
- Phase 1: Statement.run() already in Rust — baseline is established

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Initial Sync

- `packages/zero-cache/src/services/change-source/pg/initial-sync.ts` — Main sync logic (1161 lines), flush() at line 943
- `packages/zero-cache/src/db/transaction-pool.ts` — Worker pool orchestration (863 lines)
- `packages/zero-cache/src/db/initial-sync-bench.pg.test.ts` — Existing benchmark test

### Tests

- `packages/zero-cache/src/services/change-source/pg/initial-sync.test.ts` — Unit tests
- `packages/zero-cache/src/services/change-source/pg/initial-sync.pg.test.ts` — Integration tests (requires PG)

</canonical_refs>

<code_context>

## Existing Code Insights

### Hot Path (flush function, line 943-967)

- Buffers rows in `pendingValues[]` array (up to MAX_BUFFERED_ROWS × valuesPerRow)
- Fires batched INSERT via `insertBatchStmt.run(pendingValues.slice(...))` — batch of 50 rows
- Remainder rows go through single `insertStmt.run()` calls
- Already calls Rust Statement.run() from Phase 1

### Data Flow

1. PG COPY (binary/text) → stream parser → decoded values
2. Values buffered in TS array (pendingValues)
3. flush() → N/50 calls to `Statement.run(values_slice)` → Rust SQLite INSERT
4. Each napi crossing: TS array → Rust param extraction → sqlite3_step

### Integration Points

- `Database` from `packages/zqlite/src/db.ts` (already Rust-backed)
- `Statement.run()` from Phase 1 Rust implementation
- `TransactionPool` manages concurrent BEGIN CONCURRENT transactions

### INSERT_BATCH_SIZE = 50

- Empirically tuned — batches of 50 rows per INSERT statement
- Multi-value INSERT: `INSERT INTO t (cols) VALUES (?,...),(?,...),... × 50`

</code_context>

<specifics>
## Specific Ideas

- Potential Rust bulk INSERT method signature: `db.bulkInsert(sql_template, flat_values, values_per_row, batch_size)`
- Single napi crossing processes entire flush buffer internally
- Similar pattern to `getRowsMultiBuf` but for writes

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

_Phase: 06-bulk-loader_
_Context gathered: 2026-04-20_
