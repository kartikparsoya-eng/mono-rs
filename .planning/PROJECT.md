# zero-cache Rust Rewrite (mono-rs)

## What This Is

A performance-focused partial rewrite of the Zero sync engine's server-side components (zero-cache, zqlite) from TypeScript to Rust via napi-rs. The goal is to eliminate JavaScript runtime bottlenecks — GC pressure, single-threaded execution, and JS↔C++ SQLite FFI overhead — while keeping the existing TypeScript codebase for non-performance-critical paths (WebSocket/HTTP, auth, protocol handling, change streamer).

## Core Value

All SQLite I/O and row-level computation must happen in Rust, delivering 5-10x throughput improvement and eliminating GC pauses at scale.

## Requirements

### Validated

- ✓ Zero sync engine works end-to-end (PG → SQLite → IVM → Client) — existing
- ✓ Comprehensive test suites exist for all rewrite targets — existing
- ✓ Benchmarks exist in `packages/zero-cache/bench/` — existing

### Active

- [ ] Rust napi-rs module replaces `db.ts` + `statement-cache.ts` (Database/Statement API)
- [ ] Rust StatementRunner replaces `statements.ts`
- [ ] Rust TableSource replaces `table-source.ts` (IVM Input interface)
- [ ] Rust DatabaseStorage replaces `database-storage.ts` (IVM Storage interface)
- [ ] Rust Snapshotter replaces `snapshotter.ts` (highest single-target impact)
- [ ] Rust change-log, column-metadata, table-metadata, replication-state replace schema modules
- [ ] Rust ChangeProcessor replaces `change-processor.ts`
- [ ] Rust initial-sync replaces `initial-sync.ts` (bulk loader)
- [ ] Rust pipeline-driver `#advance()` replaces hottest code path (Phase 2)
- [ ] Rust IVM operators (join, take, filter, sort) replace JS generators (Phase 2)
- [ ] Rust CVR store replaces `cvr-store.ts` + `row-record-cache.ts` (Phase 3)
- [ ] All existing vitest tests pass unchanged after swapping TS→Rust imports
- [ ] Rust `#[cfg(test)]` unit tests cover edge cases, memory safety, thread safety
- [ ] Comparative benchmarks show measurable improvement

### Out of Scope

- Client-side code (zero-client, zero-react, etc.) — not performance-critical
- WebSocket/HTTP server (Fastify) — stays TypeScript
- Auth/JWT handling — stays TypeScript
- Change streamer — stays TypeScript
- Cold-path utilities: `lite-tables.ts`, `migration-lite.ts`, `replica-schema.ts`, `backup-monitor.ts`, `explain-queries.ts`, `sqlite-cost-model.ts`, `sqlite-stat-fanout.ts`
- Protocol handling — stays TypeScript

## Context

- Fork of `rocicorp/mono` at `kartikparsoya-eng/mono-rs`
- 5 core bottlenecks identified:
  1. IVM pipeline — CPU-bound single-threaded JS generators
  2. Ser/deser overhead — `Object.fromEntries` per row in `table-source.ts:toSQLiteRow()`
  3. CVR store PG round-trips — `RowRecordCache` papers over this
  4. SQLite snapshot management — `Diff[Symbol.iterator]` crosses FFI per change
  5. GC pressure — 15+ `performance.now()` calls in view-syncer
- `snapshotter.ts` lines 362-370 had 320x perf regression from NULL handling
- `pipeline-driver.ts#advance()` lines 621-715 is the single hottest code path
- 60+ files depend on `db.ts` — must be replaced first

## Constraints

- **Tech stack**: Rust via napi-rs (must produce Node.js native module)
- **API compatibility**: Rust module must exactly match existing TS Database/Statement API
- **Incremental**: Each step must be independently deployable and testable
- **Testing**: Existing vitest suites are the primary correctness gate — must pass unchanged
- **Dependencies**: Critical path is `db.ts → table-source.ts → snapshotter.ts → pipeline-driver.ts`

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Use napi-rs over wasm-bindgen | Full native access, no wasm overhead, direct SQLite embedding | — Pending |
| Embed SQLite in Rust (not wrap better-sqlite3) | Eliminates JS↔C++ FFI entirely | — Pending |
| Incremental rewrite (not big bang) | Each step testable, lower risk, rollback possible | — Pending |
| Keep TS for non-SQLite paths | Diminishing returns on rewriting protocol/auth/WebSocket | — Pending |
| 3-phase approach (SQLite → IVM → CVR) | Follows dependency chain, biggest wins first | — Pending |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd-complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-04-20 after initialization*
