# Phase 24: Parallel Advance (Full Operator Tree)

## Goal

Enable cross-pipeline push propagation through complete operator trees during advance.
When a table change arrives, fan it out through each pipeline's full operator chain
(Join, Exists, Take, Filter) in parallel using Rayon.

## Implementation

1. **`diff_row_to_source_row()`** — converts `diff::Row` (HashMap) to `source::Row` (serde_json::Map)
2. **`diff_change_to_source_changes()`** — converts diff Changes to SourceChange variants with PK matching
3. **`source_change_to_ivm_change()`** — converts SourceChange to IVM Change for operator push
4. **`ivm_change_to_row_changes()`** — converts IVM output Changes back to RowChange for JS
5. **`process_full_pipeline()`** — creates RustTableSource, builds push operator chain, pushes changes
6. **`advance_pipelines_full()`** — parallel Rayon fan-out across all pipelines
7. **`rust_advance_full()`** — NAPI entry point (JSON in/out)
8. **`FullPipelineConfig`** — deserialization struct for pipeline configs with operator_config

## Key Design Decision

Three Row types exist in the codebase:

- `diff::Row` = `HashMap<String, serde_json::Value>` (from JS diff protocol)
- `source::Row` = `serde_json::Map<String, serde_json::Value>` (internal source layer)
- `zero_ivm_rs::types::Row` = `serde_json::Map<String, serde_json::Value>` (IVM operators)

source::Row and IVM Row are identical. diff::Row requires conversion via `diff_row_to_source_row()`.

## Verification

- `cargo test` — 139 passed, 0 failed
- pipeline-driver.test.ts — 28 passed, 2 pre-existing failures
