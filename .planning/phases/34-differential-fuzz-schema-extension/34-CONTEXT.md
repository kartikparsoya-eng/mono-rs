# Phase 34: Differential Fuzz + Schema Extension - Context

**Gathered:** 2026-04-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Two parallel tracks delivered in one phase:

**Track 1 — Fuzzer:** Build a property-based differential fuzzer that generates random `(schema, AST, data, change-stream)` tuples and asserts TS↔Rust IVM produce identical results. Lives in `tools/ivm-parity/` (extends the existing two-cache harness, not a new in-process vitest fuzz).

**Track 2 — Deep-audit hardening:** Fix four BLOCKING-tier silent-correctness items found in `.planning/IVM-PORT-AUDIT-DEEP.md` (B1, B2, B3, B11) before/during fuzzer rollout, so the fuzzer's first runs aren't flooded by already-known issues. Track 2 fixes are root-cause, TS-as-spec, no shortcuts.

Out of scope for this phase: B5–B14 from the deep audit (deferred to Phase 35), in-process vitest random-AST fuzz (option B was rejected), CI integration of the fuzz suite (deferred), 5 catalogued divergences in `PARITY_STATUS.md` that map to deeper structural work (FlippedJoin, scalar EXISTS).

</domain>

<decisions>
## Implementation Decisions

### Tooling Location

- **D-01:** Fuzzer extends `tools/ivm-parity/` (option A). Two `zero-cache` instances — TS upstream from `/private/tmp/ivm-parity-ts-ref` git worktree of `rocicorp/mono`, RS via `ZERO_USE_RUST_IVM_V2=1` from this repo — share a Postgres on `:6434`. The harness is out-of-process, real binaries on both sides.
- **D-02:** Reject option B (in-process vitest fuzz importing TS as a library). Mono-rs's `tsAdvance` is empty-stub by architectural necessity (no surviving in-process TS pipeline state); a library-import oracle would still have the same handicap.
- **D-03:** New code lands in `tools/ivm-parity/`, not `packages/zero-cache/src/services/view-syncer/random-ast-parity.fuzz.test.ts`. The success-criteria-allowed alternate location (in-package vitest) is rejected.

### Random-AST Fuzz (FUZZ-01)

