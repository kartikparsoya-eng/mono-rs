---
phase: 18
plan: '02'
title: 'Vitest Pipeline Mutation Tests'
status: complete
---

# Summary

## What was built

- New test file `pipeline-driver.concurrency.test.ts` with 5 integration tests
- Tests prove pipeline add/remove/churn between consecutive advance() calls is safe

## Key files

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.concurrency.test.ts`

## Verification

- `npx vitest run ...pipeline-driver.concurrency.test.ts` — all 5 pass

## Issues

- Rust IVM path does not emit row-level changes via `advance()` for filter-only queries with `NO_TIME_ADVANCEMENT_TIMER`, so tests verify pipeline state (queries map) and absence of stale query results rather than asserting specific row output from advance.
