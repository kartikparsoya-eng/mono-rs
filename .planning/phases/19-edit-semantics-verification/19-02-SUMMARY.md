---
phase: 19
plan: 2
status: complete
---

# Summary: Vitest Integration Tests + Prev-Snapshot Bug Fix

## Bug Fixed

Discovered and fixed a fundamental architectural bug in `rust_advance()`: it opened two fresh SQLite connections to the same DB file, which in WAL mode both see identical committed state. This meant `prev_values` and `next_value` were identical for edits, making filter-boundary detection impossible.

**Fix (Option 3):** Replaced `rust_advance()` with `rust_fan_out()` architecture:

- TS performs the diff using `snapshotter.advance()` (correct two-snapshot isolation)
- Serializes collected changes as JSON
- Passes to new `rust_fan_out(changesJson, pipelineConfigsJson)` napi function
- Rust does only Rayon `par_iter` fan-out over pipelines (preserves parallelism benefit)

## Files Modified

- `packages/zqlite-rs/src/advance.rs` - Added `rust_fan_out` napi function + 6 edit tests
- `packages/zqlite-rs/src/diff.rs` - Added `Deserialize` + `#[serde(rename_all = "camelCase")]` to `Change` struct
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` - Rewrote `#rustAdvance()` to use TS diff + `rustFanOut`
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.edit-semantics.test.ts` - 4 new tests

## Verification

- 4/4 edit-semantics vitest pass with Rust enabled
- 29/30 main pipeline-driver tests pass (1 pre-existing failure: "push fails on out of bounds numbers")
- Previous "unique constraint" Rust-only failure is now FIXED (29 pass vs previous 28)
- 81 cargo tests pass
