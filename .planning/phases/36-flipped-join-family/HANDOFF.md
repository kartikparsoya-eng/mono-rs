# Phase 36 — Execution Handoff (Waves 0-1 complete)

**Status:** Wave 0 + Wave 1 COMPLETE. Waves 2-6 NOT-STARTED.
**Branch:** `worktree-agent-acf6b4dc82fd28de0`
**Test deltas:** zero-ivm-rs 154 → 190 (+36 tests).

---

## Commits

- `19c60021f` — Wave 0: research stubs (FlippedJoin/UnionFanIn/UnionFanOut
  port-mapping tables in module docstrings; struct skeletons; lib.rs
  registrations).
- (Wave 1 commit hash will be captured at session end) — Wave 1: FanOut+FanIn
  composite + push_accumulated helpers + pipeline.rs OperatorConfig::FanOut +
  zqlite-rs hydrate.rs stub arms.

---

## Wave-by-wave status

### Wave 0 (research stubs) — ✅ COMPLETE

All `must_haves` satisfied:

- ✅ `flipped_join_op.rs` 153 LOC (≥80 required), 16 TS spec citations.
- ✅ `union_fan_in_op.rs` 122 LOC (≥60 required).
- ✅ `union_fan_out_op.rs` 62 LOC (≥30 required).
- ✅ `36-RESEARCH.md` validated — 8 H2 sections present, 30+ TS:line citations.
- ✅ `cargo check -p zero-ivm-rs` clean.

### Wave 1 (FanOut + FanIn) — ✅ COMPLETE (with documented deviation)

All `must_haves` satisfied:

- ✅ FanOut + FanIn implement filter-graph fork/merge via composite operator.
- ✅ `OperatorConfig::FanOut { branches }` round-trips JSON.
- ✅ 31 new unit tests (17 push_accumulated + 14 fan_out_op) — ≥20 plan minimum.
- ✅ `pipeline.rs` 5 round-trip + builder tests added.
- ✅ `cargo test -p zero-ivm-rs` 190 passing.

**Documented deviation from plan D-06:** Plan called for two separate
`FanOutOperator` and `FanInOperator` types communicating via
`Arc<Mutex<dyn Operator>>` callbacks. Rust's synchronous-push Operator trait
(returns `Vec<Change>`) made the split structurally redundant — the only
purpose of the TS split is the cooperative-yield seam, which evaporates in
Rust. Implemented as a single composite `FanOutOperator` owning N branch
pipelines, mirroring the existing `OrExistsOperator` pattern (Phase 17/35).
The wire format still uses `OperatorConfig::FanOut { branches }` per plan.
The `FanIn` role is implicit at the same operator boundary.

**Risk assessment for Wave 4:** This deviation simplifies, not complicates,
Wave 4 AST translation. The `applyFilterWithFlips` Rust port emits a single
`OperatorConfig::FanOut { branches: [...] }` (or `UnionFanOut` for flipped),
not paired FanOut/FanIn nodes.

### Wave 2 (UnionFanOut + UnionFanIn) — ❌ NOT STARTED

**This is R-01 HIGH-RISK.** Per CONTEXT, this is the wave most likely to
require a spike.

**Path forward:**

1. Read `.planning/phases/36-flipped-join-family/36-03-PLAN.md` carefully.
2. Apply the same composite-operator deviation as Wave 1: implement a single
   `UnionFanOutFanInOperator` (or per plan, `UnionFanInOperator` owning the
   branches with the protocol embedded).
3. The actual NEW work vs Wave 1: **dedup-by-PK k-way merge in `fetch`**.
   This is implemented via `BinaryHeap<(Reverse<RowKey>, branch_idx,
node_idx)>`. The `compareRows` ordering comes from a SortSpec passed at
   construction.
4. **Internal-vs-external push** (TS line 142-181): when phase is Idle and a
   ChildChange-via-flip-join arrives, decide forward vs suppress by checking
   if any _other_ branch has the row via PK constraint fetch. The composite
   approach makes this a `for branch in &mut self.branches` loop.
5. Follow Wave 1's test fixture pattern: build N filter-branches over
   in-memory `SourceOperator`, push changes, assert `Vec<Change>` outputs.

**Test list (≥30 tests required, from plan 36-03):**

- 10 fetch tests (dedup, reorder, multi-branch overlap)
- 8 push tests (external-add, external-remove, in-fan-out accumulate,
  done-pushing flush)
- 2 schema/protocol assertion tests

**Key files to read line-by-line:**

- `packages/zql/src/ivm/union-fan-in.ts` (entire file — 296 LOC)
- `packages/zql/src/ivm/union-fan-in.test.ts` for fixtures (268 LOC)
- `push_accumulated.rs` (Wave 1 — already has `merge_relationships` and
  `make_add_empty_relationships` ready for UnionFanIn use)

