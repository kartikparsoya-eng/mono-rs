# Phase 19: Edit Semantics Verification - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md - this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 19-edit-semantics-verification
**Areas discussed:** Edit split verification strategy, Ordering semantics, Insert->delete cancellation

---

## Edit Split Verification Strategy

| Option                          | Description                                                           | Selected |
| ------------------------------- | --------------------------------------------------------------------- | -------- |
| Exact change_type match         | Verify Rust emits same change_type as TS. Stricter.                   |          |
| Final row set equality          | Only verify final visible rows match. Less strict.                    |          |
| Both: type match + set equality | Verify exact change_type strings AND final row set. Maximum coverage. | Y        |

**User's choice:** Both: type match + set equality
**Notes:** This is the whole point of Phase 19 - verifying intermediate stream, not just final state.

---

## Ordering Semantics

| Option                             | Description                                              | Selected |
| ---------------------------------- | -------------------------------------------------------- | -------- |
| Set equality (order-insensitive)   | Changes compared as set regardless of order.             | Y        |
| Ordered array comparison           | Changes must appear in exact order. Fragile with Rayon.  |          |
| Set equality + stable within query | Cross-pipeline unordered, same query_id preserves order. |          |

**User's choice:** Set equality (order-insensitive)
**Notes:** Rayon makes ordering non-deterministic. Consumers don't depend on order.

---

## Insert->Delete Cancellation

| Option                  | Description                                           | Selected |
| ----------------------- | ----------------------------------------------------- | -------- |
| Rust unit tests only    | Tests in advance.rs for cancellation. Fast, isolated. | Y        |
| Vitest integration only | Tests through pipeline-driver. Full stack.            |          |
| Both levels             | Rust unit + vitest end-to-end.                        |          |

**User's choice:** Rust unit tests only
**Notes:** Core logic lives in Rust; unit-level verification is sufficient.

---

## Claude's Discretion

- Test file naming and organization
- Number of test cases per scenario

## Deferred Ideas

None
