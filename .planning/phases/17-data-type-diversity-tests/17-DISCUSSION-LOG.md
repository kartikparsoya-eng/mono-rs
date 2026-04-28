# Phase 17: Data Type Diversity Tests - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 17-data-type-diversity-tests
**Areas discussed:** Test structure, Data fixtures, Rust-vs-TS comparison

---

## Test Structure

| Option             | Description                                            | Selected |
| ------------------ | ------------------------------------------------------ | -------- |
| Separate files (4) | One file per requirement: json, null, unicode, numbers | ✓        |
| One file           | Single file with describe blocks per type              |          |
| Two files          | Split numbers from the rest                            |          |

**User's choice:** Separate files (4)

---

## Data Fixtures

| Option          | Description                             | Selected |
| --------------- | --------------------------------------- | -------- |
| Inline data     | All test rows defined inline            |          |
| Shared fixtures | Complex data in shared fixture file     |          |
| Hybrid          | Simple inline, complex in fixtures file | ✓        |

**User's choice:** Hybrid

---

## Rust-vs-TS Comparison

| Option               | Description                                                | Selected |
| -------------------- | ---------------------------------------------------------- | -------- |
| Rust-first + smoke   | Test Rust, separate smoke test for TS match                |          |
| Dual-path comparison | Run both paths via ZERO_DISABLE_RUST_IVM, assert identical | ✓        |
| Rust-only            | Only test Rust with expected values                        |          |

**User's choice:** Dual-path comparison

---

## Claude's Discretion

- Specific test case count per file
- Exact fixture file naming and location
- Order of tests within files

## Deferred Ideas

None
