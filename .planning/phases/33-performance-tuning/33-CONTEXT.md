# Phase 33: Production Hardening + Benchmarks - Context

**Gathered:** 2026-04-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Layered defense before shipping the Rust IVM port to production. Five requirements split across two themes:

**Hardening (closes the gap between "tests pass" and "prod-ready"):**

- **HARDEN-01:** Wire `dualExecCompare` into the pipeline-driver hot path so every existing vitest run becomes a TS↔Rust parity check (gated by env var; default off in prod, sample-mode in CI, strict-mode in dedicated parity CI run).
- **HARDEN-02:** Close the OrExists test breadth gap (currently 10 tests vs 44 in `exists_op.rs` — 4.4× behind).

**Benchmarks (deferred from Phases 31/32):**

- **PERF-01:** Rust unit test that fills the bounded `mpsc::sync_channel(pipeline_count.max(1) + 1)` and confirms producer pipelines block until JS pulls.
- **PERF-02:** TTFB microbenchmark — N pipelines with one slow tail; assert time-to-first-chunk ≈ `min(pipeline_time)`, not `max`.
- **PERF-03:** Memory peak benchmark — show streaming reduces peak from `O(total_changes)` to `O(max_pipeline_changes)`.

**In scope:** All 5 requirements above. Bench results recorded to `.planning/milestones/v5.0-bench-results.md`.

**Out of scope (deferred to Phase 34 or post-milestone):**

- Random-AST differential fuzz against TS oracle (Phase 34, FUZZ-01)
- Schema-extension for fuzz harness (Phase 34, FUZZ-02)
- Production shadow mode, prod observability metrics, runbook (post-milestone — operational/deployment work, not codebase work)
- Removing the `ZQLITE_RS_USE_STREAMING_CONSUMER` flag (post-milestone, after one production cycle validates streaming)

</domain>

<decisions>
## Implementation Decisions

### Plan Granularity

- **D-01:** Three plans, all in Wave 1 (parallel — no file overlap):
  - **33-01 (Wave 1, autonomous):** HARDEN-01 — wire dualExecCompare into pipeline-driver.ts under env-var sampling. Modifies `pipeline-driver.ts` (TS) + possibly `dual-executor.ts` (TS). ~5 tasks.
  - **33-02 (Wave 1, autonomous, parallel with 33-01):** HARDEN-02 — expand OrExists test coverage to ≥30 tests. Modifies `or_exists_op.rs` (Rust, tests-only — no production code change). ~3-5 tasks (TDD-paired, but these are RED-only since the implementation already exists from Phase 30-03/30-04).
  - **33-03 (Wave 1, autonomous, parallel with 33-01/02):** PERF-01 + PERF-02 + PERF-03 — all benchmarks together since they share fixture infrastructure. Modifies `pipeline_manager.rs` (Rust, channel-block test) + new bench files + new `.planning/milestones/v5.0-bench-results.md`. ~5-6 tasks.
- **D-02:** All 3 plans run in parallel because they touch disjoint files:
  - 33-01: TS — pipeline-driver.ts, dual-executor.ts
  - 33-02: Rust — or_exists_op.rs (tests block only)
  - 33-03: Rust — pipeline_manager.rs (test additions) + new bench files

### HARDEN-01: dualExecCompare wiring strategy (Option A — always-on with sampling)

- **D-03:** Three-mode env var `ZQLITE_RS_PARITY_CHECK`:
  - `off` (default in prod): no comparison invocations; zero overhead.
  - `sample` (default in CI test runs): comparison runs every Nth invocation (default N=10, configurable via `ZQLITE_RS_PARITY_CHECK_RATE=10`). Divergences are logged via LogContext as `lc.error('parity divergence', { kind, queryId, transformationHash, summary })` and a counter `parityDivergenceCount` is exposed for test assertions.
  - `strict` (dedicated parity-check CI run): comparison runs on every invocation; throws on divergence (test fails immediately).
