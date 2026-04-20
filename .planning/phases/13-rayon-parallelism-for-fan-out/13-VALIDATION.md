---
phase: 13
slug: rayon-parallelism-for-fan-out
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-20
---

# Phase 13 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | vitest + cargo test |
| **Config file** | `packages/zero-ivm-rs/Cargo.toml`, vitest workspace |
| **Quick run command** | `cd packages/zero-ivm-rs && cargo test` |
| **Full suite command** | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` |
| **Estimated runtime** | ~30 seconds |

---

## Sampling Rate

- **After every task commit:** Run `cd packages/zero-ivm-rs && cargo test`
- **After every plan wave:** Run full pipeline-driver test suite
- **Before `/gsd-verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Test Type | Automated Command | Status |
|---------|------|------|-----------|-------------------|--------|
| 13-01-01 | 01 | 1 | unit | `cd packages/zero-ivm-rs && cargo test diff` | ⬜ pending |
| 13-02-01 | 02 | 2 | unit+integration | `cd packages/zero-ivm-rs && cargo test advance` | ⬜ pending |
| 13-03-01 | 03 | 3 | integration | `npx vitest run pipeline-driver.test.ts` | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] Rayon dependency added to `Cargo.toml`
- [ ] Existing 58 Rust tests still pass after Rayon integration
