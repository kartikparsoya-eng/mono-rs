# Phase 36: FlippedJoin Family Port — Context

**Gathered:** 2026-04-30
**Status:** Ready for planning

<domain>
## Phase Boundary

Single track: **Port the FlippedJoin family of IVM operators from TypeScript to Rust**, closing deep-audit finding **B7** and removing two parity allow-list entries (`fuzz_00132`, `fuzz_00133`).

Operators in scope:

- `FlippedJoin` (`packages/zql/src/ivm/flipped-join.ts`, 505 LOC) — child-driven inner join
- `UnionFanIn` (`packages/zql/src/ivm/union-fan-in.ts`, 296 LOC) — dedup-by-PK + reorder-by-sort merge
- `UnionFanOut` (`packages/zql/src/ivm/union-fan-out.ts`, 57 LOC) — UFI's paired fork
- `FanIn` (`packages/zql/src/ivm/fan-in.ts`, 94 LOC) — simpler filter-graph merge
- `FanOut` (`packages/zql/src/ivm/fan-out.ts`, 83 LOC) — simpler filter-graph fork

Wiring in scope:

- `packages/zqlite-rs/src/ast_to_config.rs` — implement equivalent of TS
  `applyFilterWithFlips` (`builder.ts:376-482`). Currently every CSQ with
  `flip == Some(true)` is silently treated as a regular Exists; this phase
  detects it, builds the FanOut→FanIn-with-FlippedJoin distributive expansion,
  and emits new operator-config variants.
- `packages/zero-ivm-rs/src/pipeline.rs` — add `FlippedJoin`, `UnionFanIn`,
  `UnionFanOut`, `FanIn`, `FanOut` variants to `OperatorConfig` and
  serialization tests.
- `packages/zqlite-rs/src/hydrate.rs::build_operator_chain` — wire the new
  variants into the operator tree.

**Out of scope** for this phase: B5 (OrExists short-circuit), B6 (EXISTS_LIMIT
downgrade), B12 (companion scalar drift), B8/B9 (i64 precision). Those remain
in Phase 35's carry-forward list (or follow-up phases). The planner cost model
itself is also out of scope — we accept whatever `flip:true` flag the planner
emits and translate it; we do not modify cost decisions.

</domain>

<decisions>
## Implementation Decisions

### Operator Port Strategy

- **D-01:** **TS-as-spec discipline.** Every Rust operator function cites a
  specific TS `flipped-join.ts:line`, `union-fan-in.ts:line`, etc. Mirror TS
  semantics exactly. No novel Rust-side behavior. Performance-only deviations
  are explicitly rejected.
- **D-02:** **Generator-to-synchronous translation.** TS uses `function*` +
  `yield 'yield'` + `yield* output.push(...)` to interleave fetches and pushes
  cooperatively. Rust port emits **all output up front** (synchronous return of
  `Vec<Change>`), matching the existing `JoinOperator` and `ExistsOperator`
  pattern in `zero-ivm-rs/src/`. The `'yield'` sentinel is dropped. This is the
  architectural approach already used for every other ported operator (Phase 11
  Join, Phase 12 Exists).
- **D-03:** **Inprogress-child-change overlay** (`flipped-join.ts:158-165,
247-284`) is mirrored as a per-push transient state struct — `InProgressOverlay {
change, position }` field on `FlippedJoinOperator`, populated at the start of
  `push_child` and cleared in a `Drop`-guarded scope. The fetch path checks the
  overlay synchronously, no generator interleaving needed.
- **D-04:** **UnionFanIn dedup uses a streaming priority-queue merge** —
  `BinaryHeap<(Reverse<RowKey>, branch_idx, Node)>`. Dedup-by-primary-key with
  a `last_yielded_pk: Option<Vec<Value>>` cursor. Reorder-by-sort follows from
  the heap ordering using the schema's `compareRows` equivalent.
- **D-05:** **fanOutStartedPushing / fanOutDonePushing protocol** is
  represented as a Rust enum `UnionPushPhase { Idle, InFanOut(ChangeType) }` on
  `UnionFanOutOperator`. Branches accumulate `Vec<Change>` while phase is
  `InFanOut`; `done_pushing()` calls `push_accumulated_changes` analogue then
  flips back to `Idle`.
- **D-06:** **FanIn / FanOut as filter-only operators.** TS treats them as
  `FilterOperator` (no row materialization, just predicate accumulation). Rust
  port emits a `FanIn` / `FanOut` config that drives a thin pass-through with
  branch-OR predicate evaluation — semantics preserved without the
  `beginFilter/endFilter/filter` callback shape (Rust passes filter results in
  the `Vec<Change>` return). Already validated in Phase 11/12.

### AST Translation

