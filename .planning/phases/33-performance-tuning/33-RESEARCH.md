# Phase 33: Production Hardening + Benchmarks — Research

**Researched:** 2026-04-29
**Domain:** Production hardening (parity sampling) + streaming perf benchmarks (TTFB, memory peak, channel bound)
**Confidence:** HIGH (CONTEXT is prescriptive; this research surfaces only implementation specifics + 4 calibration corrections)

## Summary

CONTEXT.md is fully prescriptive (30 locked decisions). Three plans run in parallel touching disjoint files: 33-01 (HARDEN-01 TS sampling shim in pipeline-driver.ts), 33-02 (HARDEN-02 OrExists tests in or_exists_op.rs), 33-03 (PERF-01/02/03 benchmarks). This research surfaces:

1. **Library specifics** — vitest 4.1.3 supports `bench` API (already used by `pipeline-driver.bench.ts`); `mpsc::sync_channel` blocking test pattern; `process.memoryUsage()` polling pattern.
2. **Existing infrastructure already done** — `dualExecCompare` exists at `dual-executor.ts:283` with the exact (label, RowChange[], RowChange[], lc) → RowChange[] signature; `materializeChanges` already imported in pipeline-driver.ts (line 49); the existing `pipeline-driver.test.ts` already sets `ZERO_DUAL_EXEC=strict` but no production call site reads it. **HARDEN-01 is the first call-site wiring.**
3. **Number corrections** — `exists_op.rs` has 22 tests (not 44 as CONTEXT D-09 states); `or_exists_op.rs` has 5 tests (not 10 as CONTEXT D-11/specifics state). Target ≥30 still applies; the gap is wider than CONTEXT assumed.
4. **Critical pitfall** — the existing `ZERO_DUAL_EXEC` env var has the same off/log/strict semantics CONTEXT proposes for `ZQLITE_RS_PARITY_CHECK`. The two will overlap functionally. Document this for the planner; CONTEXT D-03 locks the new name.
5. **`chunk-end` sentinel** — Phase 32 added `'chunk-end'` to the streaming AsyncIterable. PERF-02 TTFB benchmark MUST filter both `'yield'` AND `'chunk-end'` when measuring first-RowChange arrival, mirroring the existing pattern in `pipeline-driver.streaming.test.ts` and `streaming-vs-buffered-parity.fuzz.test.ts`.

**Primary recommendation:** Plan 33-01 wires the sampling shim into `pipeline-driver.ts::#rustAdvanceAsync` (line 2019) and `addQueriesAsync` (line ~996) — NOT into `advanceStreaming` / `addQueriesStreaming`, because (a) CONTEXT D-04 names `advanceAsync` / `hydrateAsync` explicitly as the wiring sites, and (b) the streaming path already cancels mid-flight and is harder to wrap atomically. The buffered path is what the existing test suite exercises, so the parity check piggybacks on it.

## User Constraints (from CONTEXT.md)

### Locked Decisions

**Plan Granularity (D-01, D-02):** Three plans, all Wave 1 parallel:

- **33-01:** HARDEN-01 — TS sampling shim in `pipeline-driver.ts` (~5 tasks)
- **33-02:** HARDEN-02 — OrExists test expansion in `or_exists_op.rs` (~3-5 tasks, RED-only since impl exists)
- **33-03:** PERF-01 + PERF-02 + PERF-03 — channel-block test + TTFB bench + memory bench (~5-6 tasks)

**HARDEN-01 sampling strategy (D-03..D-08):**

- Env var `ZQLITE_RS_PARITY_CHECK` ∈ `off|sample|strict` (default `off`)
- Sample rate via `ZQLITE_RS_PARITY_CHECK_RATE` (default `10` → 10% overhead)
- `sample` mode logs divergences via `lc.error(...)` and increments `parityDivergenceCount`
- `strict` mode throws on divergence
- Wiring lives at `pipeline-driver.ts::advanceAsync` and `pipeline-driver.ts::hydrateAsync` (and `*Streaming` siblings if shape allows — researcher to confirm)
- Implementation pattern: `#maybeRunParityCheck(rustResult, runTsExpensive: () => Promise<TsResult>)` private helper
- TS oracle invoked via `runTsExpensive` callback (lazy — only when sampling fires)
- Counter exported via `getParityDivergenceCount(): number` and `resetParityDivergenceCount(): void`
- Process-global counter (single int) for simplicity

**HARDEN-02 categories (D-09..D-11):** Categorize the existing exists_op.rs tests and mirror in or_exists_op.rs. Categories (each gets 2-4 tests):

- Fetch (basic, with constraint, with start/reverse)
- Push add/remove (parent add no children, with children matching/not, parent remove)
- Push edit (no or_predicate / with or_predicate — 4 transitions per AUDIT-04, already exist)
- Hydrate (initial with various data shapes)
- Child push (child add to parent with 0 children / with existing / child remove)
- In-push re-entrancy (already exists from Phase 30-04)
- OR branch combinations (≥2 tests covering OR over 2+ EXISTS branches)
- Builder-spec parity (`ast_to_config` vs direct construction)

Use existing `force_in_push_for_test` helper. Target ≥30; if hitting 30 requires contrived tests, stop at 25-28 and document in SUMMARY.

**PERF-01 (D-12, D-13):** New Rust unit test in `pipeline_manager.rs`. Pattern: spawn coordinator with N=4 pipelines that enqueue chunks via `tx.send`. Don't pull from receiver. Assert (N+1)th send blocks. Drain one item; confirm previously-blocked sender unblocks. Test name: `streaming_channel_bounded_blocks_when_full`. Run via `cargo test --release -p zqlite-rs --lib`.

**PERF-02 (D-14..D-17):**

- New file: `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.ts` (sibling to `rust-ivm-bench.ts`)
- Fixture: 4 pipelines with delays 10/50/100/500ms via Proxy-on-prototype pattern (mirror `pipeline-driver.streaming.test.ts:265-299`)
- Assert `firstChunkT < 1.5 * minPipelineDelay`
- Result appended to `.planning/milestones/v5.0-bench-results.md` per run
- Use vitest `bench` API if available (it is — see `pipeline-driver.bench.ts`), else `it.concurrent` with timing assertions

**PERF-03 (D-18..D-22):**

- Memory measurement via `process.memoryUsage()` polled every ~50ms
- Record peak `rss + heapUsed` for both buffered + streaming runs
- Reject `jemalloc-stats` (changes binary)
- Workload: 4 pipelines × 2,500 rows = 10K total
- Assertion: `streamingPeak * N <= bufferedPeak * 1.2` (4 pipelines: `streamingPeak * 4 <= bufferedPeak * 1.2`)
- Small-margin failure → log + downgrade to WARN; large margin (>2x) → throw
- Output appended same format as PERF-02

**Bench results storage (D-23, D-24):** Create `.planning/milestones/v5.0-bench-results.md` at phase start (or via 33-03 Task 1). Append-only, NEVER deleted. Header explains format.

**Build & verification (D-25, D-26):**

- `assertNapiBinaryFreshness` (Phase 30-05 gate) covers Phase 33
- Verifier MUST run with `ZQLITE_RS_PARITY_CHECK=sample` for standard CI; plus `strict` on a sample of pipeline-driver tests to confirm strict mode actually throws on injected divergences

**Anti-hack guardrails (D-27..D-30):**

- HARDEN-01 MUST NOT modify existing buffered methods' behavior — only ADD a sampling shim around them
- HARDEN-02 MUST NOT modify production code in `or_exists_op.rs` — only ADD tests
- PERF benches MUST be deterministic enough for CI; flaky benches use `it.skipIf(process.env.CI)`
- NO `.skip` additions to existing tests

### Claude's Discretion

- Specific test names within OrExists categories (D-09 lists categories; planner picks names)
- Exact channel-block test scaffolding (Mutex/Barrier vs `recv_timeout` — see §5.1 below)
- Whether to use existing `rust-ivm-bench.ts` or fresh `rust-ivm-streaming-bench.ts`
- Whether to use vitest `bench` API or `it` blocks (vitest 4.1.3 supports `bench` — recommend it)
- Format details of `bench-results.md` (markdown table vs append-only log lines)

### Deferred Ideas (OUT OF SCOPE)

- Random-AST differential fuzz against TS oracle → Phase 34 (FUZZ-01)
- Schema-extension for fuzz harness → Phase 34 (FUZZ-02)
- Production shadow mode, prod observability metrics, runbook → post-milestone
- Removing `ZQLITE_RS_USE_STREAMING_CONSUMER` flag → post-milestone
- jemalloc allocator switch → out of scope (changes binary)
- CI integration (which workflow runs benches) → ops decision
- Removing `advanceAsync` / `addQueriesAsync` / `decodeAdvanceResultBuf` → NOT in milestone

