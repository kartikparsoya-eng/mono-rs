# Plan 21-02 Summary: TableSource Fetch with Overlay Integration

**Status:** COMPLETE
**Date:** 2026-04-21

## Delivered

- `table_source.rs` — `RustTableSource` struct with `fetch()` method that builds SQL via query_builder, executes against connection pool, streams `Node` results. Supports filter pushdown and sort ordering.
- `overlay.rs` — `compute_overlays()` merges pending changes into overlay map. `generate_with_overlay()` and `generate_with_overlay_unordered()` splice overlay adds/removes into sorted fetch streams for self-join correctness.
- Overlay integration: `set_overlay` / `get_overlay` on TableSource for push-path re-fetch visibility.

## Tests

- 114 cargo tests passing in zqlite-rs after this plan
- TableSource fetch: basic query, with constraints, ordering, start bounds
- Overlay: compute from changes, sorted merge, unordered merge, empty overlay

## Files Created/Modified

- `packages/zqlite-rs/src/table_source.rs` (NEW)
- `packages/zqlite-rs/src/overlay.rs` (NEW)
- `packages/zqlite-rs/src/lib.rs` (MODIFIED)
