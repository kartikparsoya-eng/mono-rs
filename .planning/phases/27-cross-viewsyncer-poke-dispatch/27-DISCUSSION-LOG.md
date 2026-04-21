# Phase 27: Cross-ViewSyncer Poke Dispatch - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 27-cross-viewsyncer-poke-dispatch
**Areas discussed:** NAPI Function Signature, Integration Point, timeSliceQueue Elimination, Parallelism Strategy, Error Handling
**Mode:** Auto (all areas auto-selected, recommended defaults chosen)

---

## NAPI Function Signature

| Option                  | Description                                                                     | Selected |
| ----------------------- | ------------------------------------------------------------------------------- | -------- |
| Batch dispatch function | Single `rust_dispatch_poke()` taking all VS pipelines, returning per-VS results | ✓        |
| Per-VS parallel calls   | Keep current per-VS `rustAdvance()` but parallelize in TS with worker threads   |          |

**User's choice:** [auto] Batch dispatch function (recommended — minimizes FFI overhead, single Rayon pool scheduling)

---

## Integration Point

| Option                            | Description                                                    | Selected |
| --------------------------------- | -------------------------------------------------------------- | -------- |
| Coordinator in view-syncer.ts     | Batch pending VS advances at the `version-ready` handler level | ✓        |
| Coordinator in pipeline-driver.ts | Each pipeline-driver registers with a global batch coordinator |          |

**User's choice:** [auto] Coordinator in view-syncer.ts (recommended — matches existing architecture where VS owns the advance loop)

---

## timeSliceQueue Elimination

| Option                  | Description                                                                     | Selected |
| ----------------------- | ------------------------------------------------------------------------------- | -------- |
| Conditional elimination | Remove timeSliceQueue for advance when Rust dispatch active, keep for hydration | ✓        |
| Full elimination        | Remove timeSliceQueue entirely                                                  |          |
| Keep as-is              | Leave timeSliceQueue even with Rust dispatch                                    |          |

**User's choice:** [auto] Conditional elimination (recommended — advance path moves off main thread, hydration still needs yielding)

---

## Error Handling

| Option                 | Description                                       | Selected |
| ---------------------- | ------------------------------------------------- | -------- |
| Per-VS error isolation | Errors in one VS don't affect others in the batch | ✓        |
| Fail-entire-batch      | Any VS error fails the whole dispatch             |          |

**User's choice:** [auto] Per-VS error isolation (recommended — matches existing ResetPipelinesSignal pattern)

---

## Claude's Discretion

- Batching/coordination mechanism details
- Connection pool sizing
- Thread pool strategy

## Deferred Ideas

None.
