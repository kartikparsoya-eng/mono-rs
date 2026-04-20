# Phase 12: Rust Exists Operator - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 12-rust-exists-operator
**Areas discussed:** Rust computation scope, Cache strategy, Integration wiring

---

## Rust Computation Scope

| Option | Description | Selected |
|--------|-------------|----------|
| Batch push + decision logic | Push routing, cache lookup, size-based add/remove decision logic all in Rust. Single napi call per push. fetchSize stays in TS, passes size into Rust. | ✓ |
| Cache-only in Rust | Only cache management in Rust. Push routing stays in TS. Minimal FFI surface but less perf gain. | |
| Utility functions only | Individual utility functions in Rust, orchestration in TS. Multiple small napi calls per push. | |

**User's choice:** Batch push + decision logic
**Notes:** Consistent with Join batch pattern (D-49). fetchSize must stay in TS due to relationship generators.

---

## Cache Strategy

| Option | Description | Selected |
|--------|-------------|----------|
| Rust-side cache | Rust HashMap<String, bool> persisted across push cycles. Cleared via napi call on endFilter(). | |
| TS-side cache | Keep cache as TS Map. Rust receives pre-resolved exists values. Cache logic is trivial, not a bottleneck. | ✓ |
| Hybrid | Cache key computation in Rust, storage in TS. | |

**User's choice:** TS-side cache
**Notes:** Cache logic is trivial Map get/set — not worth the napi complexity of persisting state in Rust.

---

## Integration Wiring

| Option | Description | Selected |
|--------|-------------|----------|
| New createExists delegate | Add createExists? to BuilderDelegate. Same pattern as createStorage for Take. | ✓ |
| Extend createFilter | Extend existing createFilter to handle both Filter and Exists. Less API surface. | |
| Post-build wrapping | Detect in pipeline-driver post-build and wrap/replace. No delegate change needed. | |

**User's choice:** New createExists delegate
**Notes:** Clean separation, same proven pattern as Take's createStorage delegate.

---

## Claude's Discretion

- Internal Rust data structures for change representation
- Exact napi function signature design
- Whether to reuse Value/compare_values from filter.rs

## Deferred Ideas

None — discussion stayed within phase scope
