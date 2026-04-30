---
status: partial
phase: 34-differential-fuzz-schema-extension
source: [34-VERIFICATION.md]
started: 2026-04-30T10:30:00Z
updated: 2026-04-30T10:30:00Z
---

## Current Test

[awaiting human testing]

## Tests

### 1. Live 1k fast-check fuzz against running caches

expected: With TS oracle on :4858, RS cache on :4868, and PG on :6434 running with the FUZZ-02 schema migrated, `cd tools/ivm-parity && FUZZ_NUM_RUNS=1000 npm run fuzz-check:gate` completes in <120s with `unexpected_divergences == 0` and per-FUZZ-02-table hit counts ≥10. Any new divergences become Phase 35 regression tests per CONTEXT D-20.
result: [pending]

### 2. Full `tools/ivm-parity` integration sweep

expected: With caches running, `cd tools/ivm-parity && npm test` exits 0. Aggregate run includes BFS sweep + fast-check fuzz + report generation. PARITY_STATUS.md reflects post-Phase-34 baseline (Track 2 fixes should reduce known divergence count).
result: [pending]

### 3. Schema-typing decision validation

expected: Verify per Zero conventions that `string()` typing for PG TIMESTAMPTZ (replicated as ISO string), JSONB (replicated as JSON string), and NUMERIC (replicated as string for precision) is correct. 34-03 flagged a "schema-typing blocker" but the typing follows Zero conventions (cf. apps/zbugs/shared/schema.ts patterns). The "SchemaVersionNotSupported" error 34-03 saw was likely a runtime cache-restart issue, not a typing bug. Confirm by booting caches with the Phase 34 schema and observing whether queries on `events`/`big_id_records`/`event_tags` succeed.
result: [pending]

## Summary

total: 3
passed: 0
issues: 0
pending: 3
skipped: 0
blocked: 0

## Gaps
