# Phase 5: High-Impact Services - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 05-high-impact-services
**Areas discussed:** ChangeProcessor scope, Snapshotter diff boundary, fromSQLiteTypes location, allBuf usage in Snapshotter

---

## ChangeProcessor Scope

| Option               | Description                                                                             | Selected |
| -------------------- | --------------------------------------------------------------------------------------- | -------- |
| Skip entirely (D-15) | D-13 applies — it's all writes + DDL. Phase 1 Rust Database already handles SQLite I/O. | ✓        |
| Backfill loop only   | Keep processBackfill's inner loop in Rust (bulk read + conditional writes).             |          |
| Full Rust rewrite    | Move entire TransactionProcessor to Rust — one FFI call per commit.                     |          |

**User's choice:** Skip entirely. D-13 applies cleanly — 95% writes, single-row ops already efficient via Phase 1 Rust Database. processBackfill is a cold catch-up path, not steady-state.
**Notes:** If backfill performance surfaces in profiling, address with queryBatched pattern.

---

## Snapshotter Diff Boundary

| Option                     | Description                                                                                     | Selected |
| -------------------------- | ----------------------------------------------------------------------------------------------- | -------- |
| Full diff in Rust          | Single function: advance(prevVersion, tableSpecs) → Buffer of all Changes. Zero per-change FFI. |          |
| Individual methods in Rust | Rust exposes getRow/getRows/changesSince as optimized methods. TS still iterates change log.    | ✓        |
| Batched diff chunks        | Rust produces batch of N change-log entries + resolved rows in one call.                        |          |

**User's choice:** Individual methods in Rust
**Notes:** Simpler, lower risk, still eliminates per-row JS object construction overhead.

---

## fromSQLiteTypes Location

| Option                  | Description                                                                                     | Selected |
| ----------------------- | ----------------------------------------------------------------------------------------------- | -------- |
| Inline in Rust methods  | getRow/getRows accept column type metadata and return converted rows. Same pattern as queryAll. | ✓        |
| Keep in TS (status quo) | Rust returns raw SQLite values. TS fromSQLiteTypes converts after.                              |          |

**User's choice:** Inline in Rust methods. Consistency across all read methods. D-12 deferral applies to the standalone TS export — internal Rust usage proceeds now.
**Notes:** Eliminates per-row fromSQLiteTypes call in the diff hot path.

---

## allBuf Usage in Snapshotter

| Option                                   | Description                                                                              | Selected |
| ---------------------------------------- | ---------------------------------------------------------------------------------------- | -------- |
| Direct objects (no allBuf)               | getRow/getRows small results, use direct napi return. changesSince uses iterate().       |          |
| Mixed: allBuf for getRows + changesSince | allBuf where results are unbounded, direct object for single-row.                        | ✓        |
| Purpose-built Rust methods               | New dedicated methods combining query + type conversion. Not allBuf, not generic .all(). |          |

**User's choice:** Mixed protocol based on result size. getRow: direct napi object. getRows: allBuf. changesSince: allBuf batches of 500.
**Notes:** Purpose-built methods rejected — adds API surface without protocol benefit.

---

## Claude's Discretion

- Rust method signatures and parameter encoding
- Whether getRow/getRows are Database methods or separate module
- changesSince batch iteration API design
- safeIntegers threshold communication

## Deferred Ideas

- Full diff in Rust — revisit if individual methods prove insufficient
- ChangeProcessor backfill optimization — only if profiling shows need
- fromSQLiteTypes TS export removal — after all consumers migrated