- **D-04:** The wiring lives at `pipeline-driver.ts::advanceAsync` and `pipeline-driver.ts::hydrateAsync` (and their `*Streaming` siblings if shape allows — TBD per researcher). Implementation pattern: a private helper `#maybeRunParityCheck(rustResult, runTsExpensive: () => Promise<TsResult>)` that branches on the env-var mode and either skips, samples, or always runs the comparison.
- **D-05:** The TS oracle path is invoked via `runTsExpensive` callback so the cost (running TS IVM in parallel) is paid lazily — only when sampling fires. The callback wraps the same TS hydrate/advance logic that `dual-executor.ts` already uses.
- **D-06:** Counter exposure: `getParityDivergenceCount(): number` exported from pipeline-driver.ts. Vitest tests can call it after a test scenario to assert `expect(getParityDivergenceCount()).toBe(0)`. Counter is process-global (single int) for simplicity — acceptable since each test typically runs in its own process or properly isolated context.
- **D-07:** Sampling counter is also process-global. `parityCheckInvocationCount % rate === 0` triggers the comparison. This is deterministic enough for testing but doesn't burden every advance call.
- **D-08:** Default sample rate of N=10 chosen to give 10% overhead in CI. Operators can tune via `ZQLITE_RS_PARITY_CHECK_RATE` if needed. This is a knob, not a fixed value.

### HARDEN-02: OrExists test categories (target ≥30 tests, currently 10)

- **D-09:** Categorize the 44 existing exists_op.rs tests and mirror the relevant ones in or_exists_op.rs. Categories to cover (each gets 2-4 tests):
  - **Fetch:** basic fetch, fetch with constraint, fetch with start/reverse
  - **Push add/remove:** parent add with no children, with children matching, with children not matching, parent remove
  - **Push edit (no or_predicate):** existing behavior unchanged
  - **Push edit (with or_predicate):** all 4 transitions per AUDIT-04 (both pass / old only / new only / neither) — already exist from Phase 30-03 but verify they're properly categorized
  - **Hydrate:** initial hydration with various data shapes
  - **Child push:** child add to parent that has 0 children, child add to parent with existing children, child remove
  - **In-push re-entrancy:** assertion fires when re-entered (already from Phase 30-04 — verify present)
  - **OR branch combinations:** at least 2 tests covering OR over 2+ EXISTS branches with different parent_field
  - **Builder-spec parity:** verify or_exists_op.rs constructed via `ast_to_config` matches the same shape as direct construction
