# Phase 18: Concurrency Tests - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Prove `rust_advance()` is safe under concurrent Rayon execution — multiple pipelines processed in parallel produce deterministic results, and pipeline set changes between consecutive calls don't corrupt state.

</domain>

<decisions>
## Implementation Decisions

### Concurrency Model

- **D-82:** Focus on Rayon-internal parallelism (the real concurrency). `rust_advance` is synchronous/blocking from Node's perspective — no true call-level overlap without worker_threads.
- **D-83:** No global/static state exists in Rust — function is purely functional. True concurrency is multiple Rayon threads reading shared `Arc<changes>` while processing different pipelines simultaneously.

### Determinism Assertion

- **D-84:** Two-pronged assertion strategy:
  1. **Set equality** — sort output changes by (query_id, primary key) before comparison, proving order-independence from thread scheduling.
  2. **Stability (repeated runs)** — run same advance 50x, assert all results identical. Catches rare races.

### Pipeline Mutation

- **D-85:** Test at TS integration level — add/remove pipelines between consecutive `rust_advance()` calls. Verify:
  - New pipeline added between calls gets correct results on next cycle
  - Removed pipeline's absence doesn't cause errors or stale output
- **D-86:** No mid-advance mutation testing needed — the function is stateless (pipeline list is immutable once deserialized from JSON).

### Test Organization

- **D-87:** Two test locations:
  1. Rust unit tests in `advance.rs` — repeated `par_iter` runs with many pipelines, verify determinism (50x iterations)
  2. Vitest integration: `pipeline-driver.concurrency.test.ts` — TS-level pipeline mutation between calls
- **D-88:** Keep iteration count moderate (50x) so tests stay under 5s.

### Carried Forward

- **D-35:** No test modifications — new tests in new files only
- **D-44:** No modifications to `packages/zql/`
- **D-80:** Dual-path comparison via `ZERO_DISABLE_RUST_IVM` toggle (for TS integration tests)

### Claude's Discretion

- Number of pipelines per test (suggest 10+ to exercise parallelism)
- Specific filter/operator configurations for pipeline variety
- Whether to add a timeout assertion (e.g., test must complete within 10s to detect deadlocks)

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Rust Advance Implementation

- `packages/zqlite-rs/src/advance.rs:379-505` — `rust_advance` function, Rayon fan-out at line 486-491
- `packages/zqlite-rs/src/advance.rs:507+` — existing Rust unit tests (25 tests)

### Pipeline Driver Integration

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — TS caller of `rust_advance()`
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.fixtures.ts` — shared test helpers from Phase 17

### Requirements

- `.planning/REQUIREMENTS.md` — CON-01, CON-02 acceptance criteria

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `pipeline-driver.fixtures.ts` — shared schemas, ASTs, `createTestDB()` helper
- Existing 25 Rust advance tests — patterns for setting up test DBs in Rust

### Established Patterns

- `Arc::new(changes)` + `par_iter()` for fan-out (advance.rs:487-491)
- No global state — purely functional, all inputs via parameters
- JSON serialization for cross-boundary communication

### Integration Points

- `rust_advance()` called from `pipeline-driver.ts` with `pipeline_configs_json` parameter
- Pipeline list constructed fresh each advance cycle (no caching between calls)

</code_context>

<specifics>
## Specific Ideas

- 50x repeated runs for stability testing (catches 1-in-50 race conditions)
- 10+ pipelines per test to ensure Rayon actually parallelizes (thread pool won't bother for <4 items)
- Sort by (query_id, pk) for deterministic comparison
- Timeout assertion (10s) as deadlock detection proxy

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

_Phase: 18-concurrency-tests_
_Context gathered: 2026-04-21_
