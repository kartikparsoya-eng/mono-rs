# Phase 35: Pool & Cascade Hardening - Context

**Gathered:** 2026-04-30
**Status:** Ready for planning
**Milestone:** v6.0 (planning)

<domain>
## Phase Boundary

Two design-level workstreams targeting the operability sharp edges left over after Phase 34's cascade-delete fix and the broader connection-pool poison surface:

**Workstream B10 — `swap_path` retry + unified poison handling:** The connection pool's error model is half-finished. `swap_path` propagates `PoolError::Poisoned` explicitly, but `path()` silently recovers via `unwrap_or_else(|e| e.into_inner())`, and 4 callers in `pipeline_manager.rs` plus 3 in `table_source.rs` still `.unwrap()` inner mutexes. `swap_path` itself fails fast on `Exhausted` with no retry — a single in-flight `PooledConnection` makes a snapshot rotation impossible until the holder drops it. This workstream unifies the pool's error model and adds bounded-retry semantics to `swap_path`.

**Workstream NEW-1 — prev-snapshot connection pooling:** Phase 34's B11 fix made `emit_descendant_removals` open a fresh `rusqlite::Connection` per (recursive) call against `prev_db_path`. For a 3-level cascade with K root removes, that's O(K × D) syscalls. There's no pool, no statement cache — every recursion hits OS file-open + sqlite header reads. This workstream adds a small prev-snapshot connection cache (or threads a single connection through) and coordinates lifecycle with NEW-3 (`prev_db_path` clearing on `swap_snapshot`).

Out of scope for this phase: B5 (OrExists short-circuit), B6 (EXISTS_LIMIT downgrade), B7 (FlippedJoin family), B8 (i64 > 2^53 lossy compare), B12 (companion drift), B13 (state-key serialization NIT), B14 (`unsafe` epoch mutation — deserves its own AtomicU64 plan), NEW-2 (silent-swallow of sqlite errors in `emit_descendant_removals`), NEW-4 (B1 inner-And ordering caveat), NEW-5 (BEGIN DEFERRED on prev connection). NEW-2/NEW-3/NEW-5 may be touched opportunistically in Wave 2 if the connection-pooling refactor naturally surfaces them — but each is an explicit non-goal here.

</domain>

<decisions>
## Implementation Decisions

### Workstream B10 — Unified PoolError

- **D-01:** Keep the existing `PoolError` enum as the canonical pool error type but ADD variants needed by callers. Final variant set: `Sqlite(rusqlite::Error)` (existing), `Exhausted` (existing), `Poisoned` (existing), `IoError(std::io::Error)` (NEW — for path-open failures distinct from sqlite-internal errors), `ConcurrentSwapInProgress` (NEW — for the case where a second `swap_path` is invoked while a first is mid-flight in retry). `Display` and `std::error::Error` impls extended to match.
- **D-02:** **Retry strategy on `Exhausted` in `swap_path`:** bounded retry with linear backoff. Defaults — 5 attempts, 50ms initial sleep, 50ms increment per attempt (so cumulative budget ≤ 750ms). Configurable via a builder method `swap_path_with_timeout(new_path, max_wait: Duration)`. Rationale: matches the existing `busy_timeout(5000ms)` for sqlite-level retry but keeps pool-level retry tighter because callers expect `swap_path` to be near-instant on the WAL same-path fast path. Exceeding budget returns `PoolError::Exhausted` (terminal, no auto-fallback).
- **D-03:** **Poison handling — explicit propagation everywhere.** Replace every `.unwrap()` on inner mutexes with `.map_err(|_| PoolError::Poisoned)?` (or appropriate caller-side conversion). Replace `path()`'s silent `.unwrap_or_else(|e| e.into_inner())` with explicit `Result<String, PoolError>` return — callers convert. Caller contract: the pool's poison error bubbles up to napi boundary as `napi::Error::from_reason(format!("pool poisoned: {e}"))`; in-test code MAY use `.expect("pool not poisoned")` because the test surface controls the lock holders. **No silent recovery anywhere outside test code.** Outside the pool (pipeline_manager.rs, table_source.rs), inner-mutex poison on instance/connection state propagates as napi error too — same model, no `.unwrap()` in production paths.
- **D-04:** **Retry interaction with "all connections checked in" requirement.** `swap_path` requires all `pool_size` connections back in the queue. With retry, the second-level question is: should retry wait for connections to return (i.e., poll the queue), or fail immediately? Decision: retry-poll model — `swap_path` re-acquires the lock each attempt and re-checks `conns.len() == pool_size`. This is "wait for all to return" with a bounded timeout. The fast-fail mode is exposed as `swap_path_now` (no retry, returns Exhausted immediately) for callers that already know connections are quiesced. `pipeline_manager.rs::swap_snapshot` migrates to the polling variant by default; `swap_path_now` reserved for explicit "I just drained everything" call sites.
- **D-05:** **Concurrent-swap protection.** Two `swap_path` calls on the same pool from different threads currently race on the path mutex (one wins, the other observes the new path mid-set). Add an internal `AtomicBool` `swap_in_progress` or a dedicated `Mutex<()>` "swap lock"; second concurrent caller returns `PoolError::ConcurrentSwapInProgress`. Mostly defensive; not a known bug, but the new retry loop makes the window wider.
- **D-06:** **Test plan for poison + exhausted fault injection.** Add a `#[cfg(test)]` constructor `ConnectionPool::new_with_holds(path, pool_size, hold_count)` that pre-acquires `hold_count` connections and returns them held inside a `Vec<PooledConnection>` so the test can drop them on a timer. Add a `#[cfg(test)]` helper `ConnectionPool::poison_lock_for_test()` that spawns a thread which acquires the inner mutex, panics, and joins — leaving the mutex poisoned. Tests live in `connection_pool.rs::tests`.

