---
phase: 29
plan: 3
status: complete
---

# Summary: Plan 29.3 — Missing Test Scenarios

## What Was Built

Added 6 new tests to `pipeline-driver.test.ts` covering previously untested scenarios:

1. **Take/Limit hydration** — query with `limit: 2`, verifies hydration returns limited rows
2. **Take/Limit advance (insert)** — new row enters top-N window
3. **Take/Limit advance (delete)** — top-N row deleted, next row promoted
4. **NOT EXISTS hydration** — correlated subquery returns issues without comments
5. **NOT EXISTS advance (add)** — comment added removes issue from results
6. **NOT EXISTS advance (delete)** — all comments deleted adds issue back

## Key Files

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — 6 new tests appended

## Verification

- All 6 new tests pass in normal mode
- All 6 new tests pass in ZERO_DUAL_EXEC=strict mode
- Total: 35 pass, 1 pre-existing fail
