# zero-cache Rust Rewrite (mono-rs)

## What This Is

A performance-focused partial rewrite of the Zero sync engine's server-side components (zero-cache, zqlite) from TypeScript to Rust via napi-rs. The goal is to eliminate JavaScript runtime bottlenecks — GC pressure, single-threaded execution, and JS-SQLite FFI overhead — while keeping TS for orchestration/I/O (WebSocket, auth, CVR, poke protocol).

## Core Value

All SQLite I/O and row-level computation must happen in Rust with multi-core parallelism, delivering 5-10x throughput improvement and eliminating single-threaded bottlenecks at scale.

## Requirements

### Validated

- ✓ Zero sync engine works end-to-end (PG → SQLite → IVM → Client) — existing
- ✓ Comprehensive test suites exist for all rewrite targets — existing
- ✓ Benchmarks exist in `packages/zero-cache/bench/` — existing
- ✓ Rust napi-rs module replaces `db.ts` + `statement-cache.ts` (Database/Statement API) — v1.0
- ✓ Rust StatementRunner replaces `statements.ts` — v1.0
- ✓ Binary buffer protocol: 1.78-1.98x faster than better-sqlite3 for bulk reads — v1.0
- ✓ Rust IVM operators (Filter, Join, Take, Exists) evaluate changes correctly — v2.0
- ✓ Rayon parallelism for advance fan-out (`rust_fan_out()`) — v2.0
- ✓ 139 Rust tests + 39 integration tests, all passing — v3.0
- ✓ E2E validation: Docker image tested against xyne-spaces, zero-cache healthy — v3.0
- ✓ Rust Operator trait with full fetch() + push() (pipeline builder in Rust) — v4.0
- ✓ Rust TableSource with SQLite connection pool (parallel reads) — v4.0
- ✓ Parallel multi-pipeline hydration via Rayon — v4.0
- ✓ Within-pipeline child parallelism (Join fan-out) — v4.0
- ✓ Full operator tree advance in Rust (replaces filter-only rust_fan_out) — v4.0
- ✓ Binary serialization format for Rust → TS row transfer — v4.0
- ✓ Pipeline-driver TS integration (addQuery → Rust, advance → Rust) — v4.0
- ✓ Cross-ViewSyncer parallel poke dispatch — v4.0
- ✓ E2E validation + benchmark suite — v4.0
- ✓ Dual-Exec Correctness Hardening — v4.0
- ✓ Per-pipeline streaming Rust API (advance_streaming, hydrate_streaming) — v5.0
- ✓ TS streaming wrappers (advanceStreaming, addQueriesStreaming) returning AsyncIterable — v5.0
- ✓ Cancellation via Arc<AtomicBool> — TS timer can stop Rust work — v5.0
- ✓ view-syncer migrated to streaming (pokes fire as fast pipelines complete) — v5.0
- ✓ TTFB drops from max(pipeline_time) to min(pipeline_time) — v5.0 (1.05× ratio measured, threshold 1.5×)
- ✓ Memory peak reduction: ~12.47× streaming-vs-buffered — v5.0 (threshold ≥4×)
- ✓ Audit fixes: LIKE case sensitivity, EXISTS split_edit_keys, debug_assert→assert, Exists Edit with or_predicate — v5.0 (Phase 30)
- ✓ Production parity-check shim (`ZQLITE_RS_PARITY_CHECK` env-gated TS-vs-Rust sampling) — v5.0 (Phase 33; advance-path strict mode unusable due to architectural mono-rs constraint, hydrate path works)
- ✓ OrExists test breadth parity (5 → 26 tests, exists_op.rs ≥22 target) — v5.0 (Phase 33)
- ✓ Bounded mpsc channel test (PERF-01) — v5.0 (Phase 33)
- ✓ Random-AST differential fuzz infrastructure (`tools/ivm-parity/random-ast-fuzz.ts` + `arb-ast.ts` + WALL_MS/PER_TABLE_HITS instrumentation) — v5.0 (Phase 34; live 1k execution deferred to v6.0 pending TS-oracle env fix)
- ✓ FUZZ-02 rich-types schema (jsonb, timestamptz, numeric, nullables, composite/array, NULL semantics, type-coercion fixtures, i64 > 2^53 boundaries) — v5.0 (Phase 34)
- ✓ Deep-audit Track 2 fixes B1 (Skip ordering), B2 (parent_sizes.max(1) cache poison), B3 (partition_key threading — closes original audit Risk #1), B11 (cascade-delete prev snapshot via additive `set_prev_snapshot` napi method) — v5.0 (Phase 34)

### Active

(Active section reset for v6.0 — see ROADMAP.md "Carry-Forward Tech Debt" for the full Phase 35+ candidate list. Headlining items:)

- [ ] FlippedJoin + UnionFanIn + UnionFanOut operators in Rust (closes deep-audit B7 + 2 catalogued PARITY_STATUS.md divergences) — sized as its own phase
- [ ] TS-oracle env import resolution (unblocks live 1k fast-check fuzz from Phase 34)
- [ ] EXISTS_LIMIT downgrade fix (deep-audit B6, security-relevant for PERMISSIONS_EXISTS_LIMIT)
- [ ] OrExists.push_child first-branch short-circuit (deep-audit B5)
- [ ] Companion scalar resolved_value drift fix (deep-audit B12)
- [ ] CI integration of `npm run fuzz-check:gate` (deferred from Phase 34 D-22)
- [ ] Pre-existing pipeline-driver.test.ts whereExists+permissions snapshot failure investigation

### Out of Scope

- Client-side code (zero-client, zero-react, etc.) — not performance-critical
- WebSocket/HTTP server (Fastify) — stays TypeScript
- Auth/JWT handling — stays TypeScript
- Change streamer — stays TypeScript
- CVR Postgres operations — stays TypeScript (I/O-bound, not CPU-bound)
- Poke protocol / wire format — stays TypeScript
- Rewriting pipeline-driver.ts or view-syncer.ts entirely in Rust (orchestration/I/O glue)
- Cold-path utilities: `lite-tables.ts`, `migration-lite.ts`, `replica-schema.ts`, etc.

## Context

- Fork of `rocicorp/mono` at `kartikparsoya-eng/mono-rs`
- Architecture boundary: **TS = orchestration + I/O** (CVR, pokes, auth, locks), **Rust = computation** (SQLite reads, IVM operators, parallel fan-out)
- Hydration bottleneck: N queries hydrate sequentially, each with N\*M child queries from Joins — all on one thread, one SQLite connection
- Advance bottleneck: per-table fan-out works (v2.0) but only evaluates filters — full operator tree (Join/Take/Exists) still in JS
- Global `timeSliceQueue` in view-syncer.ts serializes ALL ViewSyncer yield points process-wide
- napi `Env` is not `Send` — can't create JS objects on Rayon threads, need binary serialization

## Constraints

- **Tech stack**: Rust via napi-rs (must produce Node.js native module)
- **API compatibility**: Rust functions called from existing TS orchestration layer
- **Incremental**: Each phase independently deployable with feature flags
- **Testing**: Existing vitest suites are the primary correctness gate — must pass unchanged
- **No TS test modifications**: D-35 constraint carries forward
- **napi Env limitation**: Rayon threads cannot create JS objects — must use binary/serde serialization

## Key Decisions

| Decision                                       | Rationale                                                                     | Outcome                               |
| ---------------------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------- |
| Use napi-rs over wasm-bindgen                  | Full native access, no wasm overhead, direct SQLite embedding                 | ✓ Good                                |
| Embed SQLite in Rust (not wrap better-sqlite3) | Eliminates JS↔C++ FFI entirely                                                | ✓ Good                                |
| Incremental rewrite (not big bang)             | Each step testable, lower risk, rollback possible                             | ✓ Good                                |
| Keep TS for non-SQLite paths                   | Diminishing returns on rewriting protocol/auth/WebSocket                      | ✓ Good                                |
| rust_fan_out() over rust_advance()             | Fan-out architecture: TS does diff, Rust does parallel eval                   | ✓ Good                                |
| BEGIN CONCURRENT → BEGIN IMMEDIATE rewrite     | Standard rusqlite doesn't support custom SQLite extension                     | ⚠️ Revisit — loses concurrent writers |
| Binary buffer for cross-FFI data               | napi Env not Send, can't create JS objects on Rayon threads                   | — v4.0                                |
| Operator tree in Rust                          | Only way to parallelize hydration fetch (operators do real work during fetch) | — v4.0                                |

## Shipped Milestones

- **v1.0** — Rust SQLite Foundation (2026-04-20): Binary buffer protocol 1.78-1.98x faster, hot read path in Rust
- **v2.0** — IVM Operators in Rust (2026-04-21): Filter, Join, Take, Exists operators, Rayon parallelism
- **v3.0** — Test Coverage & Correctness Hardening (2026-04-21): 139 Rust tests, E2E Docker validation
- **v4.0** — Parallel IVM Runtime (2026-04-29): Full operator tree in Rust, parallel hydration + advance, cross-ViewSyncer parallel poke, binary FFI format, dual-exec correctness harness
- **v5.0** — Streaming + Differential Fuzz (2026-04-30): Per-pipeline streaming (TTFB 1.05× / MemPeak 12.47×), production parity-check shim, OrExists test breadth, fast-check differential fuzz infrastructure (`tools/ivm-parity/`), FUZZ-02 production-shape rich-types schema (jsonb/timestamptz/numeric/i64-boundary), deep-audit Track 2 fixes (B1 Skip ordering, B2 parent_sizes cache poison, B3 partition_key threading, B11 cascade-delete prev snapshot via additive `set_prev_snapshot` napi method preserving CLAUDE.md gate #4)

## Current State

**Latest shipped:** v5.0 Streaming + Differential Fuzz (2026-04-30) — 5 phases, 19 plans, 19 requirements satisfied. Audit status: `tech_debt` (no critical blockers; carry-forward documented in v5.0-MILESTONE-AUDIT.md).

**Currently:** Planning v6.0. Run `/gsd-new-milestone` to set goals and generate fresh REQUIREMENTS.md + Phase 35+ roadmap.

## Next Milestone Goals (v6.0 — TBD)

The carry-forward tech debt from v5.0 splits naturally into two buckets:

**Bucket 1 — v5.0 polish (Phase 35.x style):**
- TS-oracle import resolution (unblocks live 1k fuzz from Phase 34's deferred UAT)
- Deep-audit B5 (OrExists short-circuit), B6 (EXISTS_LIMIT downgrade, security-relevant), B12 (companion scalar drift)
- Code review WR-03 (debug log gating), WR-01/WR-02 fuzz coverage tightening
- 3 pre-existing pipeline-driver.test.ts test failures
- Nyquist VALIDATION.md formal close-out for Phases 31-34

**Bucket 2 — v6.0 net-new capability:**
- **B7 / FlippedJoin operator family** — implement `FlippedJoin` + `UnionFanIn` + `UnionFanOut` operators in Rust. Closes 2 of 5 catalogued PARITY_STATUS.md divergences (fuzz_00132/133 — `OR(simple, EXISTS flip:true)`). Sized as its own phase or v6.x.
- Scalar EXISTS companion resolution (closes 2 more divergences: fuzz_00139/140)
- CI integration of `npm run fuzz-check:gate` (after divergence catalog drains)

See `.planning/v5.0-MILESTONE-AUDIT.md`, `.planning/notes/2026-04-29-phase-34-scope-decision.md`, and `.planning/IVM-PORT-AUDIT-DEEP.md` for full scope.

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):

1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions

**After each milestone** (via `/gsd-complete-milestone`):

1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---

_Last updated: 2026-04-30 after v5.0 Streaming + Differential Fuzz milestone_