- **D-07:** **applyFilterWithFlips equivalent in `ast_to_config.rs`.** The
  current code at lines 401, 456, 589 sees `flip: Option<bool>` and only uses
  it to skip `apply_exists_limit`. Phase 36 introduces a new entry path
  `append_condition_configs_with_flips` that detects any CSQ-with-`flip:true`
  in WHERE and dispatches to the FanOut→FanIn-with-FlippedJoin expansion when
  found, falling back to the existing `append_condition_configs` otherwise.
- **D-08:** **Distributive expansion** (TS `builder.ts:386-449`) — for
  `OR(simple, EXISTS flip=true)` shape, emit:
  ```
  UnionFanOut → [
    Filter(simple branch),                    // withoutFlipped
    FlippedJoin(child=subquery)               // withFlipped
  ] → UnionFanIn
  ```
  For `AND(simple, EXISTS flip=true)`, emit non-flipped branches as a normal
  Filter pipeline first, then chain FlippedJoin onto its output (TS
  `builder.ts:386-408`).
- **D-09:** **Planner emission is the source of truth for `flip:true`.** The
  planner (`packages/zql/src/planner/planner-builder.ts:240+`) sets `flip:true`
  on a CSQ when its cost model judges child-driven evaluation cheaper than
  parent-driven. Phase 36 does NOT modify the planner; we accept whatever
  comes through and execute it correctly.
- **D-10:** **`flip:true` on `NOT EXISTS` is rejected** — the planner already
  refuses (`planner-builder.ts:248`); Rust mirrors this with an `assert!` if
  encountered.

### Test Strategy

- **D-11:** **Port test fixtures, not generator-style scaffolding.** TS tests
  use a `runJoinTest` harness with synthetic `MemorySource` parents/children
  and string snapshots. Rust port writes equivalent unit tests in
  `flipped_join_op.rs::tests` using `MemoryTableSource` and explicit expected
  `Vec<Change>` outputs. Don't try to mass-import the 241KB
  `flipped-join.push.test.ts` mechanically — that will fail because the test
  scaffolding (operator builders, change accumulators) is generator-shaped and
  doesn't translate directly.
- **D-12:** **Coverage targets** (per Wave):
  - FlippedJoin: ≥50 unit tests covering fetch, push-parent (Add/Remove/Edit/
    Child), push-child (Add/Remove/Edit/Child) — parity with TS coverage on
    representative shapes.
  - UnionFanIn: ≥20 tests covering dedup, reorder, internal-vs-external push,
    fanOutStartedPushing/Done protocol, multi-branch with overlapping PKs.
  - UnionFanOut: ≥10 tests covering 2-way and 3-way fork, push-protocol
    handoff, destroy refcount.
  - FanIn / FanOut: ≥10 tests each covering filter-OR semantics + accumulation.
  - AST translation: ≥15 tests in `ast_to_config.rs::tests` covering distributive
    expansion shapes (OR-with-flip, AND-with-flip, nested, NOT-EXISTS-rejected).
- **D-13:** **Differential parity** — un-allowlist `fuzz_00132` and
  `fuzz_00133` from `tools/ivm-parity/parity-allowlist.json`. Add at least 5
  new shaped FlippedJoin tests to `tools/ivm-parity/diff-tests-track2.ts` (or a
  sibling `diff-tests-flipped-join.ts`). Confirm `npm run fuzz-check:gate`
  passes with the smaller allow-list.

### Verification Gate

- **D-14:** Phase 36 exit criteria:
  1. All five new operators have Rust unit tests AND `tools/ivm-parity/`
     differential tests committed.
  2. `cargo test -p zero-ivm-rs` includes ≥110 new tests across
     FlippedJoin/UnionFanIn/UnionFanOut/FanIn/FanOut.
  3. `tools/ivm-parity/parity-allowlist.json` shrinks by exactly 2 entries
     (`fuzz_00132`, `fuzz_00133`); `npm run fuzz-check:gate` exits 0.
  4. `cargo test -p zqlite-rs` adds ≥15 ast_to_config tests for the new
     translation path.
  5. Phase 33 benchmarks PASS (TTFB threshold 1.5×, MemPeak threshold 4×). The
     FlippedJoin path is a NEW path — latency is measured fresh, not compared
     against a no-flip baseline.
  6. TS suite still green (`packages/zql/src/ivm/flipped-join.*.test.ts` is
     unchanged; this is a Rust-side addition).
- **D-15:** **No regression.** Existing `npm run fuzz-check:gate` allow-list
  entries other than the two FlippedJoin shapes remain untouched. Existing
  Phase 35 deferred patterns stay deferred.

### Sizing & Scheduling

