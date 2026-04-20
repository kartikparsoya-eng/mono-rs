# Roadmap: zero-cache Rust Rewrite

## Completed Milestones

- [x] **v1.0 — Rust SQLite Foundation** (2026-04-20) — [Archive](.planning/milestones/v1.0-ROADMAP.md)

## Current Milestone: v2.0 — IVM Operators in Rust

### Phase 10: Rust Filter + Take Operators

**Goal:** Port simplest IVM operators to Rust via napi-rs, proving the operator boundary design.

**Approach:** Each Rust operator implements same Input/Output interface via thin TS wrapper. SourceChange in → Change[] out. Take uses in-memory HashMap (no SQLite JSON.parse/stringify). Unsupported cases fall back to TS path.

**Key deliverables:**
1. ~~Rust Filter operator (stateless predicate evaluation)~~ ✅ Plan 10-01
2. ~~Rust Take operator (in-memory storage, bound tracking)~~ ✅ Plan 10-02
3. ~~TS wrapper classes delegating to napi-rs~~ ✅ Plan 10-03
4. All existing filter.test.ts and take.test.ts pass unchanged ✅

**Plans:**
3/3 plans complete
- [x] Plan 10-02: Rust Take state machine (`1a384a18d`)
- [x] Plan 10-03: Separate IVM crate + delegate integration (`ba5fe45d0`...`d384a1e76`)

**Success criteria:** Operator tests pass unchanged, benchmark shows improvement over TS path

Depends on: v1.0 (zqlite-rs foundation)

### Phase 11: Rust Join Operator

**Goal:** Port Join operator — biggest perf bottleneck (3.56x slower than Filter).

**Approach:** Direct rusqlite reads for child fetch (no napi round-trip per parent row). Overlay logic in Rust. Handles all 4 change types (add/remove/edit/child).

**Success criteria:** join.test.ts passes unchanged, pipeline-driver.test.ts passes unchanged

Depends on: Phase 10

### Phase 12: Rust Exists Operator

**Goal:** Port Exists operator (correlated subquery with cache).

**Approach:** Builds on Join pattern (fetches relationship size). Cache logic in Rust.

**Success criteria:** exists.test.ts passes unchanged

Depends on: Phase 11

### Phase 13: Rayon Parallelism for Fan-out

**Goal:** Different connections run on different threads via Rayon.

**Approach:** No shared mutable state between connections (overlay is per-source, set before iterating). Rayon par_iter over connection set.

**Success criteria:** pipeline-driver.test.ts passes unchanged, 2-4x speedup on multi-connection workloads

Depends on: Phase 12

### Phase 14: Integration + Edge Case Testing

**Goal:** End-to-end validation, performance regression suite, documentation.

Depends on: Phase 13