- **D-10:** Use the same test helpers (`force_in_push_for_test`, etc.) added in Phase 30-04. No new helpers needed unless a category requires fresh scaffolding.
- **D-11:** Target test count: ≥30. If reaching 30 requires contrived tests with no real semantic value, stop at 25-28 and document why in the SUMMARY (don't pad the count).

### PERF-01: Channel-blocking unit test

- **D-12:** New Rust unit test in `pipeline_manager.rs` (alongside the existing streaming tests). Pattern: spawn a coordinator with N=4 pipelines that each enqueue chunks via `tx.send`. Don't pull from the receiver. Assert the (N+1)th send blocks (use a `Mutex<Barrier>` or `recv_timeout` to detect the block). Then drain one item and confirm a previously-blocked sender unblocks.
- **D-13:** Test name suggestion: `streaming_channel_bounded_blocks_when_full`. Run under `cargo test --release -p zqlite-rs --lib`.

### PERF-02: TTFB microbenchmark

- **D-14:** New file: `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.ts` (sibling to existing `rust-ivm-bench.ts` if it exists; otherwise the only bench file). Measures TTFB only — separate from the parity fuzz to keep concerns isolated.
- **D-15:** Fixture: 4 pipelines with deterministic delays (e.g., 10ms / 50ms / 100ms / 500ms) using a Proxy-on-prototype pattern (mirror the Phase 31 pattern at `pipeline-driver.streaming.test.ts:265-299`). Assert `firstChunkT < 1.5 * minPipelineDelay` (so for 10ms slowest pipeline = 500ms, first chunk arrives < 15ms).
- **D-16:** Bench output: append a single line to `.planning/milestones/v5.0-bench-results.md` per run with timestamp + measured value + threshold. Format example: `2026-04-29T18:00:00Z TTFB streaming=12ms min_pipeline=10ms ratio=1.2x threshold=1.5x PASS`.
- **D-17:** Bench is part of `vitest` (not a separate runner) so it integrates with the existing test infra. Run via standard `npx vitest run`. Marked with `bench` keyword if vitest's `bench` API is available; otherwise a regular `it.concurrent` block with timing assertions.

### PERF-03: Memory peak benchmark

- **D-18:** Memory measurement via `process.memoryUsage()` polled at ~50ms intervals during the bench workload. Record peak `rss + heapUsed` for both runs (buffered + streaming). Cross-platform, no allocator switch, available in Node by default.
- **D-19:** Reject `jemalloc-stats` for this phase — requires switching the global Rust allocator which changes the released binary characteristics; the difference between buffered/streaming peaks is what matters, and `process.memoryUsage()` captures it adequately for a relative comparison.
- **D-20:** Workload: hydrate 4 pipelines with 2,500 rows each (10K total). Run buffered first (`hydrateAsync`), measure peak; then run streaming (`hydrateStreaming`), measure peak.
- **D-21:** Assertion: `streamingPeak * N <= bufferedPeak * 1.2` where N = pipeline count (allows 20% slack for measurement noise). Specifically for 4 pipelines: `streamingPeak * 4 <= bufferedPeak * 1.2`. If assertion fails by a small margin, log the actual ratio and downgrade to a warning (don't block CI on a flaky perf metric); if it fails by a large margin (>2x), throw.
- **D-22:** Bench output appended to `.planning/milestones/v5.0-bench-results.md` same format as PERF-02 — timestamp, peaks, ratio, PASS/WARN/FAIL.

### Bench Results Storage

- **D-23:** Create `.planning/milestones/v5.0-bench-results.md` at phase start (or via 33-03 Task 1). Format: append-only markdown with one line per bench run. Header explains format.
- **D-24:** Old bench results are NEVER deleted — they're a historical record. Each run appends. PR reviewers can spot regressions by comparing recent lines.

### Build & Verification

- **D-25:** `assertNapiBinaryFreshness` (Phase 30-05) gate covers Phase 33 automatically.
- **D-26:** Verifier MUST run with `ZQLITE_RS_PARITY_CHECK=sample` for the standard CI run to confirm zero divergences in the existing test suite. Plus the strict CI run (`ZQLITE_RS_PARITY_CHECK=strict`) on a sample of pipeline-driver tests to confirm strict mode actually throws on injected divergences.

### Anti-hack guardrails

- **D-27:** HARDEN-01 MUST NOT modify the existing buffered methods' behavior — only ADD a new sampling shim around them. Buffered methods stay byte-for-byte unchanged per Phase 31 COMPAT-02.
- **D-28:** HARDEN-02 MUST NOT modify production code in `or_exists_op.rs` — only ADD tests. The Phase 30-03/30-04 implementations are correct; this plan is pure test addition.
- **D-29:** PERF benchmarks MUST be deterministic enough to run in CI. If a bench is inherently flaky (timing-sensitive in shared CI runners), the assertion threshold should be loose enough to not flake (1.5x for TTFB, 1.2x for memory). Mark genuinely flaky benches with `it.skipIf(process.env.CI)` and log instead.
- **D-30:** No `.skip` additions to existing tests. If a parity-check divergence reveals a real bug, surface as a checkpoint and fix; don't silence.

### Claude's Discretion

- Specific test names within OrExists categories (D-09 lists categories; planner picks individual test names).
- Exact test scaffolding for the channel-block unit test (D-12 specifies the property; planner picks Mutex/Barrier/recv_timeout mechanism).
- Whether to use `rust-ivm-bench.ts` (existing file if present) or a fresh `rust-ivm-streaming-bench.ts` for PERF-02/03.
- Whether to use vitest's `bench` API (if it exists in the installed version) or `it` blocks with timing.
- Format details of the bench-results.md table (markdown table vs append-only log lines).

### Folded Todos

None — no pending todos relevant to this phase.

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Phase 31/32 carry-forward (the streaming infrastructure being benchmarked)

- `.planning/phases/31-streaming-primitives-and-wrappers/31-CONTEXT.md` — D-16 channel sizing decision; D-15 try/finally pattern.
- `.planning/phases/31-streaming-primitives-and-wrappers/31-02-SUMMARY.md` — TS streaming wrappers (advanceStreaming, addQueriesStreaming, RustStreamError).
- `.planning/phases/32-view-syncer-migration/32-02-SUMMARY.md` — `ZQLITE_RS_USE_STREAMING_CONSUMER` flag pattern (mirror for `ZQLITE_RS_PARITY_CHECK`).
- `.planning/phases/32-view-syncer-migration/32-CONTEXT.md` — D-04..D-07 feature-flag-in-constructor pattern (HARDEN-01 should follow same pattern).

### Streaming design (primary source of truth)

- `.planning/IVM-STREAMING-PLAN.md` — §10 Phase D mentions perf tuning; §9 risks (memory, AsyncIterator overhead) inform PERF-02/03.

### Existing dualExecCompare implementation

- `packages/zero-cache/src/services/view-syncer/dual-executor.ts:283` — `dualExecCompare` function. HARDEN-01 wires this into the prod path.
- `packages/zero-cache/src/services/view-syncer/dual-executor.ts` (entire file) — TS oracle harness this phase consumes.

### OrExists parity reference

- `packages/zero-ivm-rs/src/exists_op.rs` — 44 tests. HARDEN-02 mirrors the test categories.
- `packages/zero-ivm-rs/src/or_exists_op.rs` — 10 tests today. Target ≥30.
- `.planning/phases/30-audit-fixes/30-03-SUMMARY.md` — AUDIT-04 transition tests already added; verify presence and gap-fill.
- `.planning/phases/30-audit-fixes/30-04-SUMMARY.md` — debug_assert promotions including or_exists_op.rs:170; verify test coverage.

### Bench infrastructure references

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts:265-299` — Proxy-on-prototype mock-pipeline pattern (PERF-02 reuses this).
- `packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts` — fast-check pattern (NOT used by this phase — Phase 34 owns fuzz; this phase is deterministic benchmarks).
- `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — existing 1k-iter fuzz pattern.

### Project conventions

- `./CLAUDE.md` — ESM, kebab-case, oxlint/oxfmt, Disposable pattern, LogContext for structured logs.
- `./AGENTS.md` — TS optional fields, no `mod.ts` imports, no `import()` in type expressions.
- `.planning/PROJECT.md` — multi-core parallelism is the core value; benchmarks must show this is realized.

### Test convention carry-forward

- `cargo test --release -p zero-ivm-rs` baseline: 170/170 (Phase 31). Must remain green.
- `cargo test --release -p zqlite-rs` baseline: 128/128 + 1 ignored (Phase 31). Must remain green.
- `pipeline-driver.*.test.ts` glob: 13 files / 141 tests (Phase 31). Phase 33 may add ≥1 file (HARDEN-01 dedicated tests) but glob baseline must not regress.
- `streaming-vs-buffered-parity.fuzz.test.ts`: 1k iter pass. Must remain green.

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- **`dualExecCompare` at `dual-executor.ts:283`** — already exists with TS oracle integration. HARDEN-01 wraps a sampling shim around it; no need to rewrite the comparison logic.
- **Phase 31 mock-pipeline Proxy pattern** at `pipeline-driver.streaming.test.ts:265-299` — PERF-02's TTFB benchmark reuses this for staggered-timer fixtures.
- **Phase 30-04 `force_in_push_for_test` helper** in or_exists_op.rs — HARDEN-02 reuses for in-push re-entrancy tests.
- **`assertNapiBinaryFreshness` at `pipeline-driver.ts`** — already covers new napi exports (none added by this phase, but if any are added, gate fires automatically).
- **Existing fuzz harness pattern** at `fuzz-ivm.test.ts` — NOT used by this phase but referenced for Phase 34 (FUZZ-01).

### Established Patterns

- **Env-var feature flag in constructor (Phase 32 D-04..D-07)** — `ZQLITE_RS_PARITY_CHECK` should follow the same pattern (read in PipelineDriver constructor, NOT module top-level, so dual-mode tests work).
- **TDD RED → GREEN per task pair** for tests-with-implementation. HARDEN-02 is RED-only (implementation already exists); PERF benchmarks are TEST + ASSERTION (no separate implementation step).
- **Counter exposure via exported function** for test assertions — mirrors how Phase 31 exposed `PIPELINE_INVOCATION_COUNT` via `force_in_push_for_test`.
- **Bench results in markdown** — append-only format. Project doesn't have a benchmark database; markdown is the historical record.

### Integration Points

- `pipeline-driver.ts` is the SINGLE Rust↔TS boundary HARDEN-01 modifies (no cross-cutting changes).
- `or_exists_op.rs` is the SINGLE Rust file HARDEN-02 modifies (test additions only — no production code).
- New bench files are isolated additions; don't leak into production code paths.
- No view-syncer changes (Phase 32 owns that surface).
- No Rust crate manifest changes expected (vitest, fast-check already in deps; rayon/napi already in scope).

</code_context>

<specifics>
## Specific Ideas

- HARDEN-01 env var name: `ZQLITE_RS_PARITY_CHECK` with values `off|sample|strict`. Sample rate via `ZQLITE_RS_PARITY_CHECK_RATE` (default 10).
- HARDEN-01 counter export: `getParityDivergenceCount(): number` and `resetParityDivergenceCount(): void` — both exported from pipeline-driver.ts.
- HARDEN-02 target: ≥30 tests in or_exists_op.rs (currently 10), categorized per D-09 list.
- PERF-02 fixture: 4 pipelines, 10/50/100/500ms delays, assert TTFB < 1.5×min.
- PERF-03 fixture: 4 pipelines, 2500 rows each, RSS+heap polled at 50ms intervals, assert streaming×N ≤ buffered×1.2.
- Bench results file: `.planning/milestones/v5.0-bench-results.md`, append-only, header explains format.

</specifics>

<deferred>
## Deferred Ideas

- **Random-AST differential fuzz against TS oracle** → Phase 34 (FUZZ-01).
- **Schema extension (jsonb, timestamptz, numeric, NULL semantics, type coercion)** → Phase 34 (FUZZ-02).
- **Production shadow mode** (run Rust IVM alongside TS in prod, log divergences, return TS as truth) → post-milestone (deployment infra, not codebase).
- **Production observability metrics** (`zero_sync_*` divergence counters, p99 latency comparison vs TS baseline) → post-milestone (deployment infra).
- **Runbook + rollback playbook** → post-milestone (operations doc, not phase work).
- **Removing the `ZQLITE_RS_USE_STREAMING_CONSUMER` flag** → post-milestone, after one production cycle.
- **jemalloc allocator switch** for higher-fidelity memory measurement → out of scope (changes binary; relative measurement via `process.memoryUsage()` is sufficient).
- **CI integration of benchmarks** (run on every PR vs nightly vs manual) → ops decision; this phase ships benches that CAN run in CI but doesn't wire them into the GitHub Actions matrix.
- **Removing `advanceAsync` / `addQueriesAsync` / `decodeAdvanceResultBuf`** → NOT in this milestone per IVM-STREAMING-PLAN.md §8.

### Reviewed Todos (not folded)

None — no pending todos relevant to this phase.

</deferred>

---

_Phase: 33-production-hardening-and-benchmarks_
_Context gathered: 2026-04-29_
