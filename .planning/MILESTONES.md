# Milestones

## v5.0 Streaming + Differential Fuzz (Shipped: 2026-04-30)

**Phases completed:** 5 phases, 19 plans, 46 tasks

**Key accomplishments:**

- One-liner:
- One-liner:
- One-liner:
- Promoted 7 framework-invariant `debug_assert!`/`debug_assert_eq!` sites to `assert!`/`assert_eq!` across Join/Exists/OrExists/Take/Cap operators so silent miscalculation regressions surface as loud release-mode panics, with 7 `#[should_panic]` regression tests proving the assertions fire under `cargo test --release`.
- Closes the AUDIT-02 verification gap (truths #6 and #7 of 30-VERIFICATION.md) via a TS build-freshness gate that prevents stale napi binaries from silently masking correct Rust IVM logic, plus a Rust regression unit test pinning the full production AST shape; the 30-02 source fix was already correct end-to-end.
- chunk_encoder + AdvanceStream/HydrateStream napi classes + advance_streaming/hydrate_streaming/hydrate_query_streaming methods, with rayon-scoped per-pipeline fan-out, panic isolation via catch_unwind, and Arc<AtomicBool> cancellation observed at the per-change loop boundary
- decodeAdvanceChunkBuf + RustStreamError + PipelineDriver.{advanceStreaming, addQueriesStreaming} with D-15 finally contract, plus 1k-iteration fast-check parity fuzz that confirms streaming and buffered advance produce equivalent RowChange multisets
- One-liner:
- View-syncer now consumes Rust's per-pipeline streaming surface under `ZQLITE_RS_USE_STREAMING_CONSUMER` (default on), firing `pokePart` mid-batch as fast pipelines complete instead of buffering to end-of-stream — operator rollback via env var preserved.
- ZQLITE_RS_PARITY_CHECK env-gated TS-vs-Rust sampling shim wired into pipeline-driver's #rustAdvanceAsync and addQueriesAsync hot paths, with a TS oracle adapted from the upstream reference repo and a 15-test parity-check.test.ts proving sample/strict/counter mechanics.
- 1. [Plan §<behavior> simplification — Builder-spec parity test]
- One-liner:
- One-liner:
- One-liner:
- One-liner:
- 1. [Rule 3 - Blocking] FK target table mismatch — `tickets` does not exist
- One-liner:
- One-liner:
- One-liner:

---

## v4.0 v4.0 Rust IVM (Shipped: 2026-04-29)

**Phases completed:** 11 phases, 17 plans, 0 tasks

**Key accomplishments:**

- Status:
- Status:
- Status:
- Status:

---
