# Phase 6: Bulk Loader - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 06-bulk-loader
**Areas discussed:** Implementation strategy

---

## Implementation Strategy

| Option                       | Description                                                                                                                                                        | Selected |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------- |
| Rust bulk INSERT method      | New Rust method: accepts flat values buffer + schema, does all batch INSERTs internally (one napi crossing per flush). Contradicts D-13 spirit but maximizes perf. |          |
| Skip Phase 6 entirely        | Phase 1 already provides Rust Statement.run(). The per-batch napi overhead is small relative to PG COPY network time.                                              |          |
| Benchmark first, then decide | Add initial-sync to the benchmark suite. If napi crossing overhead is <5% of total sync time, skip. If significant, implement Rust bulk insert.                    | ✓        |

**User's choice:** Benchmark first with clear decision thresholds: <5% skip, 5-15% flush-only Rust, >15% full Rust bulk INSERT. TransactionPool stays TS regardless.

**Notes:** User specified the decision rule should go into CLAUDE.md so Phase 6 scope is locked once benchmark runs. This makes Phase 6 effectively a "benchmark + conditional implementation" phase.

---

## Claude's Discretion

- Benchmark methodology details (which tables, how many rows, measurement approach)

## Deferred Ideas

None
