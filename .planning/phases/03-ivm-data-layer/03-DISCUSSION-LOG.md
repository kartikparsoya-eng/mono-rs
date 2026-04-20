# Phase 3: IVM Data Layer - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 03-ivm-data-layer
**Areas discussed:** Rust Boundary, DatabaseStorage Scope, Type Converter Location, Batch API Shape

---

## Rust Boundary

| Option | Description | Selected |
|--------|-------------|----------|
| Hot-path only | Keep TableSource in TS. Move fromSQLiteTypes/row-iteration to Rust. Generators stay TS. Safest. | ✓ |
| Full class in Rust | Entire TableSource in Rust including SQL gen, generators. High risk. | |
| Hybrid: fetch+write in Rust | Move #fetch and #writeChange to Rust. Medium complexity. | |

**User's choice:** Hot-path only
**Notes:** Tests pass easily since class structure unchanged. Generators/overlay/connect stay TS.

---

## DatabaseStorage Scope

| Option | Description | Selected |
|--------|-------------|----------|
| Full Rust class | Entire DatabaseStorage in Rust with TS wrapper. | |
| Keep TS, Rust helpers | Stays TS, benefits from Phase 1 Rust Database underneath. | ✓ |

**User's choice:** Keep TS, Rust helpers
**Notes:** Same reasoning as D-08 (StatementRunner). Only 187 lines, not worth the rewrite.

---

## Type Converter Location

| Option | Description | Selected |
|--------|-------------|----------|
| Rust in zqlite-rs | Move to Rust, export via napi. Single source of truth. | |
| Keep TS, optimize later | Keep as TS exports. Used by many modules not yet in scope. | ✓ |

**User's choice:** Keep TS for now. Move to Rust in Phase 5 when snapshotter and change-processor are being converted. Moving now creates FFI calls in code about to be rewritten.

---

## Batch API Shape

| Option | Description | Selected |
|--------|-------------|----------|
| Query → JS array | Rust fn returns full result as JS array. One FFI call. | |
| Streaming cursor | Lazy iterator, preserves streaming but more FFI crossings. | |
| Both modes | Array for small, cursor for large. | ✓ |

**User's choice:** Both modes with explicit threshold. `queryAll(sql, params)` for small results, `queryBatched(sql, params, batchSize=500)` for streaming. One FFI call per 500 rows. Batch size tunable.

---

## Claude's Discretion

- Internal Rust function signatures and parameter encoding
- Batch size default, column type metadata format
- Rust unit test structure

## Deferred Ideas

- fromSQLiteTypes/toSQLiteTypes in Rust → Phase 5
- Write path (#writeChange) in Rust → not a bottleneck
- Full TableSource in Rust → too complex for generator interop
