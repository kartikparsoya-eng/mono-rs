# Rust IVM Architecture

## Overview

Phases 10-13 ported the IVM (Incremental View Maintenance) hot path from
TypeScript to Rust via **napi-rs**. The goal was to move the most
CPU-intensive parts of pipeline advancement -- diff reading, predicate
evaluation, and per-pipeline fan-out -- into native code while keeping the
rest of the view-syncer in TypeScript.

## Components

### zqlite-rs

Native napi module (`packages/zqlite-rs`) compiled as a cdylib.

| File | Responsibility |
|------|---------------|
| `src/advance.rs` | `rust_advance` napi entry point. Deserializes pipeline configs, opens two read-only SQLite connections (prev/curr snapshots), reads the diff, fans out over pipelines with Rayon, and serializes the result as JSON. Also contains the full predicate AST and evaluation engine (reimplemented from zero-ivm-rs because cdylib cannot cross-link). |
| `src/diff.rs` | Reads `_zero.changeLog2` in batches, fetches prev/curr rows by primary and unique keys, converts SQLite types to ZQL types (boolean 0/1 to bool, JSON strings to parsed values), and surfaces reset/truncate/unknown errors. |
| `src/database.rs` | Lower-level SQLite helpers shared across the crate. |

### zero-ivm-rs

Pure-Rust operator library (`packages/zero-ivm-rs`) providing:

- **filter** -- predicate evaluation (eq, neq, gt, gte, lt, lte, in, like, isNull, isNotNull, and/or/not)
- **take** -- sort + limit with `RustTakeStorage`
- **join** -- parent/child key correlation (exposed via `isRustJoinAvailable`)
- **exists** -- semi-join existence check (exposed via `isRustExistsAvailable` / `createRustExistsWrapper`)

### TypeScript Wrappers

| File | Role |
|------|------|
| `pipeline-driver.ts` | Orchestrates Rust vs TS advancement. Builds `RustPipelineConfig` for each eligible query, calls `rust_advance` when all pipelines qualify, converts the JSON result back to `RowChange` objects. Falls back to TS on any error. |
| `rust-join.ts` | Checks availability and wraps the Rust join operator. |
| `rust-exists.ts` | Checks availability and wraps the Rust exists operator via `createRustExistsWrapper`. |
| `zero-ivm-rs/ts/rust-take-storage.ts` | Adapts `RustStorage` to the IVM `Storage` interface for take operators. |

## Data Flow

```
replicator
  --> _zero.changeLog2 (in SQLite WAL2 replica)
        --> diff reader (diff.rs: read_changelog_entries + get_row/get_rows)
              --> Vec<Change>  (prev_values, next_value, row_key per changed row)
                    --> Arc<Vec<Change>> shared across Rayon thread pool
                          --> par_iter over PipelineConfig[]  (advance.rs)
                                --> process_pipeline: filter, classify add/edit/remove
                                      --> Vec<RowChange> per pipeline
                                            --> collect + JSON serialize
                                                  --> napi return to TS
                                                        --> pipeline-driver.ts converts to RowChange stream
```

Key detail: `rust_advance` opens **two** read-only SQLite connections with
`BEGIN DEFERRED` for snapshot isolation -- one pinned to `prev_version` and
one to `curr_version`. The diff reader walks the changelog on the curr
connection and looks up rows on both connections.

## Operator Boundary

| Layer | Runs in |
|-------|---------|
| Changelog reading, row fetching, type conversion | Rust (diff.rs) |
| Predicate parsing and evaluation (filter) | Rust (advance.rs) |
| Pipeline fan-out (Rayon `par_iter`) | Rust (advance.rs) |
| Take storage (sort + limit state) | Rust (RustTakeStorage) |
| Join correlation | Rust (when `isRustJoinAvailable`) |
| Exists semi-join | Rust (when `isRustExistsAvailable`) |
| Pipeline eligibility check | TypeScript (pipeline-driver.ts `#extractPipelineConfig`) |
| Hydration (initial query fetch) | TypeScript (always) |
| Scalar subquery resolution | TypeScript (always) |
| Companion pipeline monitoring | TypeScript (always) |
| Result streaming to client | TypeScript (always) |

A pipeline is eligible for Rust advancement only when it has no `related`
clauses, no `limit`, no companion subqueries, and no correlated subqueries
in its `where` condition. Rust advancement is used only when **all**
pipelines in the client group are eligible.

## Fallback Mechanisms

The system is designed to degrade gracefully at every level:

| Trigger | Behavior |
|---------|----------|
| `ZERO_DISABLE_RUST_IVM=1` env var | All Rust paths disabled at startup; pure TS pipeline used. |
| `zqlite-rs` native module not built / `require('zqlite-rs')` fails | `rustAdvanceFn` remains `undefined`; `USE_RUST_ADVANCE` is false. |
| Any pipeline is ineligible (related, limit, companions, correlated subquery) | `#reevaluateRustAdvance` sets `#useRustAdvance = false`; all pipelines advance via TS. |
| Rust `rust_advance` throws at runtime | Caught in `advance()`, logs warning, sets `#useRustAdvance = false` for remainder of session, retries current advance via TS. |
| `version_mismatch` error type in result JSON | Retries once with a fresh snapshot pair; if retry also fails, throws `ResetPipelinesSignal`. |
| `reset` or `truncate` error type (schema change, table truncation) | Throws `ResetPipelinesSignal` -- view-syncer tears down and re-hydrates all pipelines. |
| `unknown` error type | Propagated as a generic `Error`; caught by the outer fallback. |
