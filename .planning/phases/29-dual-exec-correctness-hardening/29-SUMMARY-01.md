---
phase: 29
plan: 1
status: complete
---

# Summary: Plan 29.1 — Dual-Exec Hydration Comparison

## What Was Built

Added `#dualExecHydrate()` method to `pipeline-driver.ts` (~lines 625-670). When `ZERO_DUAL_EXEC` is enabled and `USE_RUST_HYDRATION` is true, runs both Rust and TS hydration paths, compares results via `dualExecCompare()`, and yields TS as truth. Wired into `addQuery` branching logic.

## Key Files

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — added `#dualExecHydrate()` method

## Verification

- 35 tests pass (normal mode)
- 35 tests pass (ZERO_DUAL_EXEC=strict mode)
- 1 pre-existing failure ("push fails on out of bounds numbers" — upstream bug)
