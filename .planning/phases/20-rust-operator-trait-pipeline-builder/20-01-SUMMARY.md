---
phase: 20
plan: '01'
status: complete
started: 2026-04-21T00:00:00Z
completed: 2026-04-21T00:00:00Z
---

# Plan 20-01 Summary: Core Types, Operator Trait, Filter/Skip/Cap Operators

## What Was Built

Core IVM (Incremental View Maintenance) types and three operator implementations in Rust, forming the foundation of the operator pipeline for zero-ivm-rs:

- **Core types**: Row (serde_json::Map), Node, Change (enum with Add/Remove/Child/Edit), FetchRequest, SortSpec, compare_rows utility
- **Operator trait**: `pub trait Operator: Send` with `fetch()`, `push()`, and `op_type()` methods
- **FilterOperator**: Wraps existing `evaluate_filter()` predicate engine; handles Add/Remove/Edit/Child push semantics (edit-to-add/remove conversion when predicate match changes)
- **SkipOperator**: Bound-based row filtering using sort-aware `compare_rows`; modifies fetch start and filters push changes
- **CapOperator**: Count-based limiting with PK tracking per partition; propagates only changes for tracked rows

## Key Files Created/Modified

- `packages/zero-ivm-rs/src/types.rs` — Row, Node, Change, FetchRequest, SortSpec, compare_rows
- `packages/zero-ivm-rs/src/operator.rs` — Operator trait definition
- `packages/zero-ivm-rs/src/filter_op.rs` — FilterOperator with 4 tests
- `packages/zero-ivm-rs/src/skip_op.rs` — SkipOperator with 3 tests
- `packages/zero-ivm-rs/src/cap_op.rs` — CapOperator with 3 tests
- `packages/zero-ivm-rs/src/lib.rs` — Module registration (modified)

## Deviations from Plan

None

## Issues Encountered

- Clippy caught an `else { if }` pattern in skip_op.rs that needed collapsing to `else if` — fixed in the final commit

## Self-Check

PASSED — 72 cargo tests pass (33 new + 39 existing), all acceptance criteria met, cargo clippy clean on new code
