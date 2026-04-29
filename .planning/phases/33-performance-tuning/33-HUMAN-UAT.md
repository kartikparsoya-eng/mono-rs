---
status: resolved
phase: 33-performance-tuning
source: [33-VERIFICATION.md]
started: 2026-04-29T19:00:00Z
updated: 2026-04-29T19:30:00Z
---

## Current Test

[all items resolved]

## Tests

### 1. Strict-mode advance-path deviation acceptance

expected: Operator agrees the documented mono-rs limitation in pipeline-driver-ts-oracle.ts header is sufficient and that strict mode is not expected to gate advance path until a future phase delivers TS replay (FUZZ-01 / shadow-mode plan)
result: passed — accepted 2026-04-29. The architectural limitation is acknowledged. Real TS↔Rust differential testing will come via Phase 34 extending the existing `tools/ivm-parity/` two-process harness (option A) with fast-check + rich-types schema, rather than via in-process strict-mode parity check on the advance path.

### 2. Production-style soak validation

expected: `ZQLITE_RS_PARITY_CHECK=sample npm run start-zero-cache` with real ingest data — zero parity divergences logged over a multi-minute window of representative traffic; lc.warn does not fire spuriously; no perf regression vs baseline
result: deferred — out-of-scope per VALIDATION's "Manual-Only Verifications". Tracked for ops; not a phase blocker.

## Summary

total: 2
passed: 1
issues: 0
pending: 0
skipped: 1
blocked: 0

## Gaps
