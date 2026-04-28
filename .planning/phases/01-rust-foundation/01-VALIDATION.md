---
phase: 1
slug: rust-foundation
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-20
---

# Phase 1 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property               | Value                                                                       |
| ---------------------- | --------------------------------------------------------------------------- |
| **Framework**          | vitest 4.1.3 (existing) + cargo test (new)                                  |
| **Config file**        | `packages/zqlite/vitest.config.ts` (existing)                               |
| **Quick run command**  | `npx turbo run test --filter=zqlite`                                        |
| **Full suite command** | `npx turbo run test --filter=zqlite && cd packages/zqlite-rs && cargo test` |
| **Estimated runtime**  | ~15 seconds                                                                 |

---

## Sampling Rate

- **After every task commit:** Run `npx turbo run test --filter=zqlite`
- **After every plan wave:** Run full suite command
- **Before `/gsd-verify-work`:** Full suite must be green
- **Max feedback latency:** 15 seconds

---

## Per-Task Verification Map

| Task ID  | Plan | Wave | Requirement        | Test Type   | Automated Command                        | Status     |
| -------- | ---- | ---- | ------------------ | ----------- | ---------------------------------------- | ---------- |
| 01-01-01 | 01   | 1    | FOUND-01           | build       | `cd packages/zqlite-rs && npm run build` | ⬜ pending |
| 01-02-01 | 02   | 2    | FOUND-01, FOUND-02 | unit        | `cd packages/zqlite-rs && cargo test`    | ⬜ pending |
| 01-03-01 | 03   | 3    | TEST-01, TEST-02   | integration | `npx turbo run test --filter=zqlite`     | ⬜ pending |

_Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky_

---

## Wave 0 Requirements

- Existing `db.test.ts` infrastructure covers primary correctness gate
- Rust `#[cfg(test)]` tests added in Plan 02

_Existing infrastructure covers all phase requirements._

---

## Manual-Only Verifications

| Behavior  | Requirement | Why Manual                        | Test Instructions                                                |
| --------- | ----------- | --------------------------------- | ---------------------------------------------------------------- |
| WAL2 mode | FOUND-01    | Requires WAL2-patched SQLite file | Open file-backed DB, verify `PRAGMA journal_mode` returns `wal2` |

---

## Validation Sign-Off

- [ ] All tasks have automated verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 15s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
