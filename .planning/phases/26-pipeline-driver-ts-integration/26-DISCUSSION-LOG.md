# Phase 26: Pipeline-Driver TS Integration - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 26-pipeline-driver-ts-integration
**Areas discussed:** Hydration integration, Advance expansion, Serialization format, TableSource fate
**Mode:** Auto (all recommended defaults selected)

---

## Hydration Integration Strategy

| Option             | Description                                                             | Selected |
| ------------------ | ----------------------------------------------------------------------- | -------- |
| Replace entirely   | Replace buildPipeline() + input.fetch() with single Rust hydration call | ✓        |
| Wrap existing flow | Keep TS pipeline, add Rust as optional accelerator                      |          |

**User's choice:** [auto] Replace entirely (recommended default)
**Notes:** This is the explicit phase goal per ROADMAP.md. TS pipeline build becomes dead code.

---

## Advance Expansion Scope

| Option                    | Description                                                       | Selected |
| ------------------------- | ----------------------------------------------------------------- | -------- |
| Expand to all query types | Remove filter-only gating, handle joins/limits/companions in Rust | ✓        |
| Keep filter-only gating   | Only expand incrementally as each operator type is validated      |          |

**User's choice:** [auto] Expand to all (recommended default)
**Notes:** Phase 24 built full operator tree advance. No reason to keep artificial restrictions.

---

## Serialization Format

| Option               | Description                                           | Selected |
| -------------------- | ----------------------------------------------------- | -------- |
| Keep JSON initially  | Match existing rustFanOut() interface, optimize later | ✓        |
| Switch to binary now | Use Phase 25 binary protocol for all paths            |          |

**User's choice:** [auto] Keep JSON initially (recommended default)
**Notes:** Binary for hydration results (large row sets), JSON for advance fan-out (smaller payloads). Full binary optimization deferred to Phase 28.

---

## TableSource Fate

| Option          | Description                                                | Selected |
| --------------- | ---------------------------------------------------------- | -------- |
| Thin wrapper    | Keep for setDB(), overlay management, connection lifecycle | ✓        |
| Remove entirely | Move all responsibilities to Rust                          |          |

**User's choice:** [auto] Thin wrapper (recommended default)
**Notes:** TableSource still owns snapshot switching and IVM graph edge management. Only SQLite reads move to Rust.

---

## Claude's Discretion

- Generator-to-array conversion strategy
- Error handling and fallback behavior
- RustPipelineConfig expansion structure

## Deferred Ideas

- Binary serialization for advance fan-out (Phase 28)
- Removing timeSliceQueue (Phase 27)
- Streaming decode for hydration (Phase 28)