## Phase Requirements

| ID        | Description                                                                            | Research Support                                                             |
| --------- | -------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| HARDEN-01 | Wire dualExecCompare into pipeline-driver hot path under `ZQLITE_RS_PARITY_CHECK` flag | §1 (existing dualExecCompare signature + call sites), §2.1 (env-var pattern) |
| HARDEN-02 | Expand or_exists_op.rs tests to ≥30                                                    | §4 (test categorization, gap analysis: 5 → 30)                               |
| PERF-01   | Rust unit test fills bounded mpsc channel; producer pipelines block until JS pulls     | §5.1 (mpsc::sync_channel test pattern), §1.4 (existing channel sizing)       |
| PERF-02   | TTFB microbench: N pipelines with one slow tail; first-chunk ≈ min(pipeline_time)      | §5.2 (Proxy-on-prototype timer fixture), §1.5 (vitest bench API)             |
| PERF-03   | Memory peak: streaming reduces O(total_changes) → O(max_pipeline_changes)              | §5.3 (process.memoryUsage polling pattern, peak measurement)                 |

---

## 1. Existing Code Reference Points

### 1.1 `dualExecCompare` — already exists, ready to call

**Location:** `packages/zero-cache/src/services/view-syncer/dual-executor.ts:283`

**Signature:**

```typescript
export function dualExecCompare(
  label: string, // e.g. 'advance' | 'hydrate'
  tsChanges: RowChange[], // pre-materialized
  rustChanges: RowChange[], // pre-materialized
  lc: LogContext,
): RowChange[]; // returns TS as source of truth
```

**Behavior:**

- Calls `compareChanges(tsChanges, rustChanges)` (line 292) to produce `CompareResult` (`{match, tsCount, rustCount, mismatches[]}`)
- On match: `lc.debug?.(...)` and returns `tsChanges`
- On mismatch:
  - Increments `stats.mismatches` and `dualExecMismatches` OTel counter
  - If `isDualExecStrict()` → `lc.error?.(detail)` then `throw new DualExecMismatchError(detail)`
  - Else → `lc.warn?.(detail)` and returns `tsChanges`

**Critical insight for HARDEN-01:**
The function takes BOTH arrays pre-materialized — caller must run TS path and Rust path independently and pass both results in. There's no internal "run both" mode. So the sampling shim in pipeline-driver.ts pattern is:

```typescript
// Inside the post-Rust-decode block:
const rustChanges = materializeChanges(this.#convertDispatchChanges(decoded.changes));
if (this.#shouldRunParityCheck()) {
  const tsChanges = await this.#runTsOracle(...);  // re-run TS path against same input
  dualExecCompare('advance', tsChanges, rustChanges, this.#lc);
}
return rustChanges;  // Rust is the production path; TS is just an oracle
```

