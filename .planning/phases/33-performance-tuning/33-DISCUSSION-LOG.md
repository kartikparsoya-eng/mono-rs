# Phase 33: Production Hardening + Benchmarks - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-29
**Phase:** 33-performance-tuning (directory name) / Production Hardening + Benchmarks (title)
**Areas discussed:** Roadmap restructure (split 33 → 33+34), HARDEN-01 wiring strategy, plus full scope decisions delegated to Claude

> **Note:** Directory name remains `33-performance-tuning` for legacy/tooling-slug consistency. Title was renamed mid-discussion when scope expanded from PERF-only to Production Hardening + Benchmarks (with new Phase 34 carved out for differential fuzz).

---

## Roadmap Restructure (Pre-discussion)

User pivoted Phase 33 from "Performance Tuning + Benchmarks" to "Production Readiness / layered defense" based on the question: "How do I make TS→Rust IVM porting prod-ready?"

| Option                                                                                                  | Description                                               | Selected |
| ------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- | -------- |
| A. Refocus narrowly: dualExecCompare wiring + OrExists tests + PERF benchmarks                          | ~1 week scope; defer fuzz to v5.1                         |          |
| B. Full hardening: all 6 in-scope items in one phase                                                    | ~3 weeks; longest phase of milestone                      |          |
| C. Split into two phases: 33 (high-ROI hardening + benchmarks), 34 (random-AST fuzz + schema extension) | Ships v5.0 sooner, hardening continues in dedicated phase | ✓        |

**Decision:** Option C. Phase 33 absorbs HARDEN-01/02 + original PERF-01/02/03. Phase 34 (NEW) takes FUZZ-01/02.

**Rationale:** dualExecCompare wiring + OrExists test expansion + benchmarks fit a ~1-week phase and deliver immediate prod-readiness value. Random-AST differential fuzz is a multi-week investment (per the plan you described — 1-2 weeks just for the harness) that deserves its own focused phase. Splitting prevents Phase 33 from ballooning while still delivering the high-ROI items in v5.0.

**Roadmap edits applied:**

- Phase 33 renamed: "Performance Tuning + Benchmarks" → "Production Hardening + Benchmarks"
- Phase 33 requirements: PERF-01, PERF-02, PERF-03 → HARDEN-01, HARDEN-02, PERF-01, PERF-02, PERF-03 (5 reqs)
- Phase 34 added: "Differential Fuzz + Schema Extension" with FUZZ-01, FUZZ-02
- Coverage table: 29 → 33 requirements
- New requirements added to REQUIREMENTS.md with full descriptions

Committed: `856c7c38d`

---

## HARDEN-01: dualExecCompare wiring strategy

| Option                              | Description                                                                                                                            | Selected |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| A. Always-on with sampling          | Every advance/hydrate may run comparison; 1-in-N sampling keeps overhead low. Logs divergences continuously, exposes counter for tests | ✓        |
| B. Strict-mode flag, off by default | `ZQLITE_RS_PARITY_CHECK=true` enables; throws on divergence. Dedicated CI run, doesn't add noise to other tests                        |          |

**Decision:** Option A.

**User's reasoning:** "procceed with a" — chose continuous sampling over dedicated-run approach.

**Implementation:** Three-mode env var `ZQLITE_RS_PARITY_CHECK=off|sample|strict`:

- `off` (default in prod): no comparison
- `sample` (default in CI tests): 1-in-10 sampling, log + count divergences
- `strict` (dedicated parity-check CI run): every call, throw on divergence

**Why A over B:** Continuous sampling catches more in practice — divergences that only manifest at certain query shapes will fire eventually under sampling, whereas a dedicated strict run only sees what those specific tests cover. The 10% sampling overhead is acceptable in CI (typical advance/hydrate is < 10ms, so 1ms additional cost per call is fine).

---

## All other Phase 33 decisions (delegated to Claude per "correctness + long-term" rule)

The following decisions were made unilaterally based on the user's standing delegation:

- **D-01:** 3 plans, all Wave 1 in parallel (no file overlap)
- **D-09:** OrExists test categories mirroring Exists (≥30 tests, no padding)
- **D-12-13:** PERF-01 channel-block test pattern (Mutex/Barrier or recv_timeout)
- **D-14-17:** PERF-02 TTFB benchmark (4 pipelines, 10/50/100/500ms delays, vitest-integrated)
- **D-18-22:** PERF-03 memory benchmark (`process.memoryUsage()` polled at 50ms, reject jemalloc)
- **D-23-24:** Bench results in `.planning/milestones/v5.0-bench-results.md`, append-only
- **D-25-26:** Verifier runs both `sample` and `strict` modes
- **D-27-30:** Anti-hack guards (no buffered method changes, no production code in or_exists, no `.skip`)

**Rationale on memory measurement (D-18/19):** rejected jemalloc-stats because switching the global Rust allocator changes the released binary; what matters for PERF-03 is the RELATIVE peak between buffered and streaming, which `process.memoryUsage()` captures cross-platform without binary modification.

**Rationale on bench in vitest (D-17):** integrating with existing test infrastructure avoids a separate runner; benchmarks become routine CI assertions rather than a parallel pipeline that could rot.

**Rationale on bench results storage (D-23/24):** append-only markdown gives a historical record without a benchmark database. PR reviewers can spot regressions by comparing recent lines.

## Claude's Discretion

(Recorded in CONTEXT.md `<decisions>` ### Claude's Discretion)

- Specific OrExists test names within categories
- Channel-block test scaffolding mechanism
- Bench file naming (extend rust-ivm-bench.ts or new file)
- Vitest bench API vs `it` blocks with timing
- Bench results table format details

## Deferred Ideas

(Recorded in CONTEXT.md `<deferred>` section)

- Random-AST differential fuzz → Phase 34 (FUZZ-01)
- Schema extension → Phase 34 (FUZZ-02)
- Production shadow mode, observability, runbook → post-milestone (operational)
- Removing feature flags → post-milestone
- jemalloc allocator switch → out of scope (binary change)
- CI bench wiring decision → ops-team responsibility