**Risk-mitigation:** if the dedup-merge `BinaryHeap` doesn't naturally fall
out within 1 day of effort, escalate to a 1-day spike on the
`pushAccumulatedChanges` semantics per CONTEXT R-01.

### Wave 3 (FlippedJoin) — ❌ NOT STARTED

**Largest wave — ~600 LOC + 50 tests.** Read
`packages/zql/src/ivm/flipped-join.ts` line-by-line; the port-mapping table
in `flipped_join_op.rs` module docstring (Wave 0) is the spec.

The InProgressOverlay struct is already declared. push_parent and push_child
each follow the same shape as JoinOperator's push paths but with reversed
parent/child roles.

### Wave 4 (AST translation) — ❌ NOT STARTED

Implement `append_condition_configs_with_flips` in
`packages/zqlite-rs/src/ast_to_config.rs`. The plan-36-04 has the Rust
sketch on lines 226-280 of 36-RESEARCH.md.

**Integration concern (per task input):** worktree
`worktree-agent-ae8949a4209fedfc6` modified `ast_to_config.rs` for B6 +
NEW-4. Wave 4 must add a NEW function (`append_condition_configs_with_flips`),
not rewrite the existing `append_condition_configs`.

### Wave 5 (parity allow-list shrink) — ❌ NOT STARTED

Remove `fuzz_00132` and `fuzz_00133` from
`tools/ivm-parity/parity-allowlist.json`. Add ≥3 planner-emission tests
(per plan 36-05) confirming round-trip from ZQL → planner → AST → Rust
operator tree → fetch result.

### Wave 6 (verification gate) — ❌ NOT STARTED

Full `cargo test -p zero-ivm-rs` + `cargo test -p zqlite-rs` + vitest +
`tools/ivm-parity` differential fuzz + Phase 33 benchmarks.

---

## Files modified / added (Waves 0-1)

- **NEW**: `packages/zero-ivm-rs/src/flipped_join_op.rs` (153 LOC, stub)
- **NEW**: `packages/zero-ivm-rs/src/union_fan_in_op.rs` (122 LOC, stub)
- **NEW**: `packages/zero-ivm-rs/src/union_fan_out_op.rs` (62 LOC, stub)
- **NEW**: `packages/zero-ivm-rs/src/push_accumulated.rs` (458 LOC, complete
  implementation + 17 tests)
- **NEW**: `packages/zero-ivm-rs/src/fan_out_op.rs` (280 LOC, complete
  composite + 14 tests)
- **MODIFIED**: `packages/zero-ivm-rs/src/lib.rs` (+5 module declarations)
- **MODIFIED**: `packages/zero-ivm-rs/src/pipeline.rs` (+`FanOut` config
  variant + `build_operator` arm + 5 tests)
- **MODIFIED**: `packages/zqlite-rs/src/hydrate.rs` (stub error arms for
  FanOut in 3 match sites)

---

## Integration concerns with parallel Wave-1 lanes

Per task input:

- ✅ `or_exists_op.rs` (B5 fix in `worktree-agent-affd500f9c6035890`) —
  not touched. No conflict.
- ⏳ `ast_to_config.rs` (B6 + NEW-4 in `worktree-agent-ae8949a4209fedfc6`) —
  not touched. Wave 4 will need to coexist; the plan's
  `append_condition_configs_with_flips` is a new function, not a rewrite of
  `append_condition_configs`.
- ✅ Other Wave 1 lanes (B14 epoch, B12 companion, B8 i64) — not touched.

---

## Parity allow-list state

Unchanged. `fuzz_00132` and `fuzz_00133` remain in
`tools/ivm-parity/parity-allowlist.json`. Wave 5 removes them once Wave 4
ships and the FlippedJoin path is end-to-end functional.

---

## Honest production-readiness assessment

**Wave 0+1 ARE production-ready:**

- `push_accumulated_changes` and `merge_relationships` are the helper
  primitives that Waves 2 and 4 will both consume — they have 17 unit tests
  including should-panic invariant checks.
- `FanOutOperator` is a complete, working filter-graph operator with 14 unit
  tests covering all change-type paths.
- The new `OperatorConfig::FanOut` variant is JSON-compatible with the
  existing wire format.

**However, the FlippedJoin port as a whole is GATED on Waves 2-6.** The
emitted operator-tree from a `flip:true` AST condition is still treated as
a regular Exists by `ast_to_config.rs` (the silent-treatment finding from
B7). That bug is not yet fixed. Waves 2-6 must complete before B7 closes.

**Recommendation:** Schedule a focused 3-week effort for Waves 2-6 with
Wave 2 spike-budget at the start. Waves 0+1 reduce that effort by ~25%
relative to a from-scratch port (the helper module + composite-operator
pattern + research stubs are foundational and reusable).
