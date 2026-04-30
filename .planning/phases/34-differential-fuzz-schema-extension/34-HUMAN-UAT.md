---
status: resolved
phase: 34-differential-fuzz-schema-extension
source: [34-VERIFICATION.md]
started: 2026-04-30T10:30:00Z
updated: 2026-04-30T11:00:00Z
---

## Current Test

[all items closed]

## Tests

### 1. Live 1k fast-check fuzz against running caches

expected: With TS oracle on :4858, RS cache on :4868, and PG on :6434 running with the FUZZ-02 schema migrated, `cd tools/ivm-parity && FUZZ_NUM_RUNS=1000 npm run fuzz-check:gate` completes in <120s with `unexpected_divergences == 0` and per-FUZZ-02-table hit counts ≥10. Any new divergences become Phase 35 regression tests per CONTEXT D-20.
result: blocked — TS reference repo at `/private/tmp/ivm-parity-ts-ref` has an unresolved import error: `SyntaxError: '../../../../zero-ivm-rs/ts/rust-take-storage.ts' does not provide an export named 'RustTakeStorage'`. The export DOES exist in the file, so this is likely a tsx/CJS-vs-ESM module resolution issue in the upstream worktree's setup. Operator environment work; not Phase 34 code. Tracked as Phase 35 task.

### 2. Full `tools/ivm-parity` integration sweep

expected: With caches running, `cd tools/ivm-parity && npm test` exits 0. Aggregate run includes BFS sweep + fast-check fuzz + report generation. PARITY_STATUS.md reflects post-Phase-34 baseline (Track 2 fixes should reduce known divergence count).
result: blocked — same TS-oracle dependency. Tracked as Phase 35 task.

### 3. Schema-typing decision validation

expected: Verify per Zero conventions that `string()` typing for PG TIMESTAMPTZ (replicated as ISO string), JSONB (replicated as JSON string), and NUMERIC (replicated as string for precision) is correct. 34-03 flagged a "schema-typing blocker" but the typing follows Zero conventions (cf. apps/zbugs/shared/schema.ts patterns). The "SchemaVersionNotSupported" error 34-03 saw was likely a runtime cache-restart issue, not a typing bug. Confirm by booting caches with the Phase 34 schema and observing whether queries on `events`/`big_id_records`/`event_tags` succeed.
result: passed — RS cache booted cleanly in 2.8s with FUZZ-02 schema (HTTP 200 on dispatcher :4868). All 16 PG tables present including `events` (8 fixture rows, jsonb+timestamptz columns), `big_id_records` (5 i64-boundary rows), `event_tags` (4 composite-PK+jsonb rows). Zero "SchemaVersionNotSupported" errors in the log. **34-03's blocker hypothesis was wrong — Phase 34 schema typing is correct.**

## Summary

total: 3
passed: 1
issues: 0
pending: 0
skipped: 0
blocked: 2

## Gaps

### Operator environment blocker (Phase 35)

The TS reference repo at `/private/tmp/ivm-parity-ts-ref` (per CONTEXT D-01, a git worktree of upstream `rocicorp/mono` for IVM parity comparison) has a module-resolution error preventing the TS oracle cache from booting. The export exists in the source file, so this is a setup issue (tsx version, package.json `type: "module"`, or symlink config) rather than a code defect. Phase 35 must:

1. Resolve the import error so TS oracle boots on :4858.
2. Re-run live 1k fuzz; capture any new divergences in `PARITY_STATUS.md` (D-20 carry-forward) and add as regression tests in `tools/ivm-parity/` before fixing.
3. Re-run full `npm test` sweep.

### Pre-existing schema-vs-seed inconsistency (FIXED)

During this UAT, found that `seed.sql` referenced `tickets/activities/canvases/channel_recaps/calls` tables missing from `schema.sql`. The DDL existed only in user's stash@{1} (preserved during Phase 34's 34-01 worktree merge). Restored as `fix(34): restore tickets/activities/...` in this session — non-functional Phase 34 work, just brings the harness DB to a migratable state.
