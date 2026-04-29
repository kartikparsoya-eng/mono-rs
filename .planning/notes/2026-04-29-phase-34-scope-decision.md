---
type: scope-decision
phase: 34
decided: 2026-04-29
decided_by: user
---

# Phase 34 Scope Decision: Two-Cache Differential Fuzz (Option A)

## Decision

**Phase 34's random-AST differential fuzz (FUZZ-01) and schema extension (FUZZ-02) will be built by extending the existing `tools/ivm-parity/` two-process harness, NOT by building a new in-process vitest fuzz that imports upstream TS as a library.**

## Why this option (A) over the alternative (B)

Option A (chosen): Bolt fast-check + rich-types schema onto `tools/ivm-parity/`'s existing two-cache setup (TS zero-cache on :4858, Rust zero-cache with `ZERO_USE_RUST_IVM_V2=1` on :4868, same Postgres on :6434).

Option B (rejected): Build new in-process random-AST fuzz inside vitest using TS imports + Rust calls side-by-side.

Option A wins because:

1. Sidesteps mono-rs's architectural problem that broke Phase 33 HARDEN-01 strict-mode advance: `tsAdvance` returns empty in mono-rs because there is no surviving TS pipeline state in-process. The two-process harness keeps TS in its native habitat.
2. The infrastructure already exists — `ast-fuzz.ts`, `harness.ts`, `harness-advance-coverage.ts`, golden-snapshot mode, `ast_corpus.json`, divergence reports. Currently 1079/1084 parity OK with 5 known triaged divergences.
3. Reuses real `zero-cache` binaries on both sides — true integration test, not a unit-level oracle.
4. Lower implementation risk for the same correctness coverage.

## What Phase 34 must add on top of `tools/ivm-parity/`

1. **fast-check shrinking layer** — currently the corpus is bounded/curated (`ast_corpus.json`, `_gen_prod_*.py`). Add a fast-check generator that produces random ASTs across the full operator surface (filter, join, exists, or-exists, take, cap) and feeds them through the same two-cache harness. `FUZZ_NUM_RUNS=1000` default. fast-check's shrinking gives minimal counterexamples.
2. **Schema extension (FUZZ-02)** — `seed.sql` / `schema.sql` are basic-types only. Add jsonb, timestamptz, numeric, nullable variants, at least one composite/array column. New seed data must cover NULL semantics (NULL ≠ NULL in equality, NULL propagation, NULL in JOIN keys) and type coercion (string→number, numeric precision, date/time round-trips).
3. **CI integration** — currently ad-hoc `npm test` in `tools/ivm-parity/`. Phase 34 should land this as a phase gate or CI step. Decide format during discuss-phase.
4. **Resolution path for the 5 open divergences** in `PARITY_STATUS.md`:
   - 2× FlippedJoin (`OR(simple, EXISTS flip=true)`) — needs FlippedJoin operator in Rust
   - 2× scalar EXISTS companion resolution — needs scalar companion in Rust pipeline manager
   - 1× duplicate CSQ alias overwrite — documented "won't fix"

   Phase 34 does NOT need to fix these — but the discuss-phase should decide whether they are blockers for the milestone or carry-forward to v6.0.

## Carry-over context to feed `/gsd-discuss-phase 34`

- The TS oracle is the upstream reference repo at `/private/tmp/ivm-parity-ts-ref` (a git worktree of `rocicorp/mono`).
- mono-rs Rust IVM is gated by env var `ZERO_USE_RUST_IVM_V2=1`.
- Both caches must point at the same Postgres for diff to be valid — `tools/ivm-parity/run.sh` already orchestrates this.
- Existing fast-check in the codebase: `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` (filter-only, line 342 has `test.todo` slot for the random-AST extension), `streaming-vs-buffered-parity.fuzz.test.ts` (Rust-internal, not TS-vs-Rust). Do NOT confuse these with FUZZ-01.

## Open questions for discuss-phase

1. fast-check shrinking inside the harness vs. outside? (Inside requires JS interop with `harness.ts`; outside means corpus pre-generation.)
2. CI integration: GitHub Action with PG service container, or just a phase gate that runs locally?
3. How to handle non-deterministic PG ordering in the diff — is the existing harness already canonicalizing, or do we need to extend `compareChanges` semantics?
4. Should the 5 open divergences in `PARITY_STATUS.md` block v5.0 ship, or are they carry-forward?
