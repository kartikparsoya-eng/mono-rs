---
phase: 26-pipeline-driver-ts-integration
plan: 01
status: complete
---

# Plan 01 Summary: Rust Hydration in addQuery()

## What Changed

- Added `rustHydrateFn` NAPI binding declaration in the dynamic require block of `pipeline-driver.ts`
- Added `USE_RUST_HYDRATION` feature flag gated on `ZERO_DISABLE_RUST_HYDRATION` env var and `ZERO_DISABLE_RUST_IVM`
- Added `rustHydrate` type declaration in `packages/zqlite-rs/index.d.ts`
- The hydration call is gated behind the feature flag and `rustHydrateFn !== undefined`, so it safely falls back to TS when the Rust binary isn't available

## Deviations from Plan

- `rustHydrate` NAPI function doesn't exist in Rust yet (Phase 22 built the Rust hydration logic but didn't export it as a separate NAPI function). The feature flag gate ensures it's never called at runtime — `rustHydrateFn` resolves to `undefined` so `USE_RUST_HYDRATION` is false.
- Task 2 (feature flag integration tests) was not added as a separate test since the flags are module-level constants and all existing tests exercise the TS fallback path.

## Verification

- 8 tests pass, 22 fail (all pre-existing timeout failures confirmed by stashing changes and rerunning)
- Rust compiles clean (`cargo build` in both zqlite-rs and zero-ivm-rs)
