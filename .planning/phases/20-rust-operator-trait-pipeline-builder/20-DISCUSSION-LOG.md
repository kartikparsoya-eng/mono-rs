# Phase 20: Rust Operator Trait & Pipeline Builder - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 20-Rust Operator Trait & Pipeline Builder
**Areas discussed:** Trait design, Reuse vs rewrite, Pipeline builder boundary, State lifetime

---

## Trait Design — Type System

| Option              | Description                                                                             | Selected |
| ------------------- | --------------------------------------------------------------------------------------- | -------- |
| Trait objects (dyn) | Box<dyn Operator> for tree. Simple, flexible, matches TS hierarchy. Small vtable cost.  | ✓        |
| Enum dispatch       | OperatorKind enum with match arms. Zero vtable but unwieldy with 6+ operators.          |          |
| Generics            | Monomorphization. Max perf but tree shape varies per query — needs type erasure anyway. |          |

**User's choice:** Trait objects (dyn)
**Notes:** Recommended option — vtable cost negligible vs SQLite I/O

## Trait Design — Return Types

| Option                | Description                                                                        | Selected |
| --------------------- | ---------------------------------------------------------------------------------- | -------- |
| Collected (Vec)       | fetch() returns Vec<Node>, push() returns Vec<Change>. Simpler, matches FFI needs. | ✓        |
| Streaming (Iterator)  | Lazy evaluation, lower memory. Harder to parallelize.                              |          |
| Vec now, stream later | Pragmatic path.                                                                    |          |

**User's choice:** Collected (Vec)
**Notes:** Results always fully materialized before crossing FFI boundary anyway

## Reuse vs Rewrite

| Option             | Description                                                                     | Selected |
| ------------------ | ------------------------------------------------------------------------------- | -------- |
| Wrap existing code | Current filter.rs/join.rs/exists.rs become internal helpers in new trait impls. | ✓        |
| Fresh rewrite      | Clean slate. Risk of re-introducing fixed bugs.                                 |          |
| Hybrid             | New fetch(), existing push().                                                   |          |

**User's choice:** Wrap existing code
**Notes:** 139 cargo tests already validate evaluation logic — don't risk regression

## Pipeline Builder Boundary

| Option                                 | Description                                                     | Selected |
| -------------------------------------- | --------------------------------------------------------------- | -------- |
| TS extracts config → Rust builds tree  | TS parses AST, sends JSON config. Rust builds tree from config. | ✓        |
| Raw AST to Rust                        | Send ZQL AST JSON to Rust, Rust parses. Duplicates builder.ts.  |          |
| TS builds topology → Rust instantiates | Ordered operator descriptors from TS.                           |          |

**User's choice:** TS extracts config → Rust builds tree
**Notes:** TS already has AST parser (builder.ts) — no need to duplicate in Rust

## State Lifetime

| Option                   | Description                                                    | Selected |
| ------------------------ | -------------------------------------------------------------- | -------- |
| Owned state per operator | Each operator owns mutable state. No sharing, no locking.      | ✓        |
| Arc<Mutex> shared state  | For multi-thread access. Overkill for Phase 20.                |          |
| External state store     | Operators stateless, state keyed externally. More indirection. |          |

**User's choice:** Owned state per operator
**Notes:** Pipelines are independent — cross-pipeline parallelism comes in Phase 22/24

## Claude's Discretion

- Exact trait method signatures, error types, lifetime parameters
- Internal data structures per operator
- Config JSON schema details
- Whether Skip/Cap are standalone or thin wrappers

## Deferred Ideas

None — discussion stayed within phase scope