Note: `dualExecCompare` returns the TS array, but in HARDEN-01 we want to KEEP using the Rust result (it's the production output). Either ignore the return value or wrap it.

[VERIFIED: file read at dual-executor.ts:283-316]

### 1.2 `materializeChanges` — already imported into pipeline-driver.ts

**Location:** Imported at `pipeline-driver.ts:49`. Used at lines 2071 and 2148. Available for the sampling shim with no new import.

```typescript
export function materializeChanges(
  iterable: Iterable<RowChange | 'yield'>,
): RowChange[];
```

[VERIFIED: pipeline-driver.ts:49 grep result]

### 1.3 Pipeline-driver hot-path call sites

**For HARDEN-01 wiring**, four target sites (CONTEXT D-04 names `advanceAsync` and `hydrateAsync` — note hydrate is via `addQueriesAsync` and per-query `#rustHydrateQuery`):

| Method                | File / Line                                                          | Returns                                                         |
| --------------------- | -------------------------------------------------------------------- | --------------------------------------------------------------- | ------------------------------------------------------ | --------------- |
| `advanceAsync`        | `pipeline-driver.ts:1795`                                            | `Promise<{version, numChanges, changes: Iterable<RowChange      | 'yield'>}>`(delegates to`#rustAdvanceAsync` line 2019) |
| `addQueriesAsync`     | `pipeline-driver.ts:~840` (the rust batch path is at line ~991-1010) | `Promise<Iterable<RowChange                                     | 'yield'>>`                                             |
| `advanceStreaming`    | `pipeline-driver.ts:1867`                                            | `Promise<{version, numChanges, changes: AsyncIterable<RowChange | 'yield'                                                | 'chunk-end'>}>` |
| `addQueriesStreaming` | `pipeline-driver.ts:1117`                                            | `Promise<AsyncIterable<RowChange                                | 'yield'                                                | 'chunk-end'>>`  |

**Recommended HARDEN-01 wiring sites (per D-04):** `#rustAdvanceAsync` (line 2019) and the `addQueriesAsync` Rust batch path (line ~996).

**Why NOT the streaming variants:**

- The streaming wrapper would have to buffer the full Rust result in memory to compare to TS, defeating the streaming purpose for the sampled path.
- Sampling fires only every Nth call (default 10%); paying the buffer cost on those occasions is acceptable.
- The buffered methods are still on Phase 32's fallback path (`ZQLITE_RS_USE_STREAMING_CONSUMER=false`), so the parity check covers operator rollback verification too.

[VERIFIED: file read at pipeline-driver.ts:1795, 1867, 2019, 996, 1117]

### 1.4 Existing streaming channel sizing (PERF-01 context)

**Location:** `pipeline_manager.rs:428` (advance_streaming), `:544` (hydrate_streaming), `:614` (hydrate_query_streaming)

```rust
let (tx, rx) = mpsc::sync_channel::<StreamItem>(pipeline_count.max(1) + 1);
```

**Phase 31 note:** the `+1` was added beyond CONTEXT D-16's literal `pipeline_count` to guarantee a slot for the post-scope ResetSignal/companion-Chunk so the coordinator never blocks. PERF-01 test must respect this — to make the channel block, you must enqueue `pipeline_count + 2` items (or more), not `pipeline_count + 1`.

[VERIFIED: pipeline_manager.rs:428, line :388-524 read]

### 1.5 Vitest 4.1.3 `bench` API support — confirmed

`packages/zero-cache/package.json:92` pins vitest 4.1.3. `pipeline-driver.bench.ts:19` already uses `import {bench, describe} from 'vitest'`. Vitest exposes `bench` (single iteration timing), `bench.skip`, `bench.only`, and `describe.bench` at runtime.

**Vitest bench config:** `packages/zero-cache/vitest.config.bench.ts` exists. Sets `ZERO_DISABLE_RUST_IVM=1`. PERF-02/03 may need a SEPARATE config (or override) because they need RUST IVM ENABLED — recommend a new config file `vitest.config.streaming-bench.ts` if needed, or set the env per-file at top.

**Run command (pattern from existing benches):**

```bash
npx vitest bench --config vitest.config.bench.ts <bench-file>
```

**For PERF-02/03 specifically** (need Rust IVM enabled): write the bench files such that they explicitly DELETE `process.env.ZERO_DISABLE_RUST_IVM` at top of file, before any pipeline-driver imports — same pattern as `pipeline-driver.bench.ts:16` does in reverse.

[VERIFIED: package.json:92 + pipeline-driver.bench.ts:19, vitest.config.bench.ts read]

### 1.6 Phase 32 `chunk-end` sentinel

Streaming methods now yield `'yield' | 'chunk-end' | RowChange`. PERF-02 TTFB measurement MUST count first non-string item:

```typescript
let firstChunkT: number | undefined;
const start = performance.now();
for await (const c of streamingResult.changes) {
  if (c !== 'yield' && c !== 'chunk-end') {
    if (firstChunkT === undefined) firstChunkT = performance.now() - start;
    break;
  }
}
```

Pattern verified in `pipeline-driver.streaming.test.ts:206-211, :334`, `streaming-vs-buffered-parity.fuzz.test.ts`. [VERIFIED]

### 1.7 OrExists test count truth

| File            | CONTEXT-claimed count | Actual count (verified)   |
| --------------- | --------------------- | ------------------------- |
| exists_op.rs    | 44                    | **22** (mirror reference) |
| or_exists_op.rs | 10                    | **5** (gap target)        |

Verified via `cargo test --release --lib exists_op:: -- --list` (22 tests) and `cargo test --release --lib or_exists_op:: -- --list` (5 tests).

**Gap is wider than CONTEXT estimated.** Plan 33-02 still targets ≥30 tests in or_exists_op.rs (need to add 25 new), but mirroring exists_op.rs categories will produce ~22 + ~10 fresh OrExists-specific tests. The "if reaching 30 requires contrived tests, stop at 25-28" escape clause (D-11) likely binds — recommend planner aim for 25-28 and document.

[VERIFIED: cargo test --list output]

### 1.8 Existing parity-check test infra

`pipeline-driver.test.ts:50` already sets `process.env['ZERO_DUAL_EXEC'] = 'strict'` in `beforeAll` and clears in `afterAll`. **However** — `dualExecCompare` is called from NO production code path (only from `streaming-vs-buffered-parity.fuzz.test.ts:265` which uses it as a library function for streaming-vs-buffered comparison, NOT TS-vs-Rust).

**Implication for HARDEN-01:** the existing `ZERO_DUAL_EXEC=strict` setup in `pipeline-driver.test.ts` becomes load-bearing AFTER HARDEN-01 wires the call. Until HARDEN-01 ships, that env var is dead. After HARDEN-01, those existing tests start exercising the parity check on every advance/hydrate.

**Decision point for planner (Claude's Discretion zone):** Should `ZQLITE_RS_PARITY_CHECK` be a NEW env var (per CONTEXT D-03 — locked) or unify with `ZERO_DUAL_EXEC`? Per CONTEXT D-03, NEW name. But the planner should be aware that `pipeline-driver.test.ts:50` sets `ZERO_DUAL_EXEC=strict`, and may want to ALSO set `ZQLITE_RS_PARITY_CHECK=strict` in the same `beforeAll` block.

[VERIFIED: pipeline-driver.test.ts:48-54, dualExecCompare grep results]

---

## 2. Library / Pattern Specifics

### 2.1 Env-var feature flag pattern (Phase 32 D-04..D-07 carry-forward)

**Pattern (from `view-syncer.ts:355-365`):** Read once in instance constructor, NOT module top-level:

```typescript
constructor(...) {
  // Read once per instance (NOT module top-level — module-init read locks
  // the value at first import, breaking dual-mode tests that flip the flag
  // in beforeEach. Per-instance read preserves "read once at boot"
  // semantics at the granularity that matters.)
  this.#parityCheckMode = parseParityCheckMode(
    process.env['ZQLITE_RS_PARITY_CHECK'] ?? 'off'
  );
  this.#parityCheckRate = parseInt(
    process.env['ZQLITE_RS_PARITY_CHECK_RATE'] ?? '10',
    10
  );
}
```

**For HARDEN-01:** Add similar reads to `PipelineDriver` constructor (line 396-419). Pattern matches Phase 32's view-syncer constructor. Helper:

```typescript
function parseParityCheckMode(env: string): 'off' | 'sample' | 'strict' {
  if (env === 'sample' || env === 'strict') return env;
  return 'off'; // unknown / empty / 'off' / 'false' all map to off
}
```

**Anti-pattern (P-03 from Phase 32):** Reading at module top-level breaks tests that set the env var in `beforeAll`/`beforeEach` because the read already happened at import time. The constructor pattern fixes this.

[CITED: view-syncer.ts:355-365 verbatim]

### 2.2 Process-global counter pattern

**Pattern (Phase 31 used `PIPELINE_INVOCATION_COUNT` AtomicUsize for test injection in advance.rs):** The TS equivalent for HARDEN-01 is a module-level counter:

```typescript
// pipeline-driver.ts (near top, after imports)
let parityDivergenceCount = 0;
let parityCheckInvocationCount = 0;

export function getParityDivergenceCount(): number {
  return parityDivergenceCount;
}

export function resetParityDivergenceCount(): void {
  parityDivergenceCount = 0;
}
```

Module-level state is acceptable here because: (a) tests reset between cases via `resetParityDivergenceCount()`, (b) it's per-process (matching the env var's per-process scope), (c) JS is single-threaded so no atomicity concerns.

**Counter increment:**

```typescript
parityCheckInvocationCount++;
if (parityCheckInvocationCount % rate === 0) {
  // sample fires
}
```

[VERIFIED: pattern from advance.rs `PIPELINE_INVOCATION_COUNT`; CONTEXT D-06/D-07]

### 2.3 vitest `bench` API — basic shape

```typescript
import {bench, describe} from 'vitest';

describe('TTFB streaming bench', () => {
  bench(
    '4 pipelines, 10/50/100/500ms delays',
    async () => {
      // single benchmark iteration
      const result = await pipelines.advanceStreaming(timer);
      let firstChunkT: number | undefined;
      const start = performance.now();
      for await (const c of result.changes) {
        if (c !== 'yield' && c !== 'chunk-end') {
          firstChunkT = performance.now() - start;
          break;
        }
      }
      // bench mode auto-times; assertion lives outside or in afterAll
    },
    {iterations: 10, time: 5000},
  );
});
```

**Caveat:** vitest's `bench` API is for STATISTICAL TIMING (mean/p50/p99). It does NOT support custom per-iteration assertions like a regular test. For PERF-02/03 where we want both timing AND assertion + log to file, recommend using `test()` (or `it()`) blocks with explicit `performance.now()` measurement, NOT `bench()`. The `bench()` API is appropriate if you want vitest to run the workload N times and print a histogram — useful for tracking trends but not for pass/fail gates.

**Recommendation:** Use `test()` with manual timing for PERF-02/03 — gives full control over assertion shape + log-to-file. Use `bench()` only if separately desired for trend tracking.

[CITED: vitest bench docs + pipeline-driver.bench.ts:19, 327 usage]

### 2.4 `mpsc::sync_channel` blocking test pattern (PERF-01)

**Idiomatic Rust pattern using `recv_timeout` + thread:**

```rust
#[test]
fn streaming_channel_bounded_blocks_when_full() {
    use std::sync::mpsc::{sync_channel, TrySendError};
    use std::thread;
    use std::time::Duration;

    // Replicate the production sizing: pipeline_count = 4 → channel capacity 5
    let pipeline_count = 4;
    let (tx, rx) = sync_channel::<u32>(pipeline_count.max(1) + 1);  // capacity 5

    // Fill the channel to capacity (5 sends should succeed without blocking)
    for i in 0..5 {
        tx.try_send(i).expect("send within capacity must succeed");
    }

    // The 6th send must fail with Full because the channel is full
    match tx.try_send(99) {
        Err(TrySendError::Full(_)) => {} // expected
        other => panic!("expected Full, got {:?}", other),
    }

    // Drain one item — confirms a previously-blocked sender would unblock
    assert_eq!(rx.recv().unwrap(), 0);

    // Now the 6th send should succeed
    tx.try_send(99).expect("send after drain must succeed");
}
```

**Alternative pattern using thread + `recv_timeout`** (more faithful to "producer pipelines block until JS pulls"):

```rust
#[test]
fn streaming_channel_producer_blocks_until_consumer_pulls() {
    use std::sync::mpsc::sync_channel;
    use std::thread;
    use std::time::Duration;

    let (tx, rx) = sync_channel::<u32>(2);  // tiny channel for clear blocking
    tx.send(0).unwrap();
    tx.send(1).unwrap();

    // Spawn a producer that will block on tx.send(2) until rx pulls.
    let tx_clone = tx.clone();
    let producer = thread::spawn(move || {
        let t0 = std::time::Instant::now();
        tx_clone.send(2).unwrap();  // BLOCKS until rx pulls
        t0.elapsed()
    });

    // Give producer time to actually call send and block.
    thread::sleep(Duration::from_millis(50));
    assert!(!producer.is_finished(), "producer must be blocked");

    // Consumer pulls. Producer unblocks.
    let _ = rx.recv().unwrap();
    let elapsed = producer.join().unwrap();
    assert!(elapsed >= Duration::from_millis(40),
            "producer should have blocked ≥ 40ms");
}
```

**Recommendation for PERF-01 (Claude's Discretion zone):** Use the **first pattern** — `try_send` + `Full`. It's:

- Deterministic (no thread sleep races)
- Fast (no 50ms sleep)
- Directly tests the property "channel rejects sends when full"
- CI-friendly (no timing flake)

The second pattern more directly maps to "blocks until JS pulls" but introduces sleep-race fragility. Use the first.

[CITED: std::sync::mpsc docs + Phase 31 streaming_tests pattern]

### 2.5 `process.memoryUsage()` polling pattern (PERF-03)

**Node.js `process.memoryUsage()` shape (verified Node 25.8.1):**

```js
{
  rss: 46432256,        // resident set size
  heapTotal: 6176768,
  heapUsed: 3727376,
  external: 1444534,
  arrayBuffers: 14571
}
```

**Pattern: setInterval poller capturing peak (rss + heapUsed):**

```typescript
function startPeakPoller(intervalMs = 50): {
  peak: () => number;
  stop: () => void;
} {
  let peak = 0;
  const sample = () => {
    const m = process.memoryUsage();
    const total = m.rss + m.heapUsed;
    if (total > peak) peak = total;
  };
  sample(); // capture baseline
  const handle = setInterval(sample, intervalMs);
  return {
    peak: () => peak,
    stop: () => clearInterval(handle),
  };
}

// Usage:
const poller = startPeakPoller(50);
try {
  await runWorkload(); // hydrate / advance
} finally {
  poller.stop();
}
const bufferedPeak = poller.peak();
```

**Caveats:**

- `setInterval(fn, 50)` runs on the same event loop as the workload. If the workload is a tight CPU loop, the poller may not fire. The streaming/buffered hydrate paths cross JS↔Rust boundaries via async libuv worker threads, so the JS event loop has plenty of opportunities to schedule the poller. For Rust-heavy workloads this is fine.
- For a more robust poll (especially if a workload iteration is mostly inside a single sync Rust call), spawn the poller in a `worker_threads.Worker` — more setup but immune to JS event-loop starvation. CONTEXT D-18 specifies `process.memoryUsage()` polled at ~50ms — recommend the simple `setInterval` approach first; only escalate to Worker if benchmarks show the poller starving.
- Force `gc()` before each measurement run for clean baseline. Requires `node --expose-gc`. If not available, just take 3 runs and use the smallest peak.

**Recommendation:** Use the `setInterval` pattern. Add a `--expose-gc` flag note to the benchmark's run command if planner wants pre-run GC.

[VERIFIED: node 25.8.1 memoryUsage shape; pattern derived from Node.js standard library]

### 2.6 Existing `rust-ivm-bench.ts` — plain script, NOT vitest

**Location:** `packages/zero-cache/src/services/view-syncer/rust-ivm-bench.ts` (363 lines)

**Pattern:** Plain Node script with `function main()`, run via:

```bash
node --experimental-strip-types --experimental-transform-types packages/zero-cache/src/services/view-syncer/rust-ivm-bench.ts
```

It does NOT use vitest. Has its own seed/sweep/correctness-check loop. Outputs to stdout.

**Implication for PERF-02/03:** CONTEXT D-14 says NEW file `rust-ivm-streaming-bench.ts` (sibling). CONTEXT D-17 says "part of vitest" — so the new bench file should be a vitest test, NOT a plain script following `rust-ivm-bench.ts`'s pattern.

The TWO different patterns coexisting is fine — `rust-ivm-bench.ts` is a manual diagnostic tool; `rust-ivm-streaming-bench.ts` is a CI gate.

[VERIFIED: rust-ivm-bench.ts:9 usage line + file read]

### 2.7 `.planning/milestones/v5.0-bench-results.md` — does NOT exist

Milestone dir contains only roadmap/requirements files for v1.0 through v4.0. No bench-results files exist. Plan 33-03 Task 1 must create the file with a header.

**Recommended format (Claude's Discretion zone — D-23 is loose on this):**

```markdown
# v5.0 Streaming — Benchmark Results

Append-only log. NEVER delete prior entries — they are the historical regression baseline.
Each line = one bench run. PR reviewers compare recent lines to spot regressions.

Format: `<ISO-timestamp> <bench-name> <metric>=<value> [<extras>] <verdict>`

Verdicts: PASS | WARN | FAIL

---

2026-04-29T18:00:00Z TTFB-streaming first_chunk_ms=12.3 min_pipeline_ms=10 ratio=1.23x threshold=1.5x PASS
2026-04-29T18:00:05Z MemPeak-streaming buffered_mb=128.4 streaming_mb=33.1 ratio=3.88x threshold=4x PASS
```

Single-line-per-run beats markdown table because lines append cleanly without messing up table formatting. Optional: also emit a JSON line for machine consumption.

[VERIFIED: milestones/ directory listing — no bench-results file]

---

## 3. Pitfalls & Gotchas

### P-01: Parity-check overhead in tight loops

**What goes wrong:** Even at 10% sample rate, running a full TS oracle path on every Nth advance materially slows tests. The TS oracle re-fetches from SQLite (or runs full TS IVM tree) — for hydrate paths over thousands of rows, this can add seconds to existing test suites.

**Why it happens:** `dualExecCompare` requires both arrays pre-materialized; you can't lazy-fold the comparison.

**How to avoid:**

- Use a `runTsExpensive: () => Promise<RowChange[]>` callback that's invoked ONLY when sampling fires (CONTEXT D-05 explicitly says this).
- The callback path can reuse the existing TS-hydrate codepath (`hydrateInternal` at `pipeline-driver.ts:2662`) for hydrate, and the existing buffered TS-IVM path for advance. NEVER eagerly compute it.

**Warning signs:** CI test suite runtime increases >30% after HARDEN-01 lands → sample rate too high or callback eagerly evaluating.

### P-02: Counter pollution across tests

**What goes wrong:** `parityDivergenceCount` is process-global. If test A causes a divergence and test B asserts `getParityDivergenceCount() === 0`, B fails because of A's pollution.

**How to avoid:**

- Export `resetParityDivergenceCount()` (CONTEXT D-06).
- Add `beforeEach(() => resetParityDivergenceCount())` in any test file that asserts on the counter.
- Consider also resetting `parityCheckInvocationCount` so sample-rate timing is deterministic.

**Warning signs:** Flaky test: passes alone, fails in suite.

### P-03: Env var read at module-init time

**What goes wrong:** `const MODE = parseMode(process.env['ZQLITE_RS_PARITY_CHECK'])` at module top level locks the value at first import. Tests that flip the flag in `beforeEach` see no effect.

**How to avoid:** Read inside the `PipelineDriver` constructor (per Phase 32 D-04..D-07 + RESEARCH P-03). Verified pattern at `view-syncer.ts:355-365`.

**Warning signs:** Test sets env var, expects parity-check fired N times, sees 0.

### P-04: vitest CI flakiness for timing benchmarks

**What goes wrong:** PERF-02 asserts `firstChunkT < 1.5 * minPipelineDelay`. On a noisy CI runner (shared CPU, GC pauses, scheduling jitter), the 1.5x ratio sporadically fails.

**How to avoid:**

- CONTEXT D-29 mandates loose threshold (1.5x for TTFB, 1.2x for memory).
- For really CI-fragile cases, wrap with `it.skipIf(process.env.CI && process.env.STREAMING_BENCH_STRICT !== 'true')` so the bench runs only on opted-in CI matrix entries.
- Run the bench 3 times, take the median, assert on that. Single-iteration assertions are inherently flaky for sub-100ms metrics.
- For PERF-02 specifically: 10ms pipeline delay is the minimum; CI scheduler jitter can be 10-20ms. Consider raising the minimum delay to 50ms (with 1.5x threshold = 75ms cushion).

**Warning signs:** Bench passes locally, fails sporadically in CI (especially under load).

### P-05: Memory measurement noise from V8 GC

**What goes wrong:** `process.memoryUsage().heapUsed` can fluctuate by ±5MB just from V8 GC cycles. Comparing buffered (one snapshot) vs streaming (one snapshot) with no GC between gives inconsistent results.

**How to avoid:**

- Run with `node --expose-gc` and call `global.gc()` between buffered and streaming runs.
- Take multiple samples and use the SMALLEST observed peak (most pessimistic).
- Use `rss + heapUsed` (CONTEXT D-18 specifies this) — `rss` reflects native Rust allocations, `heapUsed` reflects JS allocations. Together they capture both sides of the FFI boundary.
- Allow ample slack in the assertion (CONTEXT D-21 = 1.2x).

**Warning signs:** Memory ratio fluctuates >2x between runs.

### P-06: Sample-rate counter and flag in same test process

**What goes wrong:** Tests that need DIFFERENT sample rates in different cases (e.g., one test wants every-call comparison, another wants every-10th) can't both run in same process if the rate is set via env var ONCE at constructor time.

**How to avoid:**

- The constructor reads BOTH `ZQLITE_RS_PARITY_CHECK` (mode) and `ZQLITE_RS_PARITY_CHECK_RATE` (rate). Tests that need different rates must construct fresh `PipelineDriver` instances after flipping env vars.
- Or: expose a test-only setter `setParityCheckRateForTest(rate: number)` like Phase 31's `force_in_push_for_test` (gate behind `if (process.env.NODE_ENV !== 'production')`).

**Warning signs:** Test file with multiple cases needing different rates fails when run together.

### P-07: `dualExecCompare` returns TS array, not Rust

**What goes wrong:** `dualExecCompare(label, ts, rust, lc): RowChange[]` returns the TS array (per dual-executor.ts:298). If the HARDEN-01 shim does `return dualExecCompare(...)`, the production code now uses TS results — defeats the purpose (Rust is the production path).

**How to avoid:** Ignore the return value:

```typescript
if (this.#shouldRunParityCheck()) {
  dualExecCompare('advance', tsChanges, rustChanges, this.#lc);
  // return rustChanges, NOT the dualExecCompare return value
}
return rustChanges;
```

**Warning signs:** Subtle production behavior change after HARDEN-01 lands — TS path's quirks (e.g., ordering, edge cases) leak into production output.

### P-08: HARDEN-02 framework-invariant tests need separate file or `#[should_panic]`

**What goes wrong:** Tests that intentionally panic (e.g., `test_or_exists_reentrancy_panics` already exists) must use `#[should_panic]` annotation. If you add framework-invariant assertion tests without it, the test fails with "thread panicked" instead of passing.

**How to avoid:**

- Use `#[test] #[should_panic(expected = "Unexpected re-entrancy")]` (existing pattern at `or_exists_op.rs:480-481`).
- One panic-test per panic-condition.

**Warning signs:** Adding 3 new framework-invariant tests, all 3 fail with thread panicked messages instead of passing.

### P-09: PERF-01 channel sizing trap

**What goes wrong:** Production channel is sized `pipeline_count.max(1) + 1`. PERF-01 test must NOT assume capacity = `pipeline_count`. Filling `pipeline_count` items leaves 1 slot free; the test won't see the block.

**How to avoid:** Either (a) hardcode capacity in the test (e.g., 5 for 4 pipelines), or (b) call the production sizing function. Approach (a) is cleaner because PERF-01 is about validating the bound, not the choice of bound.

**Warning signs:** Test passes by accident because it never actually filled the channel.

### P-10: PERF-02 fixture must use Proxy-on-prototype, not test-only Rust hooks

**What goes wrong:** Phase 31's `PANIC_ON_PIPELINE_INDEX` is `cfg(test)` Rust-side and not exposed to TS. PERF-02 needs to inject DELAYS into specific pipelines from TS.

**How to avoid:** Use the Proxy-on-prototype pattern from `pipeline-driver.streaming.test.ts:265-299`:

```typescript
const orig = ManagerCls.prototype.advanceStreaming;
ManagerCls.prototype.advanceStreaming = function patched(id, json) {
  const realStream = orig.call(this, id, json);
  return new Proxy(realStream, {
    get(target, prop) {
      if (prop === 'next') {
        return async () => {
          const item = await target.next();
          // inject delay based on chunk index
          await sleep(delays[chunkIdx++ % delays.length]);
          return item;
        };
      }
      return Reflect.get(target, prop);
    },
  });
};
```

**Warning signs:** Trying to add per-pipeline delays in Rust requires plumbing through napi → can't be done quickly.

### P-11: PERF-03 buffered-vs-streaming comparison must use FRESH drivers

**What goes wrong:** Re-using the same `PipelineDriver` instance across buffered + streaming runs leaves residual state in caches/connection pools. Memory measurement gets polluted.

**How to avoid:** Create fresh `PipelineDriver` and fresh `DbFile` per measurement. Pattern from `rust-ivm-bench.ts:117-150` (`setupDb` per iteration). Drop both before measuring next.

**Warning signs:** Streaming peak > buffered peak (impossible if measurement is clean).

### P-12: chunk-end sentinel pollutes downstream filters

**What goes wrong:** PERF-02/03 collect/process changes from streaming wrapper. If they don't filter `'chunk-end'`, it shows up in change counts and ordering assertions.

**How to avoid:** Always filter `c !== 'yield' && c !== 'chunk-end'` (Phase 32 carry-forward).

**Warning signs:** Bench reports 2x more changes than expected.

---

## 4. OrExists Test Categorization

### Existing exists_op.rs tests (22, verified)

| #   | Category                           | Test name                                                   |
| --- | ---------------------------------- | ----------------------------------------------------------- |
| 1   | Fetch                              | `test_exists_fetch_filters_parents_without_children`        |
| 2   | Push child add                     | `test_exists_push_child_add_0_to_1_transition`              |
| 3   | Push child add (NOT EXISTS)        | `test_not_exists_push_child_add_0_to_1_transition`          |
| 4   | Push child remove                  | `test_exists_push_child_remove_1_to_0_transition`           |
| 5   | Push child add (NOT EXISTS, empty) | `test_not_exists_push_child_add_0_to_1_relationship_empty`  |
| 6   | Push parent add                    | `test_exists_push_parent_add_passes_when_children_exist`    |
| 7   | Push parent add                    | `test_exists_push_parent_add_blocked_when_no_children`      |
| 8   | Push edit                          | `test_exists_push_edit_passes_when_children_exist`          |
| 9   | Push (passthrough)                 | `test_exists_push_different_relationship_child_passthrough` |
| 10  | Push child edit                    | `test_exists_push_child_edit_passthrough`                   |
| 11  | or_predicate bypass                | `test_or_predicate_bypasses_exists_check`                   |
| 12  | Push child add (boundary)          | `test_exists_push_child_add_beyond_boundary`                |
| 13  | Push child remove (NOT EXISTS)     | `test_not_exists_push_child_remove_1_to_0_transition`       |
| 14  | Framework invariant                | `test_in_push_flag_cleared_after_push`                      |
| 15  | Cache invalidation                 | `test_cache_cleared_on_fetch`                               |
| 16  | Cache miss recovery                | `test_child_add_without_prior_fetch_recomputes_count`       |
| 17  | Re-entrancy assertion              | `test_exists_reentrancy_panics`                             |
| 18  | Edit + or_predicate (both pass)    | `test_exists_edit_or_predicate_both_pass`                   |
| 19  | Edit + or_predicate (old only)     | `test_exists_edit_or_predicate_old_only`                    |
| 20  | Edit + or_predicate (new only)     | `test_exists_edit_or_predicate_new_only`                    |
| 21  | Edit + or_predicate (neither)      | `test_exists_edit_or_predicate_neither`                     |
| 22  | Edit (no or_predicate)             | `test_exists_edit_no_or_predicate_unchanged`                |

### Existing or_exists_op.rs tests (5, verified)

| #   | Category                        | Test name                          |
| --- | ------------------------------- | ---------------------------------- |
| 1   | Re-entrancy assertion           | `test_or_exists_reentrancy_panics` |
| 2   | Edit + or_predicate (both pass) | `test_or_exists_edit_both_pass`    |
| 3   | Edit + or_predicate (old only)  | `test_or_exists_edit_old_only`     |
| 4   | Edit + or_predicate (new only)  | `test_or_exists_edit_new_only`     |
| 5   | Edit + or_predicate (neither)   | `test_or_exists_edit_neither`      |

### Recommended target test list for or_exists_op.rs (~30 tests, planner picks final names)

Mirror the exists_op.rs categories and add 2 OrExists-specific (multi-branch OR) categories per CONTEXT D-09:

| #   | Category                                        | Suggested test name                                            | New / Existing         |
| --- | ----------------------------------------------- | -------------------------------------------------------------- | ---------------------- |
| 1   | Fetch                                           | `test_or_exists_fetch_filters_parents_without_children`        | NEW                    |
| 2   | Fetch + or_predicate                            | `test_or_exists_fetch_or_predicate_short_circuits`             | NEW                    |
| 3   | Fetch with constraint                           | `test_or_exists_fetch_with_parent_constraint`                  | NEW                    |
| 4   | Push parent add (no children, no predicate)     | `test_or_exists_push_parent_add_blocked_when_no_branches_pass` | NEW                    |
| 5   | Push parent add (children matching)             | `test_or_exists_push_parent_add_passes_when_branch_passes`     | NEW                    |
| 6   | Push parent add (or_predicate matches)          | `test_or_exists_push_parent_add_passes_via_or_predicate`       | NEW                    |
| 7   | Push parent remove                              | `test_or_exists_push_parent_remove_emits_when_was_passing`     | NEW                    |
| 8   | Push edit (both pass)                           | `test_or_exists_edit_both_pass`                                | EXISTING               |
| 9   | Push edit (old only)                            | `test_or_exists_edit_old_only`                                 | EXISTING               |
| 10  | Push edit (new only)                            | `test_or_exists_edit_new_only`                                 | EXISTING               |
| 11  | Push edit (neither)                             | `test_or_exists_edit_neither`                                  | EXISTING               |
| 12  | Push edit (no or_predicate, count change)       | `test_or_exists_edit_count_change_no_predicate`                | NEW                    |
| 13  | Hydrate (single branch, basic data)             | `test_or_exists_hydrate_single_branch_basic`                   | NEW                    |
| 14  | Hydrate (multi-branch, mixed pass)              | `test_or_exists_hydrate_multi_branch_partial_pass`             | NEW                    |
| 15  | Hydrate (with or_predicate)                     | `test_or_exists_hydrate_with_or_predicate_data`                | NEW                    |
| 16  | Child push (add to 0-children parent)           | `test_or_exists_push_child_add_0_to_1_transition`              | NEW                    |
| 17  | Child push (NOT EXISTS branch, 0→1)             | `test_or_exists_not_exists_branch_child_add_0_to_1`            | NEW                    |
| 18  | Child push (remove 1→0)                         | `test_or_exists_push_child_remove_1_to_0_transition`           | NEW                    |
| 19  | Child push (different relationship passthrough) | `test_or_exists_push_different_relationship_child_passthrough` | NEW                    |
| 20  | Child push (edit)                               | `test_or_exists_push_child_edit_passthrough`                   | NEW                    |
| 21  | Child add when other branch passes              | `test_or_exists_push_child_add_other_branch_already_passing`   | NEW                    |
| 22  | Re-entrancy assertion                           | `test_or_exists_reentrancy_panics`                             | EXISTING               |
| 23  | Cache invalidation on fetch                     | `test_or_exists_cache_cleared_on_fetch`                        | NEW                    |
| 24  | Cache miss recovery                             | `test_or_exists_child_add_without_prior_fetch_recomputes`      | NEW                    |
| 25  | OR over 2 EXISTS branches (both pass)           | `test_or_exists_two_branches_both_pass`                        | NEW (D-09 requirement) |
| 26  | OR over 2 EXISTS branches (one passes)          | `test_or_exists_two_branches_one_passes`                       | NEW (D-09 requirement) |
| 27  | OR over EXISTS + NOT EXISTS branches            | `test_or_exists_mixed_exists_not_exists_branches`              | NEW                    |
| 28  | OR over 3+ branches                             | `test_or_exists_three_branches_partial`                        | NEW (optional)         |
| 29  | Builder-spec parity                             | `test_or_exists_ast_to_config_matches_direct_construction`     | NEW (D-09 requirement) |
| 30  | in_push flag cleared after push                 | `test_or_exists_in_push_flag_cleared_after_push`               | NEW                    |

**Total: 30 (5 existing + 25 new).**

**Per CONTEXT D-11:** If reaching 30 requires contrived tests, stop at 25-28 and document. Tests 28 (3+ branches) and 27 (mixed types) may be the pruning candidates if multi-branch test infra proves expensive to set up — drop those first.

---

## 5. Test Patterns

### 5.1 Channel-block test (PERF-01) — recommended scaffolding

```rust
#[test]
fn streaming_channel_bounded_blocks_when_full() {
    use std::sync::mpsc::{sync_channel, TrySendError};

    // Production sizing: pipeline_count = 4 → channel capacity = 5
    let pipeline_count = 4usize;
    let capacity = pipeline_count.max(1) + 1;  // 5

    let (tx, rx) = sync_channel::<u32>(capacity);

    // Phase 1: fill to capacity
    for i in 0..capacity as u32 {
        tx.try_send(i).expect("send within capacity must succeed");
    }

    // Phase 2: next send must fail with Full
    match tx.try_send(99) {
        Err(TrySendError::Full(99)) => {} // expected
        other => panic!("expected Full(99), got {:?}", other),
    }

    // Phase 3: drain one item
    let drained = rx.recv().expect("recv after fill must succeed");
    assert_eq!(drained, 0);

    // Phase 4: a previously-blocked sender now succeeds
    tx.try_send(99).expect("send after drain must succeed");
    assert_eq!(rx.recv().unwrap(), 1);
    assert_eq!(rx.recv().unwrap(), 2);
    assert_eq!(rx.recv().unwrap(), 3);
    assert_eq!(rx.recv().unwrap(), 4);
    assert_eq!(rx.recv().unwrap(), 99);
}
```

Place in `pipeline_manager.rs::streaming_tests` mod (line 1351 area). No new helpers needed.

### 5.2 TTFB benchmark (PERF-02) — recommended scaffolding

```typescript
// rust-ivm-streaming-bench.ts
import {test, describe, expect, beforeAll} from 'vitest';
import {appendFileSync} from 'node:fs';
// ... other imports

const BENCH_RESULTS_FILE = '.planning/milestones/v5.0-bench-results.md';

function logBenchResult(line: string): void {
  const timestamp = new Date().toISOString();
  appendFileSync(BENCH_RESULTS_FILE, `${timestamp} ${line}\n`);
}

describe('PERF-02 TTFB streaming bench', () => {
  test('first-chunk arrives ~min(pipeline_time), not max', async () => {
    // Setup: 4 pipelines, fixture with delays via Proxy-on-prototype.
    const fx = setupFixtureWithStaggeredDelays([10, 50, 100, 500]);

    const result = await fx.pipelines.advanceStreaming(NO_TIME_TIMER);
    let firstChunkT: number | undefined;
    const start = performance.now();
    for await (const c of result.changes) {
      if (c !== 'yield' && c !== 'chunk-end') {
        if (firstChunkT === undefined) {
          firstChunkT = performance.now() - start;
        }
        // Don't break — let stream complete naturally so cleanup happens
      }
    }

    const minDelay = 10;
    const ratio = firstChunkT! / minDelay;
    const threshold = 1.5;
    const verdict = ratio <= threshold ? 'PASS' : 'FAIL';

    logBenchResult(
      `TTFB-streaming first_chunk_ms=${firstChunkT!.toFixed(2)} ` +
        `min_pipeline_ms=${minDelay} ratio=${ratio.toFixed(2)}x ` +
        `threshold=${threshold}x ${verdict}`,
    );

    expect(firstChunkT).toBeLessThan(minDelay * threshold);
  });
});
```

**Proxy-on-prototype helper** (mirror `pipeline-driver.streaming.test.ts:281-310`):

```typescript
function patchAdvanceStreamingWithDelays(delaysMs: number[]) {
  const ManagerCls = (zqliteRs as any).RustPipelineManager;
  const orig = ManagerCls.prototype.advanceStreaming;
  let chunkIdx = 0;
  ManagerCls.prototype.advanceStreaming = function patched(id, json) {
    const realStream = orig.call(this, id, json);
    return new Proxy(realStream, {
      get(target, prop) {
        if (prop === 'next') {
          return async () => {
            const idx = chunkIdx++;
            const delay = delaysMs[idx % delaysMs.length];
            const item = await target.next();
            if (!item.done) {
              await new Promise(r => setTimeout(r, delay));
            }
            return item;
          };
        }
        return Reflect.get(target, prop);
      },
    });
  };
  return () => {
    ManagerCls.prototype.advanceStreaming = orig;
  };
}
```

### 5.3 Memory peak benchmark (PERF-03) — recommended scaffolding

```typescript
function startPeakPoller(intervalMs = 50): {
  peak: () => number;
  stop: () => void;
} {
  let peak = 0;
  const sample = () => {
    const m = process.memoryUsage();
    const total = m.rss + m.heapUsed;
    if (total > peak) peak = total;
  };
  sample(); // baseline
  const handle = setInterval(sample, intervalMs);
  return {
    peak: () => peak,
    stop: () => clearInterval(handle),
  };
}

describe('PERF-03 memory peak streaming bench', () => {
  test('streaming peak ~ O(max_pipeline_changes), not O(total)', async () => {
    const N_PIPELINES = 4;
    const ROWS_PER_PIPELINE = 2500;

    // Run 1: buffered
    const fxBuf = setupFixtureWithBigData(N_PIPELINES, ROWS_PER_PIPELINE);
    if (global.gc) global.gc();
    const pollerBuf = startPeakPoller(50);
    const bufRes = await fxBuf.pipelines.advanceAsync(NO_TIME_TIMER);
    for (const c of bufRes.changes) {
      /* drain */
    }
    pollerBuf.stop();
    const bufferedPeak = pollerBuf.peak();
    fxBuf.destroy();

    // Run 2: streaming
    const fxStr = setupFixtureWithBigData(N_PIPELINES, ROWS_PER_PIPELINE);
    if (global.gc) global.gc();
    const pollerStr = startPeakPoller(50);
    const strRes = await fxStr.pipelines.advanceStreaming(NO_TIME_TIMER);
    for await (const c of strRes.changes) {
      /* drain */
    }
    pollerStr.stop();
    const streamingPeak = pollerStr.peak();
    fxStr.destroy();

    const ratio = bufferedPeak / streamingPeak;
    const expectedRatio = N_PIPELINES; // streaming should be ~Nx smaller
    const threshold = 1.2; // allow 20% slack

    // CONTEXT D-21: streamingPeak * N <= bufferedPeak * 1.2
    const passing = streamingPeak * N_PIPELINES <= bufferedPeak * threshold;
    const seriousFail = streamingPeak * N_PIPELINES > bufferedPeak * 2.0;

    const verdict = passing ? 'PASS' : seriousFail ? 'FAIL' : 'WARN';

    logBenchResult(
      `MemPeak-streaming buffered_mb=${(bufferedPeak / 1e6).toFixed(2)} ` +
        `streaming_mb=${(streamingPeak / 1e6).toFixed(2)} ` +
        `ratio=${ratio.toFixed(2)}x expected=${expectedRatio}x ${verdict}`,
    );

    if (seriousFail) {
      throw new Error(`Memory peak ratio ${ratio.toFixed(2)}x is severely off`);
    }
    // CONTEXT D-21: small failures = WARN, not FAIL
  });
});
```

### 5.4 HARDEN-01 sampling shim wiring pattern

```typescript
// pipeline-driver.ts (top-level module state)
let parityDivergenceCount = 0;
let parityCheckInvocationCount = 0;

export function getParityDivergenceCount(): number {
  return parityDivergenceCount;
}

export function resetParityDivergenceCount(): void {
  parityDivergenceCount = 0;
  parityCheckInvocationCount = 0;
}

type ParityCheckMode = 'off' | 'sample' | 'strict';

function parseParityCheckMode(env: string | undefined): ParityCheckMode {
  if (env === 'sample' || env === 'strict') return env;
  return 'off';
}

// Inside class PipelineDriver:
//   readonly #parityCheckMode: ParityCheckMode;
//   readonly #parityCheckRate: number;

constructor(...) {
  // ...existing...
  // Read once per instance — see Phase 32 D-04..D-07 / RESEARCH P-03.
  this.#parityCheckMode = parseParityCheckMode(
    process.env['ZQLITE_RS_PARITY_CHECK']
  );
  this.#parityCheckRate = Math.max(
    1,
    parseInt(process.env['ZQLITE_RS_PARITY_CHECK_RATE'] ?? '10', 10)
  );
}

#shouldRunParityCheck(): boolean {
  if (this.#parityCheckMode === 'off') return false;
  parityCheckInvocationCount++;
  if (this.#parityCheckMode === 'strict') return true;
  // sample mode
  return parityCheckInvocationCount % this.#parityCheckRate === 0;
}

async #maybeRunParityCheck(
  label: string,
  rustChanges: RowChange[],
  runTsExpensive: () => Promise<RowChange[]>,
): Promise<void> {
  if (!this.#shouldRunParityCheck()) return;

  const tsChanges = await runTsExpensive();
  // dualExecCompare logs/throws based on isDualExecStrict() — but we want
  // OUR strict mode (ZQLITE_RS_PARITY_CHECK=strict) to govern, not
  // ZERO_DUAL_EXEC. Run the comparison directly via compareChanges().
  const result = compareChanges(tsChanges, rustChanges);
  if (!result.match) {
    parityDivergenceCount++;
    const detail = `[parity] ${label}: ${result.mismatches.length} divergence(s)`;
    if (this.#parityCheckMode === 'strict') {
      this.#lc.error?.(detail);
      throw new Error(`Parity check failed: ${detail}`);
    } else {
      this.#lc.warn?.(detail);
    }
  }
}
```

**Wiring at `#rustAdvanceAsync` (line 2019):**

```typescript
async #rustAdvanceAsync(timer: Timer): Promise<{...}> {
  // ...existing diff/swap/setPermissionTables...
  const resultBuf = await this.#manager.advanceAsync(this.#instanceId, changesJson);
  const decoded = decodeAdvanceResultBuf(resultBuf);
  if (decoded.error) throw new Error(...);
  if (decoded.reset_signal) throw new ResetPipelinesSignal(...);

  const rustChanges = materializeChanges(this.#convertDispatchChanges(decoded.changes));

  // HARDEN-01: sample-mode parity check.
  // Cost is paid only when shouldRunParityCheck() returns true.
  await this.#maybeRunParityCheck(
    'advance',
    rustChanges,
    async () => {
      // Run the same input through TS IVM. Reuse the existing TS-IVM path
      // that exists from before the v4.0 Rust port (see #rustAdvance for
      // the pre-Rust pattern).
      return materializeChanges(this.#tsAdvanceForOracle(collectedChanges));
    },
  );

  for (const table of this.#tables.values()) table.setDB(curr.db.db);
  // ...rest unchanged...
  return {version: curr.version, numChanges, changes: this.#wrapWithTimeout(rustChanges, timer, numChanges)};
}
```

**Note on `#tsAdvanceForOracle`:** The TS oracle path needs to push the same `collectedChanges` through the TS IVM tree. Look for the pre-Rust TS path that's still in the codebase (likely involves `Streamer` at `pipeline-driver.ts:2499` and the per-pipeline `input.push(...)` pattern). The exact wiring is the planner's call — possibly extract a helper `#tsAdvanceImpl(changes): Iterable<RowChange|'yield'>` that mirrors what `#rustAdvanceAsync` does but using TS IVM.

---

## 6. Validation Architecture (Nyquist Dimension 8)

### Test Framework

| Property                  | Value                                                                        |
| ------------------------- | ---------------------------------------------------------------------------- |
| Framework (TS)            | vitest 4.1.3                                                                 |
| Framework (Rust)          | cargo test (built-in)                                                        |
| Config files              | `vitest.config.no-pg.ts`, `vitest.config.pg-16.ts`, `vitest.config.bench.ts` |
| Quick run command (TS)    | `npx vitest run src/services/view-syncer/pipeline-driver`                    |
| Quick run command (Rust)  | `cargo test --release -p zero-ivm-rs --lib or_exists_op::`                   |
| Full suite command (TS)   | `npx vitest run`                                                             |
| Full suite command (Rust) | `cargo test --release` (both crates)                                         |

### Phase Requirements → Test Map

| Req ID    | Behavior                                                         | Test Type   | Automated Command                                                              | File Exists? |
| --------- | ---------------------------------------------------------------- | ----------- | ------------------------------------------------------------------------------ | ------------ |
| HARDEN-01 | Sampling shim runs on every Nth advance/hydrate                  | unit        | `npx vitest run src/services/view-syncer/pipeline-driver-parity.test.ts` (NEW) | NEW (Wave 0) |
| HARDEN-01 | strict mode throws on injected divergence                        | unit        | (same file)                                                                    | NEW          |
| HARDEN-01 | counter visible to tests; reset works                            | unit        | (same file)                                                                    | NEW          |
| HARDEN-01 | sample rate respected (every Nth)                                | unit        | (same file)                                                                    | NEW          |
| HARDEN-01 | full existing suite passes under `ZQLITE_RS_PARITY_CHECK=sample` | regression  | `ZQLITE_RS_PARITY_CHECK=sample npx vitest run src/services/view-syncer/`       | EXISTS       |
| HARDEN-02 | OrExists test count ≥30 (or 25-28 with documentation)            | unit (Rust) | `cargo test --release -p zero-ivm-rs --lib or_exists_op::`                     | EXISTS       |
| PERF-01   | Channel rejects send when full; unblocks after drain             | unit (Rust) | `cargo test --release -p zqlite-rs --lib streaming_channel_bounded`            | EXISTS (mod) |
| PERF-02   | TTFB ≤ 1.5 × min(pipeline_time)                                  | bench       | `npx vitest run src/services/view-syncer/rust-ivm-streaming-bench.ts`          | NEW (Wave 0) |
| PERF-03   | streaming peak × N ≤ buffered peak × 1.2                         | bench       | (same file)                                                                    | NEW          |

### Sampling Rate Analysis (per signal)

| Signal                           | Source                                                   | Capture Rate                                        | Why                                             |
| -------------------------------- | -------------------------------------------------------- | --------------------------------------------------- | ----------------------------------------------- |
| Parity divergence rate           | `parityDivergenceCount` after test run                   | Every test run with `ZQLITE_RS_PARITY_CHECK=sample` | Deterministic counter; CI asserts == 0          |
| OrExists test coverage delta     | `cargo test --list \| grep or_exists_op:: \| wc -l`      | Per phase commit                                    | Regression-detect — count must stay ≥30         |
| Channel-block timing             | recv_timeout in PERF-01 test                             | Per cargo test run                                  | Deterministic try_send pattern, no timing noise |
| TTFB regression detection        | append-only `.planning/milestones/v5.0-bench-results.md` | Per CI run with `vitest bench`                      | Multi-run history, reviewers compare lines      |
| Memory-peak regression detection | (same)                                                   | (same)                                              | (same)                                          |

### Per task commit

- 33-01 task commits: `npx vitest run src/services/view-syncer/pipeline-driver` (TS smoke, ~30s)
- 33-02 task commits: `cargo test --release -p zero-ivm-rs --lib or_exists_op::` (~5s)
- 33-03 task commits: `cargo test --release -p zqlite-rs --lib streaming_channel` (~5s) plus selective bench file

### Per wave merge / phase gate

- Full vitest suite under `ZQLITE_RS_PARITY_CHECK=sample` (must pass with 0 divergences)
- Full vitest suite under `ZQLITE_RS_PARITY_CHECK=strict` on a sample of pipeline-driver tests (must pass with no thrown errors)
- `cargo test --release` both crates (170 + 128/1-ignored baseline + new tests must all pass)
- `FUZZ_NUM_RUNS=1000 npx vitest run streaming-vs-buffered-parity.fuzz.test.ts` (must remain green)
- Bench results appended to `.planning/milestones/v5.0-bench-results.md` (file must exist)

### Wave 0 Gaps

- [ ] `packages/zero-cache/src/services/view-syncer/pipeline-driver-parity.test.ts` — covers HARDEN-01 unit-level behavior (mode parsing, counter, sample rate, strict throw)
- [ ] `packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.ts` — covers PERF-02 + PERF-03
- [ ] `.planning/milestones/v5.0-bench-results.md` — header + format note (created by 33-03 Task 1)
- [ ] No framework install needed (vitest 4.1.3 + cargo test already in place)

---

## Project Constraints (from CLAUDE.md / AGENTS.md)

- **TS conventions:** ESM, `kebab-case.ts`, oxlint/oxfmt, `import type`, no `import()` in type expressions, no `mod.ts` imports, optional fields typed `type | undefined`
- **Test conventions:** vitest only; `.test.ts` for Node tests, `.pg.test.ts` for PostgreSQL tests, `.bench.ts` for vitest benches
- **Rust conventions:** snake*case fn names, `#[cfg(test)] pub(crate) fn force*\*\_for_test`pattern for test-only helpers,`--test-threads=1` if process-wide static state is involved
- **Hard constraint:** no signature change to existing buffered methods (`advance`, `advanceAsync`, `hydrate*`, `addQuery*`); no change to `encode_advance_result_buf` or `decodeAdvanceResultBuf` formats (carries forward into Phase 33)
- **GSD workflow:** all changes through `/gsd-execute-phase` (no direct edits outside GSD)

---

## Assumptions Log

| #   | Claim                                                                                  | Section | Risk if Wrong                                                                                                                                                                   |
| --- | -------------------------------------------------------------------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1  | Vitest 4.1.3 `bench` API supports `iterations` + `time` options                        | §2.3    | Bench config syntax differs slightly; planner adjusts                                                                                                                           |
| A2  | `process.memoryUsage()` `setInterval` poller does not starve under streaming workloads | §2.5    | Memory peak under-reported; escalate to Worker thread poller                                                                                                                    |
| A3  | TS oracle path (re-running TS IVM in HARDEN-01 callback) exists in current code        | §5.4    | Planner must extract / build the TS oracle; possibly bigger task than CONTEXT estimated                                                                                         |
| A4  | `dualExecCompare` is the right primitive for HARDEN-01 (vs `compareChanges` directly)  | §5.4    | If we want strict mode keyed off `ZQLITE_RS_PARITY_CHECK` (not `ZERO_DUAL_EXEC`), call `compareChanges` directly and handle log/throw locally — recommended in §5.4 sample code |

[ASSUMED] A1 — vitest 4 `bench` options API
[ASSUMED] A2 — JS event loop scheduling under napi worker thread workloads
[VERIFIED] A3 — searched grep but did not deeply inspect — risk that "TS oracle path" requires fresh implementation work in Phase 33
[VERIFIED] A4 — verified that `dualExecCompare` keys strict on `ZERO_DUAL_EXEC=strict`, NOT on a passed-in mode → §5.4 recommends `compareChanges` directly

---

## Open Questions

1. **TS oracle wiring complexity (A3 above)** — what does the TS-IVM advance path look like AFTER the v4.0 Rust port?
   - What we know: `pipeline-driver.ts` has `materializeChanges` and `Streamer` class; `hydrateInternal` is the TS hydrate primitive; `#rustAdvance` is the production path.
   - What's unclear: Is there still a fully-wired TS-only advance path (pre-Rust), or did the v4.0 port delete it? If deleted, `#tsAdvanceForOracle` must be reconstructed.
   - Recommendation: Planner Task 1 of 33-01 should grep for any remaining TS-IVM advance helper. If none, an early task scope is "extract TS oracle" — possibly larger than the CONTEXT D-01 ~5-task estimate suggests.

2. **PERF-02 timing precision under CI** — 10ms minimum delay may be inside CI scheduler jitter range (10-20ms).
   - Recommendation: Planner consider raising minimum delay to 50ms in fixture, with same 1.5x ratio threshold (75ms cushion). Document in 33-03 SUMMARY if used.

3. **`ZQLITE_RS_PARITY_CHECK` vs `ZERO_DUAL_EXEC` overlap** — CONTEXT D-03 locks the new name, but `pipeline-driver.test.ts:50` already sets `ZERO_DUAL_EXEC=strict`. Should HARDEN-01 ALSO honor `ZERO_DUAL_EXEC` (back-compat) or strictly use the new var (cleaner)?
   - What we know: `dualExecCompare` reads `ZERO_DUAL_EXEC`; nothing in production currently calls it.
   - What's unclear: If we use `compareChanges` directly (per §5.4 recommendation), `ZERO_DUAL_EXEC` becomes truly dead. Planner choice: leave dead, or remove.
   - Recommendation: Planner uses `compareChanges` directly + new `ZQLITE_RS_PARITY_CHECK` var; does NOT touch `dual-executor.ts` or `ZERO_DUAL_EXEC`. They remain library-available for the parity fuzz tests.

---

## Sources

### Primary (HIGH confidence)

- `packages/zero-cache/src/services/view-syncer/dual-executor.ts:283` — dualExecCompare signature [VERIFIED file read]
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:1795, 1867, 2019, 996, 1117, 49, 2071, 2466, 2662` — wiring sites [VERIFIED]
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts:265-310` — Proxy-on-prototype pattern [VERIFIED]
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.bench.ts:19, 327` — vitest `bench` API in use [VERIFIED]
- `packages/zero-cache/src/services/view-syncer/view-syncer.ts:355-365` — env-var-in-constructor pattern [VERIFIED]
- `packages/zqlite-rs/src/pipeline_manager.rs:428, :1351-1640` — channel sizing + streaming_tests scaffolding [VERIFIED]
- `packages/zero-ivm-rs/src/exists_op.rs` — 22 tests via `cargo test --list` [VERIFIED]
- `packages/zero-ivm-rs/src/or_exists_op.rs` — 5 tests via `cargo test --list` [VERIFIED]
- `.planning/phases/31-streaming-primitives-and-wrappers/31-01-SUMMARY.md` — channel sizing rationale [CITED]
- `.planning/phases/32-view-syncer-migration/32-02-SUMMARY.md` — env-var-in-constructor pattern (P-03) [CITED]
- `.planning/IVM-STREAMING-PLAN.md §10 Phase D` — perf tuning context [CITED]

### Secondary (MEDIUM confidence)

- Node.js `process.memoryUsage()` shape (Node 25.8.1 verified locally) [VERIFIED]
- `std::sync::mpsc::sync_channel` `try_send` / `TrySendError::Full` semantics — Rust stdlib [CITED stdlib docs]
- vitest `bench` API existence (4.x has it) [VERIFIED via existing usage in pipeline-driver.bench.ts]

### Tertiary (LOW confidence)

- vitest `bench` `iterations` + `time` options exact API [ASSUMED — A1]
- JS event-loop scheduling fairness under napi worker thread workloads [ASSUMED — A2]

---

## Metadata

**Confidence breakdown:**

- HARDEN-01 specifics: HIGH — dualExecCompare signature verified, env-var pattern carry-forward from Phase 32, materializeChanges already imported
- HARDEN-02 categories: HIGH — exists_op.rs and or_exists_op.rs test lists verified via `cargo test --list`; CONTEXT count corrections documented
- PERF-01 channel test: HIGH — `mpsc::sync_channel` + `try_send` is canonical stdlib pattern
- PERF-02 TTFB bench: MEDIUM — Proxy-on-prototype pattern verified; vitest `bench` options API assumed
- PERF-03 memory bench: MEDIUM — `process.memoryUsage()` shape verified; setInterval polling reliability under napi workloads is judgment call
- Validation Architecture: HIGH — vitest infra and cargo test already in place

**Research date:** 2026-04-29
**Valid until:** 2026-05-29 (30 days; library APIs and code conventions stable)

---

## RESEARCH COMPLETE