- **D-16:** **5 implementation waves + 1 research wave + 1 verification wave**:
  - Wave 0 (research): port mapping tables for FlippedJoin and UnionFanIn (the
    two non-trivial ports). Identifies generator-to-sync translation choices
    per branch. ~1 plan file.
  - Wave 1: FanOut + FanIn (simple variants). ~150 LOC + ~20 tests.
  - Wave 2: UnionFanOut + UnionFanIn (dedup+reorder). ~430 LOC + ~30 tests.
  - Wave 3: FlippedJoin operator. ~600 LOC + ~50 tests.
  - Wave 4: AST translation in `ast_to_config.rs`. ~200 LOC + ~15 tests.
  - Wave 5: Differential parity test removal. Allow-list shrink + new diff
    tests. ~5 tests, code-removal heavy.
  - Wave 6 (verification): full cargo + vitest + fuzz-check + Phase 33 benches.
- **D-17:** Calendar estimate: **3-4 weeks at one engineer-equivalent**.
  Confidence on FlippedJoin push-parent/push-child interleaving is moderate;
  spike may be needed for UnionFanIn dedup if generator translation hits
  unforeseen lookahead requirements (see Risk Register).

### Risk Register

- **R-01 (HIGH, recommended spike):** Generator-to-synchronous translation
  for `UnionFanIn::push` accumulator + `fanOutDonePushing` protocol. TS
  cooperatively yields control back to UFO between branches; Rust must collect
  all branch outputs upfront and run dedup at the end. The naive translation
  works but `pushAccumulatedChanges` (`push-accumulated.ts`) merges per-row
  relationships across branches — this needs careful Rust-side equivalent. If
  Wave 2 estimate slips by >3 days, escalate to a 1-day spike on the
  `pushAccumulatedChanges` semantics before continuing.
- **R-02 (MEDIUM):** Planner cost model emits `flip:true` based on cost — if
  test fixtures don't cover the planner's natural emission patterns, B7 is
  fixed in the operator but unexercised in production. **Mitigation:** Wave 5
  includes at least 3 planner-emission round-trip tests that drive ZQL queries
  through `planner-builder.ts` and assert the resulting AST hits the new Rust
  path.
- **R-03 (LOW):** `compareRows` semantics in UnionFanIn assume schema-equal
  branches — TS asserts this (`union-fan-in.ts:55-71`). Rust port preserves
  the assertion but a runtime mismatch would panic; Wave 2 includes a test
  that asserts the panic on mismatched branch schemas.
- **R-04 (LOW):** Inprogress-child-change overlay (`flipped-join.ts:158-284`)
  has subtle lifetime: the overlay must persist across nested fetches the
  push triggers. Rust port stores it as a struct field with explicit
  `Option::take()` in a guarded scope — same lifetime behavior.

### Process

- **D-18:** TS sources are read-only; this is a Rust-side addition. No
  changes to `packages/zql/src/ivm/` files in this phase.
- **D-19:** Each wave commits independently. Inter-wave dependency:
  Wave 1 → Wave 2 (Fan{In,Out} are dependencies of Union variants in `pipeline.rs`).
  Wave 2 → Wave 3 (FlippedJoin construction is downstream of Union variants).
  Wave 3 → Wave 4 (ast_to_config emits FlippedJoin variants).
  Wave 4 → Wave 5 (allow-list shrink requires the full pipeline working end-to-end).
- **D-20:** No CI integration deferred — Phase 34's `fuzz-check:gate` already
  runs against the smaller allow-list automatically once entries are removed.

</decisions>

<deliverables>
## Deliverables

- New Rust operator files:
  - `packages/zero-ivm-rs/src/flipped_join_op.rs` (~600 LOC)
  - `packages/zero-ivm-rs/src/union_fan_in_op.rs` (~330 LOC)
  - `packages/zero-ivm-rs/src/union_fan_out_op.rs` (~100 LOC)
  - `packages/zero-ivm-rs/src/fan_in_op.rs` (~120 LOC)
  - `packages/zero-ivm-rs/src/fan_out_op.rs` (~80 LOC)
- Modified:
  - `packages/zero-ivm-rs/src/pipeline.rs` — 5 new `OperatorConfig` variants, `build_operator` arms.
  - `packages/zero-ivm-rs/src/lib.rs` — module declarations.
  - `packages/zqlite-rs/src/ast_to_config.rs` — `applyFilterWithFlips` equivalent (~200 LOC).
  - `packages/zqlite-rs/src/hydrate.rs` — `build_operator_chain` arms for new variants.
  - `tools/ivm-parity/parity-allowlist.json` — remove `fuzz_00132` and `fuzz_00133` entries.
  - `tools/ivm-parity/diff-tests-track2.ts` — new FlippedJoin shape tests.
- Documentation:
  - `.planning/IVM-PORT-AUDIT-DEEP.md` — flip B7 status to ✅ FIXED.
  - `PARITY_STATUS.md` — remove `fuzz_00132`/`fuzz_00133` from allow-list table; remove `B7-flipped-join` from deferred patterns.

</deliverables>
