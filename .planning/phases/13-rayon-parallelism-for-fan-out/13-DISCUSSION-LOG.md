# Phase 13: Rayon Parallelism for Fan-out - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 13-rayon-parallelism-for-fan-out
**Areas discussed:** Parallelism granularity, Rayon integration point, Snapshot diff sharing

---

## Parallelism Granularity

| Option | Description | Selected |
|--------|-------------|----------|
| Per-pipeline fan-out | Main thread reads diff once, Rayon par_iter over pipelines per change. No I/O duplication. | ✓ |
| Intra-operator batch split | Rayon splits batch within a single operator's napi call. Simplest but limited parallelism. | |
| Both levels | Per-pipeline + intra-operator. Maximum parallelism, most complex. | |

**User's choice:** Per-pipeline fan-out
**Notes:** Avoids the worker threads I/O duplication problem (D-26) since diff is read once.

---

## Rayon Integration Point

| Option | Description | Selected |
|--------|-------------|----------|
| Rust-owned fan-out | New napi function drives full advance loop. Rust owns thread pool + pipeline topology. | ✓ |
| TS-orchestrated, Rust-parallel | TS iterates diff, each pipeline push uses Rayon internally. Simpler but less parallel. | |
| Batch dispatch to Rust | TS batches (change, pipeline_id) pairs, Rust does par_iter. Middle ground. | |

**User's choice:** Rust-owned fan-out
**Notes:** Most ambitious — Rust owns the entire advance loop including pipeline topology.

---

## Snapshot Diff Sharing

| Option | Description | Selected |
|--------|-------------|----------|
| Pre-materialize in TS | TS reads diff into JSON array, passes to Rust. Simple but doubles memory. | |
| Rust reads diff directly | Rust reads snapshot diff from SQLite via zqlite-rs. Most performant, requires porting snapshotter logic. | ✓ |
| Streaming batches from TS | TS batches N changes at a time, sends to Rust. Incremental, no snapshotter porting. | |

**User's choice:** Rust reads diff directly
**Notes:** Bold choice — requires Rust to understand snapshotter diff SQL. Eliminates TS from the hot path entirely.

---

## Claude's Discretion

- Pipeline topology serialization format
- Snapshotter diff porting scope
- Rayon thread pool sizing
- Error handling and fallback

## Deferred Ideas

None
