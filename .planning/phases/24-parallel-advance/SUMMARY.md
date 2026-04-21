# Phase 24 Summary: Parallel Advance (Full Operator Tree)

## What Was Built

Cross-pipeline push propagation through complete IVM operator trees.
When table changes arrive during advance, each pipeline's full operator
chain (Source → Filter → Join → Exists → Take) processes the change
in parallel via Rayon.

## Files Changed

- `packages/zqlite-rs/src/advance.rs` — Added `advance_pipelines_full()`,
  `rust_advance_full()` NAPI, type conversion functions, `FullPipelineConfig`
- `packages/zqlite-rs/src/hydrate.rs` — Added `build_push_operator_chain()`
  (sequential operator tree for push path)

## Type Conversion Chain

```
JS diff Change (HashMap rows)
  → diff_row_to_source_row() → source::Row (serde_json::Map)
  → diff_change_to_source_changes() → SourceChange
  → source_change_to_ivm_change() → IVM Change
  → operator.push() → IVM output Changes
  → ivm_change_to_row_changes() → RowChange (HashMap rows for JS)
```

## Test Results

- Cargo: 139 passed, 0 failed
- pipeline-driver.test.ts: 28 passed, 2 pre-existing failures
