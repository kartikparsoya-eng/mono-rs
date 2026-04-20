# Phase 2: StatementRunner - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 02-statementrunner
**Areas discussed:** Rewrite scope

---

## Rewrite Scope

| Option | Description | Selected |
|--------|-------------|----------|
| Rewrite StatementRunner in Rust | Move 70 lines to Rust, requires Rust→TS callback for StatementCache | |
| Keep StatementRunner in TS | Already delegates to Rust Database via Phase 1 | ✓ |
| Move StatementCache + StatementRunner to Rust | Reverses Phase 1 D-08, scope creep | |

**User's choice:** Keep StatementRunner in TS
**Notes:** User explicitly said: "Keep StatementRunner in TS. Do not reverse D-08. Real Rust wins come from Phases 3-6. Consumer imports unchanged via re-export shim if any implementation moves later."

---

## Benchmark Context

A/B benchmark (zqlite vs zqlite-rs) showed per-call FFI overhead makes Rust 0.5-0.65x slower for individual operations. This confirms that wrapping TS delegation layers in Rust provides no benefit — gains come from keeping entire loops in Rust (Phases 3-6).

---

## Deferred Ideas

None.