- **D-04:** **Hybrid generator**: keep the existing bounded-BFS enumerator (`ast-fuzz.ts`) AS-IS as the deterministic regression baseline. ADD a fast-check generator that produces random ASTs across the full operator surface (filter, join, exists, or-exists, take, cap-when-emitted, skip, related[], correlated subquery). Both run in `npm test`; BFS first (deterministic catch of regressions), then fast-check (exploration with shrinking).
- **D-05:** Default `FUZZ_NUM_RUNS=1000`. Expandable via env var. Per ROADMAP success-criterion #4: must complete in <2 minutes for 1k iterations (CI budget). Two-process harness makes this tighter than in-process — implementation must batch reuses of the harness fixtures rather than restart per AST.
- **D-06:** fast-check shrinking is the WIN over BFS — every divergence must be shrunk to a minimal counterexample before being recorded as a regression test. Don't disable shrinking for speed; budget is via num-runs, not shrink-disable.
- **D-07:** AST generators are written in TS in `tools/ivm-parity/`; they emit the same `AST` JSON the existing harness already round-trips. Shrinkers operate on AST JSON, not Rust types.
- **D-08:** Generate with awareness of the deep-audit findings — specifically include shapes that exercise B1 (Skip + EXISTS), B3 (limit + related[] + child mutation), B5 (multiple OR'd EXISTS sharing a child source). Track 2 will land fixes for B1/B2/B3/B11; the fuzzer must be able to PROVE the fixes work by hitting these shapes.

### Schema Extension (FUZZ-02)

- **D-09:** Schema density target: **production-shape**. Match the density of `apps/zbugs/shared/schema.ts` (Zero's reference app). One column of each rich type is the floor, not the goal — composite tables with multiple jsonb/timestamptz columns reflect real production schemas.
- **D-10:** Required types: `jsonb`, `timestamptz`, `numeric`, nullable variants of each scalar, at least one composite/array column.
- **D-11:** Required NULL semantics fixtures: NULL ≠ NULL in equality, NULL propagation in arithmetic, NULL in JOIN keys (must produce no row), NULL in OR (three-valued logic).
- **D-12:** Required type coercion fixtures: string→number (lexical vs numeric ordering), numeric precision boundaries (i64 > 2^53 — directly hits B8 and B9 from the deep audit, treat as in-scope discovery), date/time round-trips (timestamptz across DST boundaries).
- **D-13:** Defer adversarial cases (NaN numerics, ±∞ timestamps, 0-length arrays, deeply-nested jsonb) to Phase 35 unless the production-shape fuzz hits them organically.
- **D-14:** Schema is data not code — extend `tools/ivm-parity/zero-schema.ts` and the corresponding `schema.sql` + `seed.sql` + `seed-extras.sql`. The existing `ast-fuzz.ts` and harnesses re-read schema at runtime (per ast-fuzz.ts header) and self-extend. Do NOT modify the harness fuzzer logic for type-handling — that work belongs in the operator implementations.

### Deep-Audit Hardening (Track 2)

- **D-15:** **In-scope BLOCKING fixes (must land in Phase 34)**:
  - **B1** — Skip placement order. TS does Skip→CSQ→Filter (`builder.ts:302`); Rust does Filter+Exists→Skip (`ast_to_config.rs:226-238`). Fix: in `ast_to_operator_configs`, push Skip immediately after Source, before `append_condition_configs`. Spec source: TS `builder.ts:302-331`.
  - **B2** — `parent_sizes.insert(pk, children.len().max(1))` poisons cache when or_predicate matches (`exists_op.rs:144`). Fix: cache real `children.len()`. Or_predicate decision re-evaluated on demand. Spec source: TS `exists.ts` push path — Rust must mirror.
  - **B3** — Take fetch/push state-key inconsistency when `partition_key: None` + constraint. Silent push-as-no-op for child Takes. Fix: thread `partition_key` through `ast_to_operator_configs` (also closes Risk #1 from original audit). Spec source: TS `take.ts` partition handling + builder's `applyTake` call sites.
  - **B11** — `emit_descendant_removals` reads from POST-tx snapshot (`advance.rs:450`); same-tx descendant deletions silently elided. Fix: route the read through the prev-snapshot path TS already maintains. Spec source: TS `pipeline-driver.ts` diff iterator from `prev`.

- **D-16:** **Out-of-scope this phase (deferred to Phase 35)** with explicit rationale:
  - **B5** (OrExists short-circuit) — small fix, but isolated; phase budget tight.
  - **B6** (EXISTS_LIMIT downgrade) — security-relevant for PERMISSIONS_EXISTS_LIMIT but not in active production exploitation path; flag as Phase 35.
  - **B7** (FlippedJoin missing) — large work (new operator family with UnionFanIn/UnionFanOut). Belongs to its own phase (35.x) or v6.0.
  - **B8/B9** (i64 > 2^53 precision) — FUZZ-02 may hit these organically; if so, fix in this phase under Track 1's "any divergence becomes a regression test before being fixed" rule. If not, Phase 35.
  - **B10** (swap_snapshot poisoned lock) — operational, not parity.
  - **B12** (companion scalar resolved_value drift) — moderate; defer.
  - **B13/B14** (NITs) — defer.

- **D-17:** **TS-as-spec discipline.** Every Track 2 fix cites a specific TS file:line as the canonical reference. Implementation must mirror TS semantics — no novel Rust-side behavior. If a TS↔Rust divergence is "Rust does it slightly differently for performance," that is a hack and rejected. Performance optimizations live in v6.0 ONLY after parity is proven.

- **D-18:** **No regressions.** Every Track 2 fix must:
  1. Add a Rust unit test asserting the new behavior (with a comment citing TS file:line spec)
  2. Add a TS↔Rust differential test in `tools/ivm-parity/` for the shape that was silently broken
  3. Pass full `cargo test` for `zero-ivm-rs` + `zqlite-rs`
  4. Pass full vitest suite under `packages/zero-cache/src/services/view-syncer/`
  5. Pass `streaming-vs-buffered-parity.fuzz.test.ts` with 1k iterations
  6. Pass Phase 33 benchmarks (TTFB threshold 1.5×, MemPeak threshold 4×) — performance MUST NOT regress

### Track Ordering and Coupling

- **D-19:** **Parallel execution, with synchronization point.** Track 1 (fuzzer build) and Track 2 (B1/B2/B3/B11 fixes) execute in parallel waves. Track 1's full-corpus run (the "find what else is broken" pass) waits until Track 2 lands so the fuzzer reports against a known-good baseline. Track 1 can develop and self-test against the existing schema in parallel.
- **D-20:** Any divergence the fuzzer finds beyond B1/B2/B3/B11 in Phase 34 becomes:
  - A regression test (in `tools/ivm-parity/`) — captured even if not fixed
  - An entry in `PARITY_STATUS.md` with root-cause notes
  - A planned fix in Phase 35 unless trivially fixable in this phase

### Verification Gate

- **D-21:** Phase 34 verification gate (per success-criterion #3 and "no regressions" mandate):
  1. All four BLOCKING fixes (B1/B2/B3/B11) have Rust unit tests AND `tools/ivm-parity/` differential tests committed.
  2. `npm test` from `tools/ivm-parity/` exits 0 with the post-Phase-34 baseline.
  3. fast-check fuzz with `FUZZ_NUM_RUNS=1000` completes in <2 minutes and reports zero unexpected divergences (allow-list = the 5 catalogued in `PARITY_STATUS.md` carry-forward + B5/B6/B7/B12 deferred items, with explicit allow-list entry for each).
  4. Phase 33 benches (TTFB, MemPeak) re-run and PASS.
  5. Full `cargo test` + full vitest suite green.
- **D-22:** No CI integration in this phase. The fuzzer is intended for CI in Phase 35 (or whenever the divergence catalog is fully drained), but Phase 34 just lands the runnable harness + the Track 2 fixes. Decision rationale: build the tool, prove it finds bugs, fix the bugs, THEN automate. Don't ship a CI gate that knowingly fails.

### Claude's Discretion

- **CD-01:** fast-check arbitrary structure (combinator composition, custom shrinkers vs default, whether to build a single `arbAst` or compose per-operator arbitraries). Researcher/planner picks based on fast-check idiom.
- **CD-02:** How harness fixtures are reused across iterations (Postgres truncate-and-reseed vs deterministic schema-per-iteration). Implementation detail.
- **CD-03:** Whether `tools/ivm-parity/`'s `synth-p6.ts`, `synth-p7.ts` (existing AST synthesizers) get folded into the new fast-check generator or kept separate. Research can decide.
- **CD-04:** Allow-list format in the harness for the deferred items — flat list, JSON, or in-test annotation. Pick what fits the existing harness style.

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Audits (drive Track 2 fixes)

- `.planning/IVM-PORT-AUDIT.md` — Original audit; AUDIT-01..04 already shipped in Phase 30. Risk #1 (`partition_key` not threaded) is the same root cause as B3 — closing B3 closes Risk #1 simultaneously.
- `.planning/IVM-PORT-AUDIT-DEEP.md` — 14 new findings beyond the original 6. Track 2 scope: §B1, §B2, §B3, §B11. Recommended-ordering: §D.

### TS IVM (canonical spec for Track 2 fixes)

- `packages/zql/src/ivm/builder.ts` — Pipeline construction order. **§302-331** is the authoritative ordering for B1 (Skip → CSQ → Filter). **§316-319** is the EXISTS_LIMIT downgrade for B6 (deferred but reference for proper handling).
- `packages/zql/src/ivm/exists.ts` — Reference for B2 (Exists push semantics, parent count maintenance, or_predicate handling).
- `packages/zql/src/ivm/take.ts` — Reference for B3 (Take partition_key handling, fetch/push state-key consistency).
- `packages/zql/src/ivm/operator.ts` — Input/Output/Storage interfaces. Both TS and Rust must mirror.
- `packages/zql/src/ivm/skip.ts` — Reference for Skip ordering (B1).

### TS Pipeline-Driver (orchestration spec for B11)

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — **§1326, §1402** snapshot swap call sites; the diff iterator from `prev` that B11 must match. Note: this is the upstream TS reference behavior; the Rust caller is the same file in mono-rs but must be re-read against the upstream copy in `/private/tmp/ivm-parity-ts-ref/packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` since mono-rs has been edited.

### Rust IVM (sites being modified by Track 2)

- `packages/zqlite-rs/src/ast_to_config.rs` — **§226-238** is the Skip-after-conditions ordering bug (B1). Also where `partition_key` plumbing lands (B3).
- `packages/zero-ivm-rs/src/exists_op.rs` — **§144** is the `.max(1)` cache poison (B2). **§243** is the existing or_predicate Edit branch.
- `packages/zero-ivm-rs/src/take_op.rs` — **§55-67** (`take_state_key`), **§317-329** (fetch fallback), **§390** (push early-return when state missing). All three sites coordinate for B3.
- `packages/zqlite-rs/src/advance.rs` — **§450** is the post-tx snapshot read for descendant removal (B11).
- `packages/zqlite-rs/src/connection_pool.rs` — Reference for B11 fix (prev-snapshot read path).

### Existing Parity Tooling (Track 1 extends)

- `tools/ivm-parity/README.md` — Run procedures, golden-snapshot mode, flow.
- `tools/ivm-parity/INDEX.md` — File map, when-to-read-what.
- `tools/ivm-parity/SKILL.md` — Hard rules and run procedure.
- `tools/ivm-parity/ast-fuzz.ts` — Existing bounded-BFS enumerator (KEEP — D-04). New fast-check generator coexists.
- `tools/ivm-parity/harness-coverage.ts` — Hydrate sweep harness; new fuzz reuses its TS↔RS roundtrip primitives.
- `tools/ivm-parity/harness-advance-coverage.ts` — Advance sweep harness with `MUTATIONS` block.
- `tools/ivm-parity/zero-schema.ts` — Schema definition (extend for FUZZ-02).
- `tools/ivm-parity/schema.sql` + `seed.sql` + `seed-extras.sql` — PG DDL and data (extend for FUZZ-02).
- `tools/ivm-parity/PARITY_STATUS.md` — Catalogues 1079/1084 sweep state and 5 known divergences. Phase 34 carries these forward, does not address.
- `tools/ivm-parity/divergence_report.md` — RS↔TS structural parity audit (companion to deep audit).

### Reference Application Schema (FUZZ-02 density target)

- `apps/zbugs/shared/schema.ts` — Production reference Zero schema. FUZZ-02's schema-extension density target.

### Phase 33 Carry-Forward (no-regression gate)

- `.planning/phases/33-performance-tuning/33-VALIDATION.md` — Phase 33 validation strategy.
- `.planning/milestones/v5.0-bench-results.md` — Append-only bench log; Phase 34 must add fresh runs and assert no regression.
- `.planning/phases/33-performance-tuning/33-REVIEW.md` — Open review items WR-01 (strict-mode advance unusable) and others; Phase 34 may pick up WR-05 (`messagesBuf` shared fixture) if it touches `rust-ivm-streaming-bench.test.ts`.
- `.planning/notes/2026-04-29-phase-34-scope-decision.md` — Initial scope decision (option A) that bootstrapped this discussion.

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- **`tools/ivm-parity/ast-fuzz.ts`** — Bounded-BFS AST enumerator. Schema-driven (re-reads `zero-schema.ts` at runtime). Hard caps prevent recursion blow-up (`MAX_WHERE_DEPTH`, `MAX_BRANCHES_PER_NODE`, `MAX_RELATED_DEPTH`, `MAX_RELATED_PER_NODE`, `MAX_RELATIONSHIP_HOPS`). KEEP — D-04. New fast-check generator runs alongside.
- **`tools/ivm-parity/harness-coverage.ts` + `harness-advance-coverage.ts`** — Hydrate and advance sweep harnesses. The TS↔RS roundtrip primitives (WebSocket, JSON encode/decode) are reusable; fast-check generator drives the same primitives with random ASTs instead of the BFS corpus.
- **`tools/ivm-parity/run-capture-golden.sh` + `run-replay-golden.sh`** — Golden-snapshot mode. Useful for fast-check shrinking iteration cycles where bringing TS up per iteration is too slow.
- **`tools/ivm-parity/refresh-stats.ts`** — Stats refresher; new fast-check fuzz hooks into the same reporting pipeline.
- **`tools/ivm-parity/parity-report.ts`** — Report generator; extends naturally for fast-check findings.
- **fast-check is already a project dep** — `node_modules/fast-check` at top level; used by existing `fuzz-ivm.test.ts` and `streaming-vs-buffered-parity.fuzz.test.ts`. No new install needed.

### Established Patterns

- **Two-process harness with shared Postgres** — TS on `:4858` (now via `run.sh` from `/private/tmp/ivm-parity-ts-ref` worktree), RS on `:4868` with `ZERO_USE_RUST_IVM_V2=1`, both pointing at `parity` DB on `:6434`. Phase 34 reuses verbatim.
- **`compareChanges` from `dual-executor.ts`** — Multiset-aware change comparison. Phase 34's fast-check assertion uses the same primitive.
- **Schema-as-data** — `ast-fuzz.ts` reads `zero-schema.ts` at runtime; FUZZ-02 schema additions auto-extend the corpus.
- **Append-only bench log** (`.planning/milestones/v5.0-bench-results.md`) — Pattern from Phase 33; Phase 34 adds entries here for any new fuzz-derived perf measurements.

### Integration Points

- **`packages/zero-cache/src/services/view-syncer/pipeline-driver.ts`** — Where Track 2 B11 fix lands (`emit_descendant_removals` consumer side).
- **`packages/zqlite-rs/src/ast_to_config.rs`** — Track 2 B1 + B3 fix site.
- **`packages/zero-ivm-rs/src/exists_op.rs`** — Track 2 B2 fix site.
- **`packages/zero-ivm-rs/src/take_op.rs`** — Track 2 B3 secondary fix site (state-key path).
- **`packages/zqlite-rs/src/advance.rs`** — Track 2 B11 primary fix site.
- **`tools/ivm-parity/zero-schema.ts`** — FUZZ-02 schema-extension entry point.

### Performance Constraints

- **Phase 33 bench thresholds carry forward as no-regression gates.** TTFB-streaming `firstChunkT < 1.5 * minPipelineDelay`. MemPeak-streaming `streamingPeak * 4 <= bufferedPeak * 1.2`.
- **Fuzz CI budget**: <2 minutes for `FUZZ_NUM_RUNS=1000` per ROADMAP success-criterion #4. Two-process harness is slower than in-process; consider batched fixture reuse.
- **No perf compromise rule (D-17)**: any "Rust does it slightly differently for performance" is a rejected hack. Parity first; performance is v6.0.

</code_context>

<specifics>
## Specific Ideas

- The user explicitly emphasized **TS as spec, root-cause fixes, no shortcuts, no perf or correctness compromise**. Every Track 2 commit message should cite the TS file:line that drove the fix. If a Rust unit test is needed to assert the fix, the test should also cite the TS spec line in its doc comment.
- The fuzzer's value is **finding shapes that aren't in the curated corpus**. fast-check shrinking on top of BFS is the explicit win — the BFS corpus stays as the regression baseline so existing coverage doesn't regress; fast-check adds the random exploration with shrinking that the user identified as "the single biggest correctness gap" earlier in this session.
- B3 is the headline fix. The deep audit notes: "every related-subquery with `limit` should be silently mishandling child-table edits/adds/removes." That's a real production-traffic bug masked by the fact that hydrate uses fetch (which works) and most tests don't exercise child mutations under related-with-limit. The Track 1 fuzz must include a generator that produces this shape, and the Track 2 fix must close it.
- Schema-extension density target is `apps/zbugs/shared/schema.ts`. zbugs is the project's own reference Zero application — using its schema density as the target keeps "production-shape" concrete instead of a vague aesthetic.

</specifics>

<deferred>
## Deferred Ideas

### Phase 35 candidates (deep-audit findings not in Phase 34 scope)

- **B5** — `OrExists.push_child` short-circuits on first matching branch. Small surgical fix; deferred for phase budget reasons.
- **B6** — `EXISTS_LIMIT` doesn't downgrade existing larger child Take. Includes `PERMISSIONS_EXISTS_LIMIT` security implication. Phase 35.
- **B7** — `flip: true` on CSQ silently treated as regular Exists. Requires implementing `FlippedJoin` + `UnionFanIn` + `UnionFanOut` operators in Rust — sized as its own phase or a v6.0 milestone item.
- **B8** — `compare_values` lossy for i64 > 2^53. May land in Phase 34 if FUZZ-02 hits it organically (large-int PKs); else Phase 35.
- **B9** — Wire-protocol JSON-fallback i64 precision loss. Pairs with B8.
- **B10** — `swap_snapshot` poisoned-lock no-retry. Operational, not parity.
- **B12** — Companion scalar `resolved_value` drift causes spurious resets. Phase 35.
- **B13** — `unwrap_or_default()` swallows state-key serialization errors. NIT, Phase 35 or v6.0.
- **B14** — `unsafe` raw mutation of `last_pushed_epoch`. NIT; replace with `AtomicU64`. Phase 35.
- **Risk #4 (original audit)** — Cap operator dead code. Cleanup; Phase 35.

### Phase 35+ candidates (catalogued divergences from `PARITY_STATUS.md`)

- 2× `OR(simple, EXISTS flip=true)` divergences (`fuzz_00132`, `fuzz_00133`) — same root as B7. Need FlippedJoin in Rust.
- 2× `scalar: true` EXISTS divergences (`fuzz_00139`, `fuzz_00140`) — need scalar-companion resolution in Rust pipeline manager.
- 1× duplicate CSQ alias overwrite (`seed_18`) — TS uniquifies; documented "won't fix" pre-Phase 34. Revisit during v6.0 if production impact emerges.

### Adversarial schema cases (out of FUZZ-02 production-shape scope)

- NaN numerics, ±∞ timestamps, 0-length arrays, deeply-nested jsonb. Phase 35 if the production-shape fuzz doesn't surface them organically.

### CI integration

- GitHub Action with PG service container running the fuzz suite per-PR. Decided AGAINST shipping in Phase 34 — explicit user preference. Wait until divergence catalog is drained, then automate.

### Reviewed Todos (not folded)

None — no pending todos in `.planning/todos/pending/` matched this phase's scope.

</deferred>

---

_Phase: 34-differential-fuzz-schema-extension_
_Context gathered: 2026-04-29_
