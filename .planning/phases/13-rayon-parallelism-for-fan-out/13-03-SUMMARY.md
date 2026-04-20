---
plan_id: "13-03"
phase: 13
title: "TS Integration + Fallback"
status: complete
---

# Plan 13-03 Summary: TS Integration + Fallback

## What was done

Wired `pipeline-driver.ts` to call `rust_advance()` for filter-only pipelines with automatic fallback to the existing TS advance path.

### Changes

- **`packages/zero-cache/src/services/view-syncer/pipeline-driver.ts`** (~160 lines added):
  - Added `RustPipelineConfig` / `RustOperator` types matching Rust `PipelineConfig` struct
  - Dynamic `require('zqlite-rs')` with try/catch for `rustAdvance` binding
  - `USE_RUST_ADVANCE` constant gated on `USE_RUST_IVM` and binding availability
  - `#pipelineConfigs` Map tracks pipeline topology per query ID
  - `#useRustAdvance` flag: true only when ALL pipelines are filter-only (no joins, exists, take, scalar subqueries)
  - `#extractPipelineConfig()` inspects AST to determine Rust eligibility
  - `#conditionHasCorrelatedSubquery()` recursive check for exists in WHERE clauses
  - `#reevaluateRustAdvance()` called on addQuery/removeQuery/reset
  - `#rustAdvance()` calls `rustAdvanceFn()` with serialized pipeline configs
  - Version mismatch retry (one retry with fresh snapshots)
  - ResetPipelinesSignal on reset/truncate error types from Rust
  - Fallback: on any unexpected error, logs warning and disables Rust path for the driver instance
  - `#convertRustChanges()` generator maps Rust JSON results to RowChange stream
  - `reset()` clears pipeline configs and disables Rust advance

### Fallback behavior

1. **Unsupported pipelines**: Any pipeline with joins, exists, take, or scalar subqueries → TS path (automatic, #useRustAdvance stays false)
2. **Rust error**: catch → log warning → set #useRustAdvance=false → fall through to TS path
3. **ResetPipelinesSignal**: re-thrown (not caught by fallback)
4. **ZERO_DISABLE_RUST_IVM=1**: all Rust paths disabled at module level

## Verification

- Rust tests: 55 (zqlite-rs) + 58 (zero-ivm-rs) = 113 passing
- pipeline-driver.test.ts: 29/30 passing (1 pre-existing failure: "push fails on out of bounds numbers")
- Operator regression: 110/110 passing (filter, exists, take.push)
