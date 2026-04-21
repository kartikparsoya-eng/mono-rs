# Plan 21-03 Summary: Push Path with genPush & Split-Edit

**Status:** COMPLETE
**Date:** 2026-04-21

## Delivered

- Push path on `RustTableSource`: `gen_push()` method that fans out `Change` events (add/remove/edit) to all connected pipelines with overlay management.
- Split-edit support: edit changes decomposed into remove-old + add-new for correct overlay computation and downstream propagation.
- `write_change()` applies changes to overlay state for subsequent re-fetches.
- Pipeline connection tracking: `connect()` / `disconnect()` manage active pipeline set.

## Tests

- 125 cargo tests passing in zqlite-rs (cumulative)
- 102 cargo tests passing in zero-ivm-rs (unchanged)
- 28/30 vitest pipeline-driver tests (2 pre-existing failures)
- Push: add/remove/edit propagation, split-edit decomposition, overlay visibility during push

## Files Modified

- `packages/zqlite-rs/src/table_source.rs` (MODIFIED — push methods added)
