---
phase: 34
slug: differential-fuzz-schema-extension
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-04-29
---

# Phase 34 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property               | Value                                                                                   |
| ---------------------- | --------------------------------------------------------------------------------------- |
| **Framework**          | vitest 4.1.3 (TS) + cargo test (Rust) + custom Node tsx harness (`tools/ivm-parity/`)   |
| **Config file**        | `vitest.config.ts` per package; `Cargo.toml` per crate; `tools/ivm-parity/package.json` |
| **Quick run command**  | `cd packages/zero-ivm-rs && cargo test --release --quiet` (~6s)                         |
| **Full suite command** | See "Sampling Rate" below — multiple suites                                             |
| **Estimated runtime**  | ~30s quick, ~5min full incl. fuzz, ~2min fuzz alone                                     |

---

## Sampling Rate

- **After every task commit:**
  - Rust tasks: `cargo test --release --quiet` for the affected crate (zero-ivm-rs ~6s, zqlite-rs ~25s)
  - TS tasks: `npx vitest run <touched-test>` for the affected file
  - Harness tasks (`tools/ivm-parity/`): `npm run test-rs` from that dir
- **After every plan wave:**
  - `cd packages/zero-ivm-rs && cargo test --release --quiet` (full crate)
  - `cd packages/zqlite-rs && cargo test --release --quiet` (full crate)
  - `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts packages/zero-cache/src/services/view-syncer/parity-check.test.ts packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts`
  - For waves that touch `tools/ivm-parity/`: `cd tools/ivm-parity && npm test` (full sweep, ~3-5min)
- **Before `/gsd-verify-work`:**
  - Full vitest suite for `packages/zero-cache/`
  - `cargo test` green for `packages/zero-ivm-rs/` and `packages/zqlite-rs/`
  - Full `tools/ivm-parity/` sweep with allow-list passing
  - **fast-check fuzz**: `FUZZ_NUM_RUNS=1000 npm run fuzz` from `tools/ivm-parity/` completes in <2 min, zero unexpected divergences
  - **Phase 33 perf gate**: `npx vitest run packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts` — TTFB ratio < 1.5×, MemPeak ratio ≥ 4× (the v5.0-bench-results.md append-only log)
- **Max feedback latency:** 30s for quick, 5min for full

---

## Per-Task Verification Map

> Updated by planner during plan creation. Wave/plan/task IDs assigned by `gsd-planner`.
> Format: each task that closes a B-fix or builds harness functionality must have a verifiable `automated` command.

| Task ID  | Plan | Wave | Requirement                        | Threat Ref | Secure Behavior                                                                                             | Test Type                                                            | Automated Command                                                                                                       | File Exists | Status     |
| -------- | ---- | ---- | ---------------------------------- | ---------- | ----------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- | ----------- | ---------- |
| 34-XX-XX | TBD  | TBD  | B1 (Skip ordering)                 | —          | TS-spec parity for Skip→CSQ→Filter                                                                          | unit (Rust) + diff (harness)                                         | `cargo test --release skip_ordering_matches_ts -p zqlite-rs` + `cd tools/ivm-parity && npm run sweep:hydrate`           | ❌ W0       | ⬜ pending |
| 34-XX-XX | TBD  | TBD  | B2 (parent_sizes max(1))           | —          | Real child count cached, or_predicate evaluated on demand                                                   | unit (Rust) + diff (harness)                                         | `cargo test --release exists_parent_sizes_real_count -p zero-ivm-rs`                                                    | ❌ W0       | ⬜ pending |
| 34-XX-XX | TBD  | TBD  | B3 (Take partition_key threading)  | —          | fetch and push state keys match for child Takes                                                             | unit (Rust) + diff (harness with limit + related[] + child mutation) | `cargo test --release take_partition_state_consistency -p zero-ivm-rs` + `cd tools/ivm-parity && npm run sweep:advance` | ❌ W0       | ⬜ pending |
| 34-XX-XX | TBD  | TBD  | B11 (cascade-delete prev snapshot) | —          | descendant removals enumerate against prev, not curr                                                        | unit (Rust) + diff (harness with multi-table tx + FK cascade)        | `cargo test --release cascade_delete_uses_prev_snapshot -p zqlite-rs`                                                   | ❌ W0       | ⬜ pending |
| 34-XX-XX | TBD  | TBD  | FUZZ-01 (random-AST gen)           | —          | fast-check generates valid ASTs across full operator surface                                                | integration (Node tsx)                                               | `cd tools/ivm-parity && FUZZ_NUM_RUNS=10 npm run fuzz`                                                                  | ❌ W0       | ⬜ pending |
| 34-XX-XX | TBD  | TBD  | FUZZ-01 (1k iter <2min)            | —          | full fuzz run within CI budget                                                                              | perf (Node tsx)                                                      | `cd tools/ivm-parity && time FUZZ_NUM_RUNS=1000 npm run fuzz`                                                           | ❌ W0       | ⬜ pending |
| 34-XX-XX | TBD  | TBD  | FUZZ-02 (rich types schema)        | —          | jsonb, timestamptz, numeric, nullable, composite/array represented; NULL semantics + type-coercion fixtures | integration (Node tsx)                                               | `cd tools/ivm-parity && npm run db-migrate && npm run sweep:hydrate`                                                    | ❌ W0       | ⬜ pending |
| 34-XX-XX | TBD  | TBD  | No-regression gate                 | —          | Phase 33 benches don't regress (TTFB <1.5×, MemPeak ≥4×)                                                    | perf (vitest)                                                        | `npx vitest run packages/zero-cache/src/services/view-syncer/rust-ivm-streaming-bench.test.ts`                          | ✅          | ⬜ pending |

_Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky_

---

## Wave 0 Requirements

> Wave 0 = test fixtures, harness scaffolding, and infra needed before any feature work begins.

- [ ] `tools/ivm-parity/arb-ast.ts` — fast-check arbitraries for AST generation (per RESEARCH §2)
- [ ] `tools/ivm-parity/random-ast-fuzz.ts` — fast-check driver wrapping the existing harness
- [ ] `tools/ivm-parity/harness-fuzz.ts` — shared TS↔RS roundtrip primitive used by fast-check driver (extracted from `harness-coverage.ts`)
- [ ] **Timing probe**: 100-iteration fast-check run before extending FUZZ-02 schema (RESEARCH recommends Wave 1 probe to validate <2min budget)
- [ ] Allow-list config file at `tools/ivm-parity/parity-allowlist.json` — encodes 5 known divergences from `PARITY_STATUS.md` + 4 deferred items (B5, B6, B7, B12) for fuzz gating
- [ ] Rust unit-test stubs for B1/B2/B3/B11 (one per fix, written FIRST, asserting current-broken behavior so fix flips them green)
- [ ] TS↔RS differential test stubs in `tools/ivm-parity/` for the four B-fix shapes (one per fix)
- [ ] Schema additions in `tools/ivm-parity/zero-schema.ts` + `schema.sql` for FUZZ-02 (jsonb/timestamptz/numeric/nullable/composite tables)
- [ ] Seed data additions in `tools/ivm-parity/seed-extras.sql` for NULL semantics and type-coercion fixtures
- [ ] Integration with `npm test` script — fuzz step added to existing pipeline (BFS first, then fast-check)

_All Wave 0 items must complete before Wave 1+ feature implementation begins._

---

## Manual-Only Verifications

| Behavior                                          | Requirement            | Why Manual                                                                                                                   | Test Instructions                                                                                                     |
| ------------------------------------------------- | ---------------------- | ---------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| Production-realistic ingest under fast-check fuzz | FUZZ-01 + FUZZ-02      | Fuzz fixtures cover synthetic schemas; real production schemas may have shapes the BFS or fast-check generators don't reach. | Run `tools/ivm-parity/` with a real production-like schema (zbugs DB after seed). Manually inspect for parity gaps.   |
| Visual review of allow-list entries               | D-21 verification gate | Allow-list determines what counts as "expected" divergence — wrong entries hide real bugs.                                   | Review `tools/ivm-parity/parity-allowlist.json` PR by PR. Each entry must have a `reason` and a `phase_to_fix` field. |
| Performance under realistic concurrency           | No-regression gate     | Phase 33 benches measure single-pipeline timings; production has multi-client load.                                          | Optional: run `tools/ivm-parity/` with multiple sync workers, observe perf. Out of phase scope; track for Phase 35.   |

---

## Validation Sign-Off

- [ ] All B-fix tasks have `<automated>` verify (Rust unit + harness diff)
- [ ] All FUZZ tasks have `<automated>` verify (integration + perf)
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references (arb-ast.ts, harness-fuzz.ts, allow-list, schema additions, fix test stubs)
- [ ] No watch-mode flags (vitest run, not vitest watch)
- [ ] Feedback latency < 30s for quick, < 5min for full
- [ ] `nyquist_compliant: true` set in frontmatter
- [ ] Phase 33 bench gate (no perf regression) explicitly enforced before phase verification

**Approval:** pending