### Workstream NEW-1 — Prev-Snapshot Pooling

- **D-07:** **Strategy: Option A — extend `ConnectionPool` to support a secondary "prev pool" slot.** Rejected B (thread connection through recursion — invasive signature change across `emit_descendant_removals` and 4 call sites) and C (TLS-cached open by path string — global state, hard to clear, racy with NEW-3). Option A keeps the pool model uniform and lets prev-pool reuse the existing `BEGIN DEFERRED` pinning, fixing NEW-5 (B11's prev connection currently has no snapshot pin) as a free side effect.
- **D-08:** Implementation shape: `Instance` (in `pipeline_manager.rs`) gains `prev_pool: Option<ConnectionPool>` alongside `prev_db_path: Option<String>`. `set_prev_snapshot` opens the prev_pool with size 1 (single connection sufficient — `emit_descendant_removals` is sequential on the advance thread). `swap_snapshot` clears BOTH `prev_db_path` and `prev_pool` per NEW-3 — so the pool's lifetime never outlives its path-validity.
- **D-09:** **Lifecycle:** prev_pool created lazily by `set_prev_snapshot` (open + `BEGIN DEFERRED`). Reused across the next `advance` call. Destroyed at the start of the NEXT `set_prev_snapshot` (replaces) or at `swap_snapshot` (clear). No long-lived caching across advances — each advance pair (set_prev_snapshot → swap_snapshot → advance → result) gets exactly one prev_pool.
- **D-10:** **Prepared-statement cache reuse.** `emit_descendant_removals` builds dynamic SQL per child relation (`SELECT * FROM "{table}" WHERE ...`) — currently no statement caching across the recursion. Add a `HashMap<String, CachedStatement>` keyed by `(table, conditions_signature)` inside the function, scoped to the single advance batch. The cache is dropped at function end. This is a minor extension on top of the connection reuse and amplifies the benefit for K-fanout cascades (same SQL shape repeats across siblings).
- **D-11:** **Coordination with NEW-3 (`prev_db_path` cleared on `swap_snapshot`).** This phase opportunistically lands NEW-3: `swap_snapshot` MUST set both `prev_db_path = None` AND drop `prev_pool`. The fallback path at `advance.rs:490-491/759-760` (which currently warns when `prev_db_path` is `None`) keeps the same warning text but is now reachable via "explicit clearing happened, caller forgot to set prev for this advance" — strictly improves observability over the current "stale-but-non-None" hazard. NEW-3 is in scope here because the prev-pool refactor invalidates without it.
- **D-12:** **Out of scope for NEW-1:** NEW-2 (silent error swallow) and NEW-5 (BEGIN DEFERRED) are ADJACENT and tempting to fix in the same edit but stay deferred. Rationale: NEW-2 needs a logging strategy (LogContext is TS-side; Rust uses `eprintln!` warnings) — that's its own design discussion. NEW-5 is now FREE because Option A's prev_pool inherits `BEGIN DEFERRED` from `ConnectionPool::new` — explicitly call this out in the SUMMARY but do NOT pretend it's a Phase 35 deliverable; it's a side effect of D-07.

### Cross-Cutting

- **D-13:** **TS-as-spec discipline does NOT apply here.** Both workstreams are Rust-internal hygiene + perf — there is no canonical TS counterpart to mirror. Decisions are documented with code-internal references (`connection_pool.rs:132-162`, `advance.rs:469-580`) rather than `builder.ts:N`-style citations. Do NOT invent a fake TS reference; state explicitly in the plan that this is Rust-internal.
- **D-14:** **No regressions on Phase 34 deliverables.** B11 cascade-delete tests (`test_b11_descendants_from_prev`, `test_b11_set_prev_snapshot_napi_present`, `test_b11_fallback_when_prev_not_set`, `diff-tests-track2.ts:739-875`) MUST stay green. The prev-pool refactor is internal — the napi surface (`set_prev_snapshot`) keeps the same signature.
- **D-15:** **No signature changes to existing buffered methods** (CLAUDE.md gate #4 carries forward). Pool API may grow new methods; existing public methods may NOT change argument shape. `swap_path` keeps `(&self, &str) -> Result<()>`; new behavior lives in `swap_path_with_timeout` and `swap_path_now`.
- **D-16:** **Bench gate.** Phase 34 left `v5.0-bench-results.md` with TTFB and MemPeak baselines. Phase 35 adds a NEW bench section: cascade-delete throughput on a 3-level / 100-row fixture. Acceptance: ≥2× throughput improvement post-NEW-1. Phase 33 thresholds (TTFB <1.5×, MemPeak ≥4×) MUST NOT regress. Bench result file remains `v5.0-bench-results.md` (append-only) until v6.0 milestone gets its own bench file.

### Verification Gate

- **D-17:** Phase 35 verification gate:
  1. All callers of `ConnectionPool` methods use the unified `PoolError` (or convert at the napi boundary). `grep -rn "\\.unwrap()" packages/zqlite-rs/src/connection_pool.rs packages/zqlite-rs/src/pipeline_manager.rs packages/zqlite-rs/src/table_source.rs | grep -v "test" | grep -v "//"` returns 0 lines on inner mutexes.
  2. `emit_descendant_removals` opens at most one sqlite Connection per advance batch — proven by an integration test that wraps `Connection::open_with_flags` and counts (or by per-thread metric in `advance.rs`).
  3. Cascade-delete throughput ≥2× on the new 3-level / 100-row bench (results appended to `v5.0-bench-results.md`).
  4. Full `cargo test` for both `zqlite-rs` and `zero-ivm-rs` green.
  5. `tools/ivm-parity/` differential tests (especially `diff-tests-track2.ts`) green — B11 carry-forward tests don't regress.
  6. Phase 33 benches (TTFB <1.5×, MemPeak ≥4×) re-run and PASS.

### Claude's Discretion

- **CD-01:** Exact retry-loop polling cadence (linear vs exponential) and the cutover threshold to log a warning. Implementer picks based on existing pool instrumentation conventions.
- **CD-02:** Whether `prev_pool` lives on `Instance` (per-pipeline-manager) or on the parent `RustPipelineManager`. Trade-off: per-instance is simpler but allocates one prev_pool per query subscription; shared at the manager level requires a `(instance_id → prev_pool)` map. Default to per-instance for symmetry with `shared_pool`; revisit if profiling shows pressure.
- **CD-03:** Whether to expose `swap_path_with_timeout` as a napi method or keep it Rust-only with `swap_snapshot` calling it internally. Default: Rust-only — TS callers don't need finer-grained control.
- **CD-04:** Whether the prepared-statement cache in `emit_descendant_removals` is HashMap-keyed or Vec-of-tuples (LRU). Default: HashMap with no eviction — the recursion has bounded depth so the map naturally bounds.
- **CD-05:** Whether to also update `path()` callers (currently silent unwrap_or_else) — recommended yes, but if the call surface is large enough to scope-creep, leave a TODO and capture as Wave 4 follow-up.

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Audits (drive both workstreams)

- `.planning/IVM-PORT-AUDIT-DEEP.md` §B10 (lines 138-150) — `swap_path` retry / unified poison concerns.
- `.planning/IVM-PORT-AUDIT-DEEP.md` §NEW-1 (lines 212-225) — per-call connection open in `emit_descendant_removals`.
- `.planning/IVM-PORT-AUDIT-DEEP.md` §NEW-3 (lines 232-237) — `prev_db_path` not cleared on `swap_snapshot` (in-scope coordination).
- `.planning/IVM-PORT-AUDIT-DEEP.md` §NEW-5 (lines 246-250) — prev connection lacks `BEGIN DEFERRED` pin (free side effect of D-07).

### Code under modification

- `packages/zqlite-rs/src/connection_pool.rs` — full file. Lines 132-162 (`swap_path`), line 115 (`path()` silent recovery), line 56 (`BEGIN DEFERRED` pattern that prev_pool inherits), lines 7-13 (`PoolError` enum to extend), lines 27-33 (`From<rusqlite::Error>` impl that may need siblings).
- `packages/zqlite-rs/src/pipeline_manager.rs` — `.unwrap()` poison surface at lines 290 (advance), 318 (set_prev_snapshot), 330 (swap_snapshot), 336 (pipeline iteration). `Instance` struct around line 69 (where `prev_db_path` lives — `prev_pool` lands here).
- `packages/zqlite-rs/src/table_source.rs` — `.unwrap()` sites at lines 228, 240, 241 (`reset_state`, write_conn lock).
- `packages/zqlite-rs/src/advance.rs` — lines 469-583 (`emit_descendant_removals`), recursion at line 576-579, four reachable call sites at lines 1332, 1394, 1553, 1628 (per audit).

### TS Pipeline-Driver (caller, no changes expected)

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:2171, 2317, 2409` — `setPrevSnapshot` call sites. NEW-1 is internal Rust; TS surface unchanged unless D-15 violated.

### Phase 34 Carry-Forward

- `.planning/phases/34-differential-fuzz-schema-extension/34-06-PLAN.md` — B11 plan; sets up the prev_db_path field and `set_prev_snapshot` napi method that NEW-1 extends.
- `.planning/phases/34-differential-fuzz-schema-extension/34-CONTEXT.md` D-15 — Track 2 BLOCKING fix list including B11 (NEW-1 sits on top of this).
- `tools/ivm-parity/diff-tests-track2.ts:739-875` — B11 differential tests that MUST stay green.
- `.planning/milestones/v5.0-bench-results.md` — bench log; Phase 35 appends cascade-throughput section here.

### Pool semantics references

- `rusqlite::Connection::open_with_flags` — used in pool init AND in `emit_descendant_removals` (the opener that NEW-1 displaces).
- `rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX | SQLITE_OPEN_URI` — flags pattern that prev_pool reuses (line 48-50 in `connection_pool.rs`).
- `BEGIN DEFERRED` — the snapshot-pin transaction; `set_snapshot()` at line 105-111 commits + restarts.

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- **`ConnectionPool` itself** — already has `swap_path`, `set_snapshot`, `available()`, `pool_size()`. Prev-pool reuses 100% of this; just construct a second instance with `pool_size: 1`.
- **`PooledConnection` Drop guard** — RAII return-to-pool already correct; no change needed for prev_pool.
- **`PoolError` `From<rusqlite::Error>`** — extends naturally for `From<std::io::Error>` if NEW IoError variant lands.
- **Test helpers in `connection_pool.rs::tests`** — `create_test_db()` is reusable for poison/exhaustion fault-injection helpers (D-06).

### Established Patterns

- **`map_err(|_| PoolError::Poisoned)?`** — already used in `set_snapshot`, `available`, `swap_path`. The hygiene replacement just propagates this pattern outward.
- **napi error conversion** — `pipeline_manager.rs` consistently uses `napi::Error::from_reason(format!("...: {e}"))` to lift Rust errors. Pool errors follow same pattern.
- **Per-instance pool model** — `Instance.shared_pool: ConnectionPool` is the existing precedent for D-08 / CD-02 ("per-instance prev_pool" symmetry).
- **Bench append-only log** (`v5.0-bench-results.md`) — Phase 34 left this open; Phase 35 appends a new section.

### Integration Points

- **`pipeline_manager.rs::swap_snapshot`** — current call site of `pool.swap_path`. Migrates to retry variant per D-04. Also lands the `prev_pool = None` clear per D-11.
- **`pipeline_manager.rs::set_prev_snapshot`** — current call site of `instance.prev_db_path = Some(...)`. Extends to construct prev_pool (D-08).
- **`advance.rs::emit_descendant_removals`** — current ad-hoc `Connection::open_with_flags`. Receives a borrowed `&PooledConnection` (or `&ConnectionPool`) instead of a path string.
- **`table_source.rs:228, 240, 241`** — three `.unwrap()` sites in `swap_db` and `reset_state`. Convert to explicit error propagation per D-03.

### Performance Constraints

- **Phase 33 thresholds carry forward as no-regression gates.** TTFB-streaming `firstChunkT < 1.5 * minPipelineDelay`. MemPeak-streaming `streamingPeak * 4 <= bufferedPeak * 1.2`.
- **D-16 cascade-throughput target: ≥2×.** New bench fixture at 3 levels, 100 rows. Implementation lives in `packages/zero-cache/bench/cascade-delete-bench.test.ts` (or wherever the existing rust-ivm benches live; researcher confirms exact path).
- **No regression on B11 differential tests** — `diff-tests-track2.ts:739-875` is the truth source for cascade correctness.

</code_context>

<specifics>
## Specific Ideas

- The user explicitly framed this as a **design-level** phase: B10 is "unify the error model so callers stop hand-rolling poison handling," NEW-1 is "stop opening sqlite per recursion." Both can ship without touching IVM operator semantics. **No parity risk.** The verification gate leans on `cargo test` + Phase 34's existing differential corpus, plus the new cascade-throughput bench.
- B10's retry loop is intentionally tight (≤750ms total) because the only legitimate caller — `swap_snapshot` — runs on the advance thread. Long retries here would block sync. If a connection is _truly_ leaked (not just briefly held), 750ms is enough to surface that as an error rather than mask it.
- NEW-1's win is amortized syscalls + statement preparation. With the existing `BEGIN DEFERRED` snapshot pin, the prev_pool ALSO closes NEW-5 for free — write that up in the SUMMARY so nobody plans NEW-5 as a separate effort.
- NEW-3 lands in this phase because not landing it leaves prev_pool keyed on stale `prev_db_path` after `swap_snapshot` — the refactor is unsafe without it. Treat NEW-3 as a sub-deliverable of NEW-1, NOT a separate Wave.
- The `unsafe impl Send/Sync` in `table_source.rs:70-71` and the `last_pushed_epoch` raw mutation (B14) are TEMPTING to fix while in `table_source.rs` for `.unwrap()` cleanup. **Resist.** B14 is its own design problem (`AtomicU64` migration) and warrants a focused plan; combining it here would balloon Wave 1's diff and conflate two unrelated concerns.

</specifics>

<deferred>
## Deferred Ideas

### v6.0 candidates (not in Phase 35 scope)

- **B5** — OrExists short-circuit on first matching branch.
- **B6** — EXISTS_LIMIT downgrade not honored (security-relevant).
- **B7** — FlippedJoin + UnionFanIn + UnionFanOut operator family (sized as own phase).
- **B8** — `compare_values` lossy for i64 > 2^53.
- **B12** — Companion scalar `resolved_value` drift causes spurious resets.
- **B13** — `unwrap_or_default()` on state-key serialization (NIT).
- **B14** — `unsafe` raw mutation of `last_pushed_epoch` (deserves its own AtomicU64 plan).
- **NEW-2** — Silent error swallow in `emit_descendant_removals` (needs logging-strategy discussion).
- **NEW-4** — B1 inner-And ordering caveat (`[simple, csq]` vs TS's `[csq, simple]` partition).
- **TS-oracle env import resolution** (Phase 34 D-22 carry-forward).
- **CI integration of `npm run fuzz-check:gate`** (Phase 34 D-22 carry-forward).

### Reviewed Todos (not folded)

None — no pending todos in `.planning/todos/pending/` matched this phase's scope.

</deferred>

---

_Phase: 35-pool-cascade-hardening_
_Context gathered: 2026-04-30_
