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

### Active

- [ ] Rust Operator trait with full fetch() + push() (pipeline builder in Rust)
- [ ] Rust TableSource with SQLite connection pool (parallel reads)
- [ ] Parallel multi-pipeline hydration via Rayon
- [ ] Within-pipeline child parallelism (Join fan-out)
- [ ] Full operator tree advance in Rust (replaces filter-only rust_fan_out)
- [ ] Binary serialization format for Rust → TS row transfer
- [ ] Pipeline-driver TS integration (addQuery → Rust, advance → Rust)
- [ ] Cross-ViewSyncer parallel poke dispatch
- [ ] E2E validation + benchmark suite

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

## Current Milestone: v4.0 Parallel IVM Runtime

**Goal:** Move the full IVM operator tree to Rust — hydration and advance both execute entirely in Rust with Rayon parallelism across pipelines and within Join fan-outs. TS becomes pure orchestration/I/O.

**Target features:**

- Full Operator trait (fetch + push) for all IVM operators in Rust
- SQLite connection pool for parallel reads
- N-pipeline parallel hydration (wall clock ≈ max(single query) not sum(all))
- Within-pipeline child query parallelism (M relationships fetched in parallel)
- Full operator tree advance (replaces filter-only rust_fan_out)
- Cross-ViewSyncer parallel poke dispatch
- Binary serialization format for Rust → TS transfer

---

_Last updated: 2026-04-21 after v3.0 milestone completion_
