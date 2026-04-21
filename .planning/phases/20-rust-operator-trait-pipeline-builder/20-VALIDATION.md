---
phase: 20
slug: rust-operator-trait-pipeline-builder
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-21
---

# Phase 20 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property               | Value                                                                                                                                                                                                                                                                                                                        |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Framework**          | vitest + cargo test                                                                                                                                                                                                                                                                                                          |
| **Config file**        | `vitest.config.ts` (root), `packages/zero-ivm-rs/Cargo.toml`, `packages/zqlite-rs/Cargo.toml`                                                                                                                                                                                                                                |
| **Quick run command**  | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts`                                                                                                                                                                                                                                        |
| **Full suite command** | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts && ZERO_DUAL_EXEC=strict npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts && cargo test --manifest-path packages/zero-ivm-rs/Cargo.toml && cargo test --manifest-path packages/zqlite-rs/Cargo.toml` |
| **Estimated runtime**  | ~60 seconds                                                                                                                                                                                                                                                                                                                  |

---

## Sampling Rate

- **After every task commit:** Run `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts`
- **After every plan wave:** Run full suite command + `npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts`
- **Before `/gsd-verify-work`:** Full suite must be green + extended fuzz (FUZZ_NUM_RUNS=10000)
- **Max feedback latency:** 60 seconds

---

## Per-Task Verification Map

| Task ID  | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type   | Automated Command                                                                                           | File Exists | Status     |
| -------- | ---- | ---- | ----------- | ---------- | --------------- | ----------- | ----------------------------------------------------------------------------------------------------------- | ----------- | ---------- |
| 20-01-01 | 01   | 1    | OPR-01      | —          | N/A             | unit        | `cargo test --manifest-path packages/zero-ivm-rs/Cargo.toml`                                                | TBD         | ⬜ pending |
| 20-02-01 | 02   | 1    | OPR-02      | —          | N/A             | integration | `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts`                       | ✅          | ⬜ pending |
| 20-03-01 | 03   | 2    | OPR-03      | —          | N/A             | integration | `ZERO_DUAL_EXEC=strict npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` | ✅          | ⬜ pending |

_Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky_

---

## Wave 0 Requirements

_Existing infrastructure covers all phase requirements — pipeline-driver.test.ts, fuzz-ivm.test.ts, and cargo test already in place from prior phases._

---

## Manual-Only Verifications

_All phase behaviors have automated verification._

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 60s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
