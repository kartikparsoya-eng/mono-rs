# Phase 32: View-Syncer Streaming Migration - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-29
**Phase:** 32-view-syncer-migration
**Areas discussed:** Rollback / safety strategy, RustStreamError handling per kind, CR-01 i64 decoder fix scope, MIGRATE-03 test strategy, Plan granularity (implicit)

---

## Gray Area Selection

| Option                            | Description                                            | Selected |
| --------------------------------- | ------------------------------------------------------ | -------- |
| Rollback / safety strategy        | Feature flag for streaming consumer, or full migration | ✓        |
| RustStreamError handling per kind | Branch on `kind` discriminator vs single fallthrough   | ✓        |
| CR-01 i64 decoder fix scope       | Fix in Phase 32, separate phase, or defer              | ✓        |
| MIGRATE-03 test strategy          | Synthetic, real-world fixture, or both                 | ✓        |

**User's choice:** All four areas, with the directive: **"take decision based on correctness and long term"**

**Notes:** User delegated all four decisions to Claude with the same constraint as Phase 31. Following the broader anti-hack rule, decisions made with explicit reasoning rather than picking shortcuts. Each decision recorded inline in CONTEXT.md `<decisions>` and itemized below for audit.

---

## Rollback / Safety Strategy

| Option                                             | Trade-offs                                                                                                | Selected |
| -------------------------------------------------- | --------------------------------------------------------------------------------------------------------- | -------- |
| Feature flag, kept indefinitely                    | Permanent rollback escape hatch; double maintenance burden; both code paths must stay tested forever      |          |
| Feature flag for one production cycle, then remove | Phased rollout with emergency pressure release; cleanup in follow-up phase keeps codebase clean long-term | ✓        |
| Full migration, no flag                            | Cleanest codebase; harder to rollback (revert commit only); risky for first-consumer migration            |          |

**Decision:** Feature flag `ZQLITE_RS_USE_STREAMING_CONSUMER`, default `true` after deploy validation. Phase 32.1 (or milestone wrap-up) removes the flag and the buffered fallback.

**Rationale:** This is the FIRST production consumer of streaming. Production exposure characteristics (rayon scheduling under real load, mpsc burst, panic timing in real pipelines) have unknown unknowns. A 1-cycle flag gives operators an emergency rollback without a code revert. Long-term, don't carry the flag forever (option a) or ship without one for a first-consumer migration (option c is too aggressive). Option b is the principled middle.

---

## RustStreamError Handling per kind

| Option                                                                   | Trade-offs                                                                                                                  | Selected |
| ------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------- | -------- |
| Single catch + log + bubble (no per-kind logic)                          | Simplest; but can't differentiate panic vs cancel vs channel-close in observability                                         |          |
| Catch + log with `kind` discriminator + bubble                           | Same crash semantics as today's buffered errors; `kind` field enables observability without coupling to in-process recovery | ✓        |
| Per-kind recovery (e.g., panic crashes, channel_closed retries silently) | Most sophisticated; but second-guessing panics in the first-consumer migration risks masking bugs                           |          |

**Decision:** All three kinds → `lc.error('rust streaming error', { kind, message, source })` then `throw err` to crash view-syncer. Reconnect handles transient issues. No per-kind recovery logic.

**Rationale:** Streaming surface is too new to second-guess panic causes mid-flight. Existing view-syncer crash + reconnect pattern handles transient issues. The `kind` field in structured logs enables observability and post-hoc bug identification without coupling to in-process recovery code that could mask real bugs. Long-term cost of per-kind recovery: every kind becomes a hidden state machine. Single discriminated log keeps it simple and observable.

---

## CR-01 i64 Decoder Fix Scope

