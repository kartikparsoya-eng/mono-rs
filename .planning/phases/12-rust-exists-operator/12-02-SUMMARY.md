# Plan 12-02 Summary: TS Integration + Delegate Wiring

## What was built

Created `rust-exists.ts` wrapper module and wired Rust exists acceleration into
`pipeline-driver.ts` via the `decorateFilterInput` delegate method.

## Key files

### Created

- `packages/zero-cache/src/services/view-syncer/rust-exists.ts` — TS wrapper with
  `isRustExistsAvailable()` and `createRustExistsWrapper()`. Follows `rust-join.ts`
  pattern: conditional require, graceful fallback, FilterOperator wrapper that
  intercepts CHILD add/remove push() for Rust-accelerated decisions.

### Modified

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — Added
  `USE_RUST_EXISTS` constant, `RUST_EXISTS_NAME_RE` regex, and Rust exists wrapping
  in the `decorateFilterInput` callback. Logs availability on init.

## Architecture

The wrapper intercepts `push()` for CHILD add/remove changes matching the target
relationship. It pre-computes `fetchSize` (TS generator, D-50), calls
`rustExistsPushBatch` for the decision, and applies the action:

- `convert_add` / `convert_remove`: bypass filter, push directly to output
- `pass_filter_with_exists`: apply pre-resolved exists value inline
- `pass_filter` / fallback: delegate to original Exists.push()

ADD, EDIT, REMOVE changes always delegate to the original (filter logic requires
TS generators for fetchExists).

## Limitations

- EXISTS type detection: `decorateFilterInput` name string doesn't encode
  EXISTS vs NOT EXISTS. Currently defaults to EXISTS. NOT EXISTS queries are
  not exercised in pipeline-driver tests. Full NOT EXISTS support requires
  D-61 (createExists delegate method).

## Test results

- `exists.test.ts`: 2/2 passed
- `pipeline-driver.test.ts`: 29/30 passed (1 pre-existing failure: "push fails on out of bounds numbers")
- `filter.test.ts`: 6/6 passed
- No test files modified (D-35)
- No packages/zql/ files modified (D-44)