| Option                                           | Trade-offs                                                    | Selected |
| ------------------------------------------------ | ------------------------------------------------------------- | -------- |
| Fix in Phase 32 as plan 32-01 (runs first)       | Adds scope but ensures consumer migration ships clean decoder | ✓        |
| Fix as separate gap-closure phase (32.1 or 31.1) | Keeps Phase 32 focused on migration; delays the fix           |          |
| Defer until it actually breaks                   | Risky; pre-existing bug becomes "we knew about it" liability  |          |

**Decision:** Plan 32-01 (Wave 1, autonomous): fix CR-01 BEFORE the migration. 32-02 (Wave 2) does the migration.

**Rationale:** Phase 32 IS the first production exposure of `decodeAdvanceChunkBuf` (which shares the buggy `readJsonValue` helper with `decodeAdvanceResultBuf`). Both decoders are now in the live request path. Shipping a known critical bug because it's "pre-existing" is technical debt — the user explicitly said "no hacks, proper fixes." Long-term: fixing it here is the proper move.

---

## MIGRATE-03 Test Strategy

| Option                                           | Trade-offs                                                                      | Selected |
| ------------------------------------------------ | ------------------------------------------------------------------------------- | -------- |
| Synthetic test only (mock pipelines with timers) | Sufficient for correctness property (mid-batch pokePart); fast, deterministic   | ✓        |
| Real-world zbugs fixture only                    | More realistic; harder to set up; really a performance characterization concern |          |
| Both                                             | Most coverage; inflates Phase 32 scope                                          |          |

**Decision:** Synthetic test in 32-02 (`view-syncer-streaming-mid-batch.test.ts`). Real-world fixture deferred to Phase 33 PERF-01.

**Rationale:** Synthetic test is sufficient to verify the load-bearing correctness property — mid-batch pokePart fires before slowest pipeline completes. Real-world fixture is performance characterization, which is Phase 33's domain. Doing both in Phase 32 inflates scope without proportional benefit. Long-term: keep MIGRATE-03 verification atomic (single test pinning the property); Phase 33 adds benchmark fixtures for performance reporting.

---

## Plan Granularity (Decided implicitly during area discussion)

| Option                                                                                  | Trade-offs                                                                                      | Selected |
| --------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- | -------- |
| Single migration plan (CR-01 + view-syncer in one plan)                                 | Tight coupling; large diff; CR-01 fix tangled with migration logic                              |          |
| 2 plans: 32-01 (CR-01 fix), 32-02 (view-syncer migration both callsites + flag + tests) | Atomic CR-01 fix; reviewable migration unit; sequential waves                                   | ✓        |
| 3 plans: 32-01 (CR-01), 32-02 (#advancePipelines), 32-03 (#hydrateUnchangedQueries)     | Most granular; but both view-syncer plans touch view-syncer.ts → file conflict in worktree mode |          |

**Decision:** 2 plans, sequential waves.

**Rationale:** CR-01 is independent of view-syncer; isolating it makes the migration's diff focused and reviewable as one unit. Both view-syncer callsites belong in one plan because they're tightly coupled (same file, same trace span structure, same error handling, same poker protocol) and would conflict at the worktree level if split. 2 plans is the right granularity.

---

## Claude's Discretion

(Recorded in CONTEXT.md `<decisions>` ### Claude's Discretion)

- Specific instrumentation mechanism for the mid-batch pokePart test (timestamp array, mock spy, async barrier)
- Exact threshold value for `hi` magnitude check in CR-01 fix
- How to wrap existing view-syncer tests in `describe.each([true, false])`
- Exact env var read site (boot vs per-instance)

## Deferred Ideas

(Recorded in CONTEXT.md `<deferred>` section)

- Remove feature flag + buffered fallback → follow-up phase after one production cycle
- Real-world workload fixture → Phase 33 PERF-01
- Memory footprint benchmark → Phase 33 PERF-02
- TTFB microbenchmark → Phase 33 PERF-01
- Removing `advanceAsync` / `addQueriesAsync` / `decodeAdvanceResultBuf` → NOT in this milestone
- WR-02..WR-05 from 31-REVIEW.md → revisit if production exposure surfaces actual bugs
