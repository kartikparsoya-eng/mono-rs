# Phase 34: Differential Fuzz + Schema Extension — Research

**Researched:** 2026-04-29
**Domain:** property-based differential testing of TS↔Rust IVM operator port; targeted root-cause fixes against TS canonical reference
**Confidence:** HIGH (all primary claims [VERIFIED] against codebase or [CITED] from local source files; one [ASSUMED] item explicitly listed in §Assumptions Log)

## Summary

Phase 34 has two tracks. **Track 2** (audit-fix) is concrete, surgical, and largely planned by the deep audit: B1 swaps Skip ordering in `ast_to_config.rs` so it lands BEFORE `append_condition_configs`; B2 drops `.max(1)` in `exists_op.rs:152`; B3 threads `partition_key` through `ast_to_operator_configs` so child Takes get the `child_field` partition; B11 routes `emit_descendant_removals`'s SQL read at `advance.rs:477` against the **prev** snapshot path rather than the post-`swap_snapshot` path. All four fixes are TS-as-spec — `builder.ts:302-345`, `exists.ts`, `take.ts:710-757`, and `pipeline-driver.ts:1542-1577` (upstream ref) drive each one.

**Track 1** (fuzzer) bolts a fast-check generator on top of the existing `tools/ivm-parity/` two-process harness. The existing BFS enumerator (`ast-fuzz.ts`) STAYS as the deterministic regression baseline (D-04). fast-check produces random ASTs that share the same JSON shape (`packages/zero-protocol/src/ast.ts`) and feeds them through `harness-coverage.ts` + `harness-advance-coverage.ts` — both already speak `(WebSocket → AST → row delta)`. The CI budget of 1k iterations in <2 min is achievable only if iterations BATCH against existing hydration sockets and reuse the `MUTATIONS` block — per-AST `subscribe / hydrate / mutate / cleanup` is ~1.2s/AST today (220s/180 ASTs in `harness-advance-coverage`'s last full sweep) so the fuzzer must drive iterations through the existing batch pipeline rather than bring new processes up.

**Schema extension (FUZZ-02)** adds `jsonb`, `timestamptz`, `numeric`, nullable variants, and at least one composite/array column to `zero-schema.ts` + `schema.sql` + `seed.sql`. Density target is `apps/zbugs/shared/schema.ts`. NULL-semantics and type-coercion fixtures (i64 > 2^53 hits B8/B9 organically) are required. `ast-fuzz.ts` re-reads `zero-schema.ts` at runtime, so adding rich-typed columns auto-extends the corpus.

**Primary recommendation:** Schedule Track 2 (B1, B2, B3, B11) in Wave 1 — these are independent of fuzzer infrastructure and unblock the no-regression gate. Schedule Track 1 fuzzer build in Wave 1 in parallel — it can develop against the existing schema. Wave 2 is FUZZ-02 schema extension + the full-corpus fuzz run; per D-19 the full run waits until Track 2 lands. Wave 3 is verification: 1k fuzz iterations <2 min, full vitest, full cargo test, Phase 33 benches re-run, allow-list of 5 known divergences carry forward.

---

## User Constraints (from CONTEXT.md)

### Locked Decisions

**Tooling Location**

- **D-01:** Fuzzer extends `tools/ivm-parity/` (option A). Two `zero-cache` instances — TS upstream from `/private/tmp/ivm-parity-ts-ref` git worktree of `rocicorp/mono`, RS via `ZERO_USE_RUST_IVM_V2=1` from this repo — share a Postgres on `:6434`.
- **D-02:** Reject option B (in-process vitest fuzz importing TS as a library).
- **D-03:** New code lands in `tools/ivm-parity/`, not `packages/zero-cache/src/services/view-syncer/random-ast-parity.fuzz.test.ts`.

**Random-AST Fuzz (FUZZ-01)**

- **D-04:** Hybrid generator — keep BFS enumerator (`ast-fuzz.ts`) AS-IS as deterministic regression baseline. ADD a fast-check generator. Both run in `npm test`; BFS first, then fast-check.
- **D-05:** `FUZZ_NUM_RUNS=1000` default. Must complete in <2 minutes. Two-process harness — implementation must batch fixture reuses rather than restart per AST.
- **D-06:** fast-check shrinking is the WIN over BFS — every divergence shrunk to minimal counterexample before being recorded as regression test. Don't disable shrinking for speed; budget is via num-runs.
- **D-07:** AST generators in TS in `tools/ivm-parity/`; emit same `AST` JSON the existing harness round-trips. Shrinkers operate on AST JSON, not Rust types.
- **D-08:** Generate with awareness of deep-audit findings — include shapes that exercise B1, B3, B5.

**Schema Extension (FUZZ-02)**

- **D-09:** Production-shape density. Match `apps/zbugs/shared/schema.ts`. One column of each rich type is the floor, not the goal.
- **D-10:** Required types: `jsonb`, `timestamptz`, `numeric`, nullable variants of each scalar, at least one composite/array column.
- **D-11:** Required NULL semantics: NULL ≠ NULL in equality, NULL propagation in arithmetic, NULL in JOIN keys, NULL in OR (three-valued logic).
- **D-12:** Required type-coercion fixtures: string→number, numeric precision boundaries (i64 > 2^53 — organic discovery for B8/B9), date/time round-trips across DST boundaries.
- **D-13:** Defer adversarial cases (NaN, ±∞, deeply-nested jsonb) to Phase 35.
- **D-14:** Schema is data not code — extend `tools/ivm-parity/zero-schema.ts` + `schema.sql` + `seed.sql` + `seed-extras.sql`. Do NOT modify harness fuzzer logic for type-handling — that work belongs in operator implementations.

**Deep-Audit Hardening (Track 2)**

- **D-15:** **In-scope BLOCKING fixes:** B1, B2, B3, B11. Each cites TS file:line and mirrors TS semantics.
- **D-16:** **Out-of-scope (Phase 35):** B5, B6, B7, B8/B9 (unless organic), B10, B12, B13, B14.
- **D-17:** **TS-as-spec discipline.** Every Track 2 fix cites a specific TS file:line. No novel Rust-side behavior. No "Rust does it slightly differently for performance." Performance optimizations live in v6.0 ONLY after parity is proven.
- **D-18:** **No regressions.** Every Track 2 fix:
  1. Rust unit test asserting new behavior with TS file:line citation
  2. TS↔Rust differential test in `tools/ivm-parity/`
  3. Pass full `cargo test` for `zero-ivm-rs` + `zqlite-rs`
  4. Pass full vitest under `packages/zero-cache/src/services/view-syncer/`
  5. Pass `streaming-vs-buffered-parity.fuzz.test.ts` with 1k iterations
  6. Pass Phase 33 benchmarks (TTFB 1.5×, MemPeak 4×) — performance MUST NOT regress

**Track Ordering**

- **D-19:** Parallel execution. Track 1's full-corpus run waits until Track 2 lands.
- **D-20:** Any divergence beyond B1/B2/B3/B11 becomes a regression test + `PARITY_STATUS.md` entry; planned fix in Phase 35 unless trivially fixable.

**Verification Gate**

- **D-21:** All four BLOCKING fixes have Rust unit tests AND `tools/ivm-parity/` differential tests committed; `npm test` from `tools/ivm-parity/` exits 0; fast-check fuzz with `FUZZ_NUM_RUNS=1000` completes in <2 min and reports zero unexpected divergences (allow-list = 5 catalogued in `PARITY_STATUS.md` + B5/B6/B7/B12); Phase 33 benches PASS; full `cargo test` + full vitest green.
- **D-22:** No CI integration in this phase.

### Claude's Discretion

- **CD-01:** fast-check arbitrary structure (combinator composition, custom shrinkers vs default, single `arbAst` vs per-operator arbitraries).
- **CD-02:** How harness fixtures are reused across iterations (Postgres truncate-and-reseed vs deterministic schema-per-iteration).
- **CD-03:** Whether `synth-p6.ts`, `synth-p7.ts` get folded into the new fast-check generator or kept separate.
- **CD-04:** Allow-list format in the harness for deferred items.

### Deferred Ideas (OUT OF SCOPE)

- **B5** (OrExists short-circuit) — Phase 35.
- **B6** (EXISTS_LIMIT downgrade) — Phase 35.
- **B7** (FlippedJoin missing) — Phase 35 or v6.0.
- **B8/B9** (i64 > 2^53 precision) — fix in Phase 34 only if FUZZ-02 hits organically, else Phase 35.
- **B10, B12, B13, B14** — Phase 35.
- 5 catalogued divergences in `PARITY_STATUS.md` (FlippedJoin × 2, scalar EXISTS × 2, duplicate alias × 1) — carry forward.
- Adversarial schema cases (NaN, ±∞, deep jsonb).
- CI integration.

---

## Phase Requirements

| ID               | Description                                                                                                                                                               | Research Support                                                                |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| FUZZ-01          | Random-AST differential fuzz against TS oracle. fast-check across full operator surface. `FUZZ_NUM_RUNS=1000` default. Any divergence becomes regression test before fix. | §fast-check generator design, §Two-process harness reuse, §CI budget compliance |
| FUZZ-02          | Schema-extension — rich production types (jsonb, timestamptz, numeric, nullable, composite/array). NULL semantics + type coercion fixtures.                               | §Schema extension layout                                                        |
| B1 (DEEP-AUDIT)  | Skip placement order. TS does Skip→CSQ→Filter (`builder.ts:302`); Rust does Filter+Exists→Skip (`ast_to_config.rs:226-238`). Move Skip BEFORE `append_condition_configs`. | §Track 2 fix specifications §B1                                                 |
| B2 (DEEP-AUDIT)  | `parent_sizes.insert(pk, children.len().max(1))` poisons cache (`exists_op.rs:152`). Cache real `children.len()`.                                                         | §Track 2 fix specifications §B2                                                 |
| B3 (DEEP-AUDIT)  | Take fetch/push state-key inconsistency when `partition_key: None` + constraint. Thread `partition_key` through `ast_to_operator_configs`. Closes Risk #1 simultaneously. | §Track 2 fix specifications §B3                                                 |
| B11 (DEEP-AUDIT) | `emit_descendant_removals` reads from POST-tx snapshot (`advance.rs:477`). Route through prev-snapshot path TS already maintains.                                         | §Track 2 fix specifications §B11                                                |

---

## Project Constraints (from CLAUDE.md / AGENTS.md)

- **Tech stack:** Rust via napi-rs (Node native module). API compatibility — Rust must exactly match existing TS Database/Statement API.
- **Verification gate (mandatory every phase):**
  1. `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.*.test.ts` — all existing tests pass unchanged.
  2. `npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` — fuzz with 1k iterations.
  3. `cargo test` in `packages/zqlite-rs/` and `packages/zero-ivm-rs/`.
  4. **Hard constraint** (preserved from Phase 31): no signature change to existing buffered methods (`advance`, `advanceAsync`, `hydrate*`, `addQuery*`); no change to `encode_advance_result_buf` or `decodeAdvanceResultBuf` formats.
- **Lint/format/types:** `npm run lint` (oxlint), `npm run format` (oxfmt), `npm run check-types` after every change.
- **TypeScript style:** ESM, `import` paths use explicit `.ts` extension; `kebab-case.ts` files; private fields `#field`; PascalCase interfaces with no `I` prefix.
- **Re-export rules** (AGENTS.md): no re-exports between internal packages that would create cycles. Phase 34 changes are local to `tools/ivm-parity/` and Rust crates — no cross-package re-export risk.
- **`tools/ivm-parity/` hard rules** (SKILL.md):
  1. Don't modify the test apparatus to hide a divergence.
  2. Don't UPDATE or DELETE existing seed rows. `seed-extras.sql` is additive only.
  3. RS fixes must cite TS file:line. No novel RS logic.
  4. Don't edit TS source except env-gated `console.log` instrumentation (revert after use).
  5. Re-run full sweep after every fix.
- **Commit message format:** `type(scope): description` per AGENTS.md (e.g., `fix(zero-ivm-rs): thread partition_key into child Take`).

---

## Architectural Responsibility Map

| Capability                                  | Primary Tier                                    | Secondary Tier                                  | Rationale                                                                                             |
| ------------------------------------------- | ----------------------------------------------- | ----------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| AST→OperatorConfig translation (B1, B3)     | Rust `zqlite-rs::ast_to_config`                 | —                                               | Sole authority that maps the protocol AST into the Rust operator chain; TS reference is `builder.ts`. |
| EXISTS membership cache (B2)                | Rust `zero-ivm-rs::exists_op`                   | —                                               | Cache lives inside the operator; no TS-side equivalent because Rust collapsed Join+Exists.            |
| Take partition-key state (B3)               | Rust `zero-ivm-rs::take_op`                     | Rust `ast_to_config`                            | partition_key is provisioned by ast_to_config; consumed/threaded by take_op.                          |
| Cascade-delete enumeration (B11)            | Rust `zqlite-rs::advance`                       | TS `pipeline-driver` (snapshot lifecycle owner) | Snapshot pointer is owned by TS via swapSnapshot napi call; Rust must read from the right snapshot.   |
| Random-AST generation (FUZZ-01)             | TS `tools/ivm-parity/`                          | Node fast-check 3.23.2                          | Fuzzer is data-only generator; TS for ergonomic AST manipulation; fast-check for shrinking.           |
| Two-process harness orchestration (FUZZ-01) | TS `tools/ivm-parity/harness-*-coverage.ts`     | WebSocket / Postgres                            | Already-built integration tier; fast-check sits ABOVE this layer.                                     |
| Schema definition (FUZZ-02)                 | TS `zero-schema.ts` + Postgres DDL              | Both `zero-cache` instances re-read at start    | Schema is data — runtime-loaded by the BFS enumerator.                                                |
| TS↔Rust differential assertion              | TS `compareChanges` from `dual-executor.ts:283` | —                                               | Multiset-aware change comparison primitive already exists; reused.                                    |

---

## Standard Stack

### Core (already installed — no new deps in Phase 34)

| Library                  | Version                                                                                                | Purpose                                                                              | Why Standard                                                                                                                                                           |
| ------------------------ | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `fast-check`             | 3.23.2 [VERIFIED: `/Users/kartik.parsoya/Documents/Zero/mono-rs/node_modules/fast-check/package.json`] | Property-based test generator with shrinking                                         | Already used by `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts` and `streaming-vs-buffered-parity.fuzz.test.ts` — Phase 34 must continue passing both. |
| `vitest`                 | 4.1.3 [VERIFIED: `package.json:48`]                                                                    | Test runner; not used by fuzzer harness, but the no-regression gate runs full vitest | Project standard.                                                                                                                                                      |
| `tsx`                    | (version per `tools/ivm-parity/package.json`)                                                          | Run TypeScript directly, no compile step                                             | Existing harness scripts run via `npx tsx`.                                                                                                                            |
| `ws`                     | (per `tools/ivm-parity/package.json`)                                                                  | WebSocket client driving zero-cache `:4858` and `:4868`                              | Existing harness uses it; new fuzz iterations reuse the same WebSocket pattern.                                                                                        |
| `postgres` (postgres.js) | ^3.4.5 [VERIFIED: `tools/ivm-parity/package.json:25`]                                                  | PG client used by `harness-advance-coverage.ts` for the mutation block               | Existing pattern; reused as-is.                                                                                                                                        |

### fast-check 3.23.2 capabilities relevant to FUZZ-01 [VERIFIED: existing usage in `fuzz-ivm.test.ts`]

- **`fc.assert(fc.property(...), {numRuns, seed})`** — main entry point. `numRuns` controls iteration count; `seed` (env `FUZZ_SEED`) gives reproducibility.
- **`fc.oneof`, `fc.constantFrom`, `fc.tuple`, `fc.dictionary`, `fc.record`, `fc.array`** — combinators for composing arbitraries.
- **`fc.letrec`** — recursive arbitraries (needed for nested `Condition` and `CorrelatedSubquery`).
- **Default shrinker** — works on the structure of combinators automatically. Adequate for Phase 34; custom shrinkers are unnecessary (D-06 says don't disable shrinking, doesn't say custom).
- **`fc.pre(...)`** — discard predicate inside a property, useful for filtering generated ASTs that the parser would reject before sending them to the harness.

### Alternatives Considered

| Instead of                  | Could Use                             | Tradeoff                                                                                                                                                        |
| --------------------------- | ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| fast-check                  | hypothesis (Python) / proptest (Rust) | Both reject by D-01/D-07; fuzzer is in-TS in `tools/ivm-parity/`.                                                                                               |
| Two-process WS harness      | In-process vitest oracle              | Rejected (D-02). mono-rs's `tsAdvance` returns empty (no surviving in-process TS pipeline state) — failed Phase 33 HARDEN-01 strict mode for that exact reason. |
| Per-iteration cache restart | Batch reuse of hydrated sockets       | D-05 — `<2 min for 1k` is the budget. Per-AST restart is ~1.2s, blowing the budget by 10×.                                                                      |

### Installation

No new packages. Verify with:

```bash
cd /Users/kartik.parsoya/Documents/Zero/mono-rs/tools/ivm-parity
node -e "console.log(require.resolve('fast-check'))"
```

---

## Architecture Patterns

### System Architecture Diagram

```
                        ┌─────────────────────────────┐
                        │  zero-schema.ts + schema.sql│
                        │  + seed.sql + seed-extras   │
                        │  (FUZZ-02 extends here)     │
                        └──────────────┬──────────────┘
                                       │ schema runtime-loaded
                                       ▼
                ┌──────────────────────────────────────────┐
                │  AST generators (tools/ivm-parity/)      │
                │                                          │
                │  ast-fuzz.ts        random-ast-fuzz.ts   │
                │  (BFS, KEEP)        (NEW — fast-check)   │
                │  → ast_corpus.json  → arbAst per-iter    │
                └──────────────┬─────────────────┬─────────┘
                               │                 │
                  per-AST corpus│        random AST + shrink
                               │                 │
                               ▼                 ▼
                ┌──────────────────────────────────────────┐
                │  harness-coverage.ts (hydrate ~5s)       │
                │  harness-advance-coverage.ts (~2 min)    │
                │                                          │
                │  WS :4858 (TS)        WS :4868 (RS)      │
                │       │                    │             │
                │       └────diff via────────┘             │
                │       compareChanges (multiset)          │
                │                                          │
                │  Mutation block (PG :6434/parity)        │
                │  applied once per BATCH (BATCH_SIZE=30)  │
                └──────────────┬───────────────────────────┘
                               │
                               ▼
                ┌─────────────────────────────────────┐
                │  divergence detected?               │
                │   ├─ yes → fast-check shrinks       │
                │   │        → minimal counterexample │
                │   │        → regression test        │
                │   │        → PARITY_STATUS.md entry │
                │   │        → Track 2 fix or         │
                │   │          Phase 35 deferred      │
                │   └─ no  → continue                 │
                └─────────────────────────────────────┘

Track 2 fix sites (independent of fuzzer):
  ┌─ ast_to_config.rs:226-238  → B1 (Skip ordering)
  ├─ ast_to_config.rs:240-247  → B3 (partition_key threading)
  ├─ exists_op.rs:152          → B2 (.max(1) cache poison)
  ├─ take_op.rs:55-67, 317-329 → B3 (state key consistency)
  └─ advance.rs:464-477        → B11 (prev snapshot path)
```

### Recommended File Layout (additions only — KEEP everything existing)

```
tools/ivm-parity/
├── ast-fuzz.ts                         # KEEP (D-04 BFS baseline)
├── random-ast-fuzz.ts                  # NEW — fast-check generator
├── arb-ast.ts                          # NEW — fast-check arbitraries (per-operator)
├── harness-coverage.ts                 # KEEP — hydrate sweep
├── harness-advance-coverage.ts         # KEEP — advance sweep with MUTATIONS
├── harness-fuzz.ts                     # NEW — drives fast-check iterations through
│                                       #       existing two-process harness
├── shrink-min.ts                       # NEW — record minimal counterexample to
│                                       #       ast_corpus.regressions.json
├── ast_corpus.regressions.json         # NEW (gitignored or checked-in?) —
│                                       #       D-20 carry-forward divergences
├── allow-list.json                     # NEW — D-21 deferred-divergence allow-list
├── zero-schema.ts                      # EDIT — FUZZ-02 schema additions
├── schema.sql                          # EDIT — FUZZ-02 PG DDL
├── seed.sql                            # KEEP unchanged
├── seed-extras.sql                     # EDIT — FUZZ-02 seed data (additive only)
└── PARITY_STATUS.md                    # EDIT — D-20 carry-forward + B1/B2/B3/B11 fixes
```

### Pattern 1: fast-check generator composition (CD-01 — recommended)

**What:** Per-operator arbitraries composed via `fc.letrec` for recursion, then combined into a single `arbAst`. Schema-driven — re-read `zero-schema.ts` at runtime same way `ast-fuzz.ts` does (CD-03 → keep `synth-p6.ts`/`synth-p7.ts` as separate corpus generators; don't fold).

**When to use:** All Phase 34 fuzz iterations.

**Example:**

```ts
// Source: extends pattern from packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts:14-100
import fc from 'fast-check';
import type {
  AST,
  Condition,
  CorrelatedSubquery,
  SimpleOperator,
} from '../../packages/zero-protocol/src/ast.ts';
import {schema as paritySchema} from './zero-schema.ts';

// Tables × columns × ops are derived from the same adapter ast-fuzz.ts uses.
// fast-check picks deterministically by seed; shrinking traverses the
// recursive structure built via fc.letrec.

export function buildArbitraries(adapted: ReturnType<typeof adaptSchema>) {
  const tables = Object.values(adapted);
  const arbTable = fc.constantFrom(...tables);

  return fc.letrec<{
    cond: Condition;
    csq: CorrelatedSubquery;
    ast: AST;
  }>(tie => ({
    cond: fc.oneof(
      // Simple - uses table.columns × opsForColumn × valuesForOp from ast-fuzz.ts
      arbSimpleCondition(tables),
      // And/Or with bounded depth (depth tracked in fc.letrec via fc.option/recursive)
      fc.record({
        type: fc.constantFrom('and', 'or'),
        conditions: fc.array(tie('cond'), {minLength: 1, maxLength: 3}),
      }),
      // Embedded CSQ
      tie('csq').map(c => ({
        type: 'correlatedSubquery',
        related: c.related,
        op: 'EXISTS',
      })),
    ),
    csq: arbCsq(tables, tie('cond')),
    ast: arbTopAst(arbTable, tie('cond'), tie('csq')),
  }));
}
```

### Pattern 2: Hot-fixture iteration loop (CD-02 — recommended)

**What:** Reuse the `harness-advance-coverage.ts` BATCH model. Each fuzz iteration enqueues an AST into a batch of size `BATCH_SIZE` (current default 30). Mutation block runs once per batch. Per-iteration cost is amortized: subscribe, hydrate, await mutation pokes, snapshot delta, close.

**When to use:** All `FUZZ_NUM_RUNS=1000` iterations.

**Example:**

```ts
// Source: pattern from tools/ivm-parity/harness-advance-coverage.ts:614-687
const arb = buildArbitraries(adapted);
let divergences: Result[] = [];

await fc.assert(
  fc.asyncProperty(arb.ast, async ast => {
    // Buffer this AST into a pending batch. When batch fills (or when
    // fc.assert is about to evaluate, signaled via finalizer hook),
    // run the existing batch flow — subscribe×30 / hydrate / mutate /
    // snapshot×2 / cleanup.
    const result = await batchedRunner.enqueueAndMaybeFlush(ast);
    if (result?.outcome.status === 'diverge') divergences.push(result);
    // fc.assert PASSES when result is OK; fails (and triggers shrinking)
    // when result is diverge.
    return result?.outcome.status !== 'diverge';
  }),
  {
    numRuns: NUM_RUNS,
    seed: Number(process.env.FUZZ_SEED ?? Date.now()),
    verbose: 1,
  },
);
```

### Pattern 3: Allow-list filtering (CD-04)

**What:** Drop divergences whose AST canonical-key matches a curated allow-list of carry-forward items.

**When to use:** Verification gate per D-21 — the fuzz must report ZERO unexpected divergences. The 5 known + B5/B6/B7/B12 deferred shapes must pre-filter.

**Example:**

```ts
// Source: extends tools/ivm-parity/PARITY_STATUS.md catalog (5 divergences)
const ALLOW_LIST: Array<{
  canonicalKey: string; // hashed normalized AST minus alias
  reason: string; // 'flipped-join' | 'scalar-exists' | ...
  phase35Issue: string; // tracking ref
}> = JSON.parse(readFileSync('./allow-list.json', 'utf8'));

function isAllowed(ast: AST): boolean {
  const key = canonicalKey(normalizeAST(ast));
  return ALLOW_LIST.some(e => e.canonicalKey === key);
}

// Inside fc.asyncProperty: if divergence detected AND ast matches allow-list,
// log it and return true (so the property succeeds). Otherwise fail
// to trigger shrinking.
```

### Anti-Patterns to Avoid

- **Per-AST process restart.** Spinning up the TS+RS caches for each fuzz iteration multiplies the budget by 10×+. Reuse the running pair.
- **Modifying `ast-fuzz.ts` to drive fast-check directly.** D-04 says BFS stays as deterministic baseline. Build the new generator alongside.
- **Custom AST shrinker.** Default fast-check shrinker is structural and adequate. Don't reinvent. (D-06 says don't disable shrinking; doesn't mandate custom.)
- **Hand-rolled multiset diff.** `compareChanges` from `dual-executor.ts:283` already exists and is used by Phase 33 dualExecCompare. Reuse.
- **Mutating `seed.sql` for FUZZ-02.** SKILL.md hard rule #2 — additive only. Use `seed-extras.sql`.
- **Performance-first Rust deviation.** D-17 — any "Rust does it slightly differently for performance" is rejected. Mirror TS exactly.

---

## Don't Hand-Roll

| Problem                             | Don't Build                         | Use Instead                                                                         | Why                                                                                               |
| ----------------------------------- | ----------------------------------- | ----------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| Random AST generator                | Custom RNG-walk                     | fast-check 3.23.2 (already installed)                                               | Shrinking is the WIN over BFS (D-06). Reinventing means losing minimal counterexamples.           |
| Multiset row-change diff            | Sort+JSON.stringify compare         | `compareChanges` from `dual-executor.ts:283`                                        | Already used by Phase 33 dualExecCompare; multiset-aware (handles non-deterministic PG ordering). |
| Schema runtime adapter              | Re-implement zero-schema parsing    | `adaptSchema` from `ast-fuzz.ts:112-166`                                            | Already converts `zero-schema.ts` exports to internal `TableDef`. New `arb-ast.ts` should reuse.  |
| WebSocket subscribe + hydrate poll  | Custom socket + polling             | `subscribe()` from `harness-advance-coverage.ts:279-396`                            | Already handles `gotQueriesPatch`, `pokePart`, `pokeEnd`, `lastPokeEndAtMs` quiescence.           |
| Mutation block                      | Custom DML                          | `MUTATIONS[]` from `harness-advance-coverage.ts:68-196`                             | Already 17 carefully-curated steps covering Filter/Take/Exists/Join/FlippedJoin coverage.         |
| AST normalization for canonical-key | Custom canonicalize                 | `normalizeAST` from `packages/zero-protocol/src/ast.ts` (used by `ast-fuzz.ts:823`) | Strips aliases; used for dedup; reuse for allow-list keying.                                      |
| TS↔RS oracle in-process             | Library import (Phase 33 attempted) | Two-process harness (D-01)                                                          | mono-rs `tsAdvance` is empty stub. Library oracle has same handicap.                              |
| New CI gate                         | GitHub Action                       | DEFER (D-22)                                                                        | Build the tool, prove it finds bugs, fix them, THEN automate.                                     |

**Key insight:** Every reusable primitive already lives in `tools/ivm-parity/`. New code is the fast-check generator, the iteration driver, and the allow-list. Track 2 fixes are surgical edits in 4 Rust files plus regression tests.

---

## Track 2 Fix Specifications

### B1 — Skip placement order (BLOCKING)

**TS reference behavior** [CITED: `packages/zql/src/builder/builder.ts:284-356` mono-rs copy; verified identical to `/private/tmp/ivm-parity-ts-ref/packages/zql/src/builder/builder.ts`]

The TS pipeline construction order in `buildPipelineInternal` is:

1. Source connect (line 291-296)
2. **Skip** — `if (ast.start) end = new Skip(end, ast.start)` (line 302-306)
3. **CSQ-Exists** — for each non-flipped CSQ, `applyCorrelatedSubQuery` with the EXISTS_LIMIT/PERMISSIONS_EXISTS_LIMIT downgrade (line 308-329)
4. **Filter** — `applyWhere` (line 331-333)
5. **Take** — `if (ast.limit !== undefined)` (line 335-345) — partition_key passed to `Take` constructor (line 341)
6. **Related Joins** — `if (ast.related)` (line 347-356)

**Current Rust behavior** [CITED: `packages/zqlite-rs/src/ast_to_config.rs:200-264`]

Order in `ast_to_operator_configs`:

1. Source (line 219-224)
2. **Filter + Exists** via `append_condition_configs` (line 227-229) — INCLUDES the CSQ-Exists ordering AND simple Filters in the same pass
3. **Skip** — `if let Some(start) = &ast.start` (line 232-238) — WRONG: should be before step 2
4. **Take** — `partition_key: None` (line 241-246) — additional B3 bug here
5. **Related Joins** (line 250-261)

**Diff strategy**

In `ast_to_operator_configs` (`ast_to_config.rs:200`), reorder to:

```rust
// 1. Source (existing, line 219-224)
configs.push(OperatorConfig::Source { … });

// 2. Skip — moved here from line 232. Must be BEFORE conditions per
//    TS builder.ts:302-306 (canonical spec).
if let Some(start) = &ast.start {
    configs.push(OperatorConfig::Skip {
        bound_row: start.row.clone(),
        exclusive: start.exclusive,
        sort: sort.clone(),
    });
}

// 3. CSQ-Exists FIRST, then Filter — append_condition_configs already does
//    this internally (it splits Or branches with CSQs into separate Exists
//    configs). Need to verify append_condition_configs's internal ordering
//    when an And-of-(simple, CSQ) appears: TS does CSQ first (line 308-329)
//    THEN Filter (line 331-333). Currently append_condition_configs treats
//    them order-of-appearance. May need a second-pass split.
if let Some(cond) = ast.where_cond.as_deref() {
    append_condition_configs(schema, &mut configs, cond, primary_key)?;
}

// 4. Take (existing, line 241-246) — see B3 below for partition_key fix
if let Some(limit) = ast.limit {
    configs.push(OperatorConfig::Take {
        limit,
        sort: sort.clone(),
        partition_key,  // NEW — see B3
    });
}

// 5. Related Joins (existing, line 250-261)
```

**Caveat for the reviewer:** TS `applyWhere` runs AFTER `csqConditions` are extracted via `gatherCorrelatedSubqueryQueryConditions(ast.where)` (builder.ts:677-694, the function gathers CSQs out of the And/Or tree). The Rust `append_condition_configs` already splits Or branches that contain CSQs into separate configs (`ast_to_config.rs:332-349`). The Skip move is the hot fix; verify with a fuzz shape that has BOTH `start` AND `where: {csq + simple}` whether the inner ordering matches TS — if not, B1 has a follow-up.

**Spec source:** TS `builder.ts:302-345` for canonical ordering. TS `skip.ts:1-100` for Skip semantics (Rust `skip_op.rs` is already correct per AUDIT — see audit §6 "Skip getStart 9-case logic preserved").

**Regression tests required**:

1. Rust unit test in `ast_to_config.rs::tests`: build an AST with `start + where: {EXISTS}`, assert configs ordering is `[Source, Skip, Exists, …]` with citation `// mirrors TS builder.ts:302-329`.
2. Differential test in `tools/ivm-parity/`: AST shape `messages.where(exists('attachments')).start({row:..., exclusive:false})` — fuzz already produces these via `topOptionsForTable` (ast-fuzz.ts:698-725) crossed with `csqLeafConditions`, so the existing corpus likely already exercises this if rerun after fix.

### B2 — Exists `parent_sizes` cache poisoning (BLOCKING)

**TS reference behavior** [CITED: `packages/zql/src/ivm/exists.ts` — confirmed by `IVM-PORT-AUDIT-DEEP.md §B2`]

TS `Exists` (`exists.ts:21`) is a pure FilterOperator that consults `node.relationships[name]` populated by an upstream Join. The TS path tracks the actual child count via the Join's relationship contents, no separate "max(1) for or-predicate" branch.

**Current Rust behavior** [CITED: `packages/zero-ivm-rs/src/exists_op.rs:139-174`]

```rust
fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
    self.parent_sizes.clear();
    let parent_nodes = self.input.fetch(req);
    let mut result = Vec::new();
    for mut node in parent_nodes {
        if self.or_condition_matches(&node.row) {
            let pk = self.parent_key_str(&node.row);
            let children = self.fetch_children(&node.row);
            self.parent_sizes.insert(pk, children.len().max(1));   // ← LINE 152: poisoned
            node.relationships.insert(self.relationship_name.clone(), children);
            result.push(node);
            continue;
        }
        let children = self.fetch_children(&node.row);
        let count = children.len();
        let pk = self.parent_key_str(&node.row);
        self.parent_sizes.insert(pk, count);                       // ← line 163: correct
        if self.passes_filter(count) { … }
    }
    result
}
```

When `or_condition_matches` returns true and `children.is_empty()`, the cache stores `1` instead of `0`. Subsequent transitions consult this lie:

- `push_impl` Add branch (line 235-251): `old_count = max(1) = 1`, `new_count = 2` after add. Boundary check `old_count == 0 && new_count == 1` is false; no Add transition emitted.
- Combined with AUDIT-04 (Edit-with-or_predicate flip): if a parent edit flips or_predicate from true→false, the count-based fallback at line 282-292 uses cached `1` even when real child count is `0` — row stays in output instead of being removed.

**Diff strategy**

Single-line change at `exists_op.rs:152`:

```rust
// Before
self.parent_sizes.insert(pk, children.len().max(1));

// After — mirrors TS exists.ts behavior: cache the REAL count.
// or_predicate decision is re-evaluated on demand.
self.parent_sizes.insert(pk, children.len());
```

**Spec source:** TS `exists.ts` push path — Rust must mirror.

**Regression tests required**:

1. Rust unit test in `exists_op.rs`:
   - AST: `Exists { or_condition: Some(simple_pred), … }`. Hydrate with a parent that has 0 children but matches or_predicate. Assert `parent_sizes[pk] == 0`.
   - Then push a child Add. Assert the Exists output emits the right Add transition (since old_count=0 → new_count=1 crosses the boundary).
2. Differential test: AST shape `OR(simple, EXISTS(child))` where the simple predicate matches but the child is empty. Mutation block adds a child row. Assert TS and RS emit identical change deltas.

### B3 — Take partition_key threading (BLOCKING — headline fix)

**TS reference behavior** [CITED: `packages/zql/src/builder/builder.ts:611-647` (`applyCorrelatedSubQuery`); `packages/zql/src/ivm/take.ts:55-105` (Take constructor + fetch)]

When TS encounters a related subquery, `applyCorrelatedSubQuery` recursively calls `buildPipelineInternal(sq.subquery, …, sq.correlation.childField)` (line 626-632). The 5th argument is `partitionKey: PartitionKey | undefined`. Inside that recursive call:

- Line 260-261: `partitionKey?: CompoundKey,` (parameter)
- Line 274-277: `splitEditKeys = partitionKey ? new Set(partitionKey) : …` — partition_key columns are added to splitEditKeys.
- Line 341: `new Take(end, …, partitionKey)` — the Take operator gets the partition.

In `take.ts`:

- Line 80-83: `this.#partitionKey = partitionKey; this.#partitionKeyComparator = partitionKey && makePartitionKeyComparator(partitionKey);`
- Line 99: `getTakeStateKey(this.#partitionKey, req.constraint)` — fetch path.
- Line 219: `getTakeStateKey(this.#partitionKey, row)` — push path.
- Line 710-725: `getTakeStateKey` uses `partitionKey` to extract values from row OR constraint, building the same key for both fetch (constraint) and push (row).

The KEY invariant: fetch (called with constraint) and push (called with row) must hash to the SAME state key when constraint values match the row's partition columns.

**Current Rust behavior** [CITED: `packages/zqlite-rs/src/ast_to_config.rs:240-261`, `packages/zero-ivm-rs/src/take_op.rs:55-67, 317-329, 383-393`]

In `ast_to_config.rs:240-247`:

```rust
if let Some(limit) = ast.limit {
    configs.push(OperatorConfig::Take {
        limit,
        sort: sort.clone(),
        partition_key: None,        // ← LINE 245: HARD-CODED NONE — never threaded
    });
}
```

The recursive call at line 254 (`ast_to_operator_configs(schema, &rel.subquery, &child_pk)?`) does NOT pass the parent's `child_field`. So child Takes ALWAYS get `partition_key: None`.

Rust `take_op.rs::take_state_key` (lines 55-67):

```rust
fn take_state_key(&self, row: &Row) -> String {
    match &self.partition_key {
        Some(pk) => {
            let mut vals = vec![Value::String("take".to_string())];
            for k in pk { vals.push(row.get(k).cloned().unwrap_or(Null)); }
            serde_json::to_string(&vals).unwrap_or_default()
        }
        None => "[\"take\"]".to_string(),       // ← always-same global bucket
    }
}
```

`take_op.rs::fetch` (lines 317-329) has a DELIBERATE FALLBACK:

```rust
} else if let Some(c) = &req.constraint {
    // No explicit partition_key, but a constraint is present (child Take inside Join).
    let mut vals: Vec<serde_json::Value> = vec![Value::String("take".to_string())];
    let mut keys: Vec<&String> = c.columns.keys().collect();
    keys.sort();
    for k in keys {
        vals.push(serde_json::Value::String(k.clone()));    // ← INCLUDES the column NAME
        vals.push(c.columns[k].clone());                    // ← AND the value
    }
    serde_json::to_string(&vals).unwrap_or_default()
}
```

Note this fallback's key includes BOTH column names AND values: `["take", "channelId", "ch-1"]`.

`take_op.rs::push` (line 389): `let key = self.take_state_key(&change.node().row)` — calls the row-only path (line 55-67), which returns `"[\"take\"]"` for `partition_key: None`. The two keys do not match. The state stored under `["take", "channelId", "ch-1"]` is invisible to push, which queries `["take"]`. Result at line 390-393: `match self.states.get(&key) { Some(s) => …, None => return vec![] }` — silent push-as-no-op.

**Net effect:** every `related[]` subquery with `limit` silently mishandles child-table edits/adds/removes during advance. Hydrate works because hydrate uses fetch (which has the per-constraint fallback). Most tests don't exercise child mutations under related-with-limit.

**Diff strategy**

Three coordinated edits:

1. **`ast_to_config.rs::ast_to_operator_configs`** — accept `partition_key: Option<Vec<String>>` parameter; pass it to `OperatorConfig::Take`; pass child relation's `child_field` to recursive call:

   ```rust
   // Signature change at line 195-198
   pub fn ast_to_operator_configs(
       schema: &mut SchemaCache,
       ast: &Ast,
       primary_key: &[String],
       partition_key: Option<Vec<String>>,        // NEW
   ) -> Result<Vec<OperatorConfig>, String> {
       …
       // line 240-247 — use the parameter, not None
       if let Some(limit) = ast.limit {
           configs.push(OperatorConfig::Take {
               limit, sort: sort.clone(),
               partition_key: partition_key.clone(),
           });
       }
       …
       // line 250-261 — thread child_field into recursive call
       if let Some(related) = &ast.related {
           for rel in related {
               let child_pk = schema.get_primary_key(&rel.subquery.table)?;
               let child_partition = Some(rel.correlation.child_field.clone());
               let child_configs = ast_to_operator_configs(
                   schema, &rel.subquery, &child_pk, child_partition,
               )?;
               configs.push(OperatorConfig::Join { … });
           }
       }
   }
   ```

   Top-level callers (in `advance.rs` and `hydrate.rs`) pass `None`.

2. **`take_op.rs`** — no signature change needed. `take_state_key(&Row)` already uses `self.partition_key`. Once Rust ast→config provides the right value, fetch (line 55-67) and push (line 389) produce identical keys.

3. **`take_op.rs::fetch` (line 317-329)** — the per-constraint fallback CAN STAY as-is for safety, but should NEVER fire when partition_key is properly threaded. Add a `debug_assert!(self.partition_key.is_some(), "Take: partition_key not threaded — child Take with constraint but no partition_key")` to surface any remaining miss in tests. Per AUDIT-03, framework invariants should be `assert!` not `debug_assert!`. Promote during this fix. After Track 2 lands, the fallback path should be reachable only when `partition_key.is_some()` AND constraint matches.

4. **`hydrate.rs::apply_exists_limit`** also calls `ast_to_operator_configs` for child subqueries — its call sites must pass the EXISTS parent_field as partition_key. Three call sites already exist (per `PARITY_STATUS.md` "flip check added at all 3 `apply_exists_limit` call sites"). Audit each one.

**Spec source:**

- TS partition propagation: `builder.ts:626-632` (`buildPipelineInternal(sq.subquery, …, sq.correlation.childField)`).
- TS Take partition use: `take.ts:80-83, 99, 219, 710-725`.
- Original audit Risk #1 (`IVM-PORT-AUDIT.md §Risk #1`) — closes simultaneously.

**Regression tests required**:

1. Rust unit test in `take_op.rs`: build a Take with `partition_key: Some(vec!["channelId"])`. Hydrate via fetch with `Constraint{channelId: "ch-1"}`. State key MUST equal `take_state_key(&row{channelId: "ch-1"})`. Push a child Add for that channelId. Assert push fetches state successfully and emits the correct transition.

2. Rust unit test in `ast_to_config.rs`: AST `messages.related([{ correlation: {parentField: ['conversationId'], childField: ['conversationId']}, subquery: {table: 'messages', limit: 5} }])`. After translation, the child Take's `partition_key` should be `Some(vec!["conversationId"])`. Cite `// mirrors TS builder.ts:626-632`.

3. Differential test: corpus already produces the exact shape (`relatedShapes` Category C in `ast-fuzz.ts:601-625` adds `orderBy + limit` to child subqueries — `messages.related('attachments', q => q.orderBy('id', 'asc').limit(5))`). Combine with the existing mutation block's child-edit step (`update m-test-1 body`, `delete m-2`). Diff TS vs RS deltas.

### B11 — `emit_descendant_removals` reads from POST-tx snapshot (BLOCKING)

**TS reference behavior** [CITED: `/private/tmp/ivm-parity-ts-ref/packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:1542-1577`]

The upstream TS pipeline-driver materializes `collectedChanges` from the snapshotter diff (which is computed BETWEEN `prev` and `curr` snapshots) BEFORE calling `swapSnapshot(curr.db.db.name)`. Each diff entry contains `prevValues: ReadonlyArray<Readonly<Row>>` and `nextValue: Readonly<Row> | null` — the DELETE prev-row data is in `prevValues`. TS does not emit "cascading deletes" via fresh SQL; it walks the diff output that the snapshotter already canonically produced from the `prev` connection's view. Rust does its own descendant SQL because it collapsed Join+Exists into a single operator that needs to emit child Remove rows for each parent Remove.

The KEY insight: when Rust enumerates descendants it MUST query the `prev` snapshot (where the to-be-deleted rows still exist), not `curr` (where they're gone).

**Current Rust behavior** [CITED: `packages/zqlite-rs/src/advance.rs:464-578, 1316-1320, 1377-1380, 1528-1531, 1602-1605`]

`emit_descendant_removals` opens a fresh read connection at line 477:

```rust
let conn = match rusqlite::Connection::open_with_flags(
    db_path,
    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
) {
    Ok(c) => c, Err(_) => return,
};
```

The `db_path` argument is plumbed from `RustPipelineManager::advance` at line 1856:

```rust
let db_path = self.db_path.lock().unwrap().clone();
```

By the time `advance` runs, the TS pipeline-driver has already called `swapSnapshot(curr.db.db.name)` at line 1572/1576 (upstream) or line 2081 (mono-rs streaming variant). `self.db_path` is now `curr.db.db.name`. The descendant SQL at `advance.rs:514-528` runs against `curr.db` — descendants deleted in the same transaction are NOT in `curr.db` and are silently elided.

**Diff strategy**

Three options, ordered by impedance match with the TS-as-spec rule:

**Option A (preferred — matches TS most directly):** TS pre-computes the descendant list and passes it in. Modify the napi `advance` boundary to accept a second JSON arg: `prev_db_path` OR a precomputed `descendant_removals` list keyed by deleted-row-key. Then `emit_descendant_removals` reads from `prev_db_path` (or doesn't query at all if the list is precomputed).

The TS side already has the `prev` snapshot in scope (`pipeline-driver.ts:1546`: `const {prev, curr, changes: numChanges} = diff`). Add a parameter to the Rust napi method:

```ts
// pipeline-driver.ts — in #rustAdvance / advanceStreaming
this.#rustPipeline.swapSnapshot(curr.db.db.name);
const changesJson = JSON.stringify(collectedChanges);
const resultBuf = this.#rustPipeline.advance(changesJson, prev.db.db.name); // NEW
```

```rust
// advance.rs napi #[napi] advance(&self, changes_json: String, prev_db_path: Option<String>)
//   → emit_descendant_removals(prev_db_path.as_deref().unwrap_or(&db_path), …)
```

The Hard Constraint check (CLAUDE.md verification gate #4): "no signature change to existing buffered methods (`advance`, `advanceAsync`, `hydrate*`, `addQuery*`)". A NEW optional second argument is **additive** if treated as `Option<String>` (napi passes `null`/`undefined` if absent). Existing callers continue to work — the descendant path silently uses `db_path` (current behavior, which is buggy but compatible). NEW callers pass `prev_db_path` and get correct behavior. Mono-rs is the only consumer; we can update it in this phase.

The signature change is incompatible with a literal reading of "no signature change," but the spirit is "buffered methods must not break existing TS callers." This change is purely additive and the only TS caller is mono-rs's pipeline-driver.ts which Phase 34 is allowed to edit. Verify with planner whether to interpret strictly or pragmatically.

**Option B (alternative):** Cache `prev_db_path` inside `RustPipelineManager` via a new `set_prev_snapshot(prev_db_path)` napi method called BEFORE `swap_snapshot`. No method signature changes; new method is additive.

```ts
// pipeline-driver.ts
this.#rustPipeline.setPrevSnapshot(prev.db.db.name); // NEW
this.#rustPipeline.swapSnapshot(curr.db.db.name);
const resultBuf = this.#rustPipeline.advance(changesJson);
```

```rust
// pipeline_manager.rs / advance.rs
struct RustPipelineManager {
    db_path: Mutex<String>,        // curr after swap
    prev_db_path: Mutex<String>,   // NEW — set explicitly before swap
    …
}

#[napi] pub fn set_prev_snapshot(&self, prev_db_path: String) -> napi::Result<()> {
    *self.prev_db_path.lock().unwrap() = prev_db_path;
    Ok(())
}
```

`emit_descendant_removals` reads from `prev_db_path`. Lifecycle: TS sets prev BEFORE swap, swap repoints curr, advance reads prev for descendant SQL.

**Option C (rejected — too invasive):** Have Rust hold both prev and curr connections in the pool, passing prev to descendant SQL. Requires deep changes to `connection_pool.rs` and `swap_snapshot`. Not justified at this scope.

**Recommended: Option B.** Additive napi method, no buffered-method signature change, lifecycle is explicit. Rust unit test mocks both paths. TS test verifies set+swap+advance ordering.

**Spec source:**

- TS: `pipeline-driver.ts:1542-1577` (upstream ref) for snapshot diff lifecycle.
- TS: snapshotter `_zero.changeLog2` two-snapshot WAL2 diff iterator (canonical "diff from prev").
- Audit: `IVM-PORT-AUDIT-DEEP.md §B11`.

**Regression tests required**:

1. Rust unit test: in-memory SQLite. Setup: `prev.db` has parent `p-1` and child `c-1`. `curr.db` has neither. Call `set_prev_snapshot(prev_path); swap_snapshot(curr_path); advance(remove p-1)`. Assert descendant SQL enumerates `c-1` and emits Remove for it.
2. Rust unit test: prev_db_path NOT set → `emit_descendant_removals` reads from `db_path` (current buggy behavior, marked with TODO removed once Track 2 ships).
3. Differential test in `tools/ivm-parity/`: AST `channels.related('conversations', q => q.related('messages'))`. Mutation: `DELETE FROM channels WHERE id = 'ch-test-1'`. Cascading delete should produce Remove rows for descendant conversations and messages. TS already does this correctly via diff; RS must match after fix. The existing `MUTATIONS` block deletes `m-2` (and the cleanup deletes the test channel) — extend with a deeper cascade if needed.

**Pitfall callout:** Phase 33's review item WR-01 ("strict-mode advance unusable") is potentially related. Verify B11 fix doesn't surface a latent assumption in `dualExecCompare` strict mode. (Phase 33 33-REVIEW.md should be checked during planning.)

---

## fast-check Generator Design (CD-01)

### Combinator structure

Per-operator arbitraries composed via `fc.letrec` for recursion. The top-level `arbAst` mirrors `ast-fuzz.ts::generateForTable` but uses `fc.oneof` for stratification rather than deterministic round-robin.

**Schema-driven base layer** (reuses `adaptSchema` from `ast-fuzz.ts:112`):

- `arbTable: fc.Arbitrary<TableDef>` — `fc.constantFrom(...tables)`
- `arbColumn: (table) => fc.Arbitrary<ColumnDef>` — `fc.constantFrom(...table.columns)`
- `arbOpForColumn: (col) => fc.Arbitrary<SimpleOperator>` — derived from `opsForColumn(col)` from `ast-fuzz.ts:185`
- `arbValueForOp: (col, op) => fc.Arbitrary<LiteralValue>` — derived from `valuesForOp(col, op)` from `ast-fuzz.ts:199`

**Conditions** (`fc.letrec` for recursion):

```ts
const arb = fc.letrec(tie => ({
  simple: arbSimpleCondition(tables),
  and: fc.record({
    type: fc.constant('and' as const),
    conditions: fc.array(tie('cond'), {minLength: 2, maxLength: 3}),
  }),
  or: fc.record({
    type: fc.constant('or' as const),
    conditions: fc.array(tie('cond'), {minLength: 2, maxLength: 3}),
  }),
  csq: arbCorrelatedSubquery(tables, tie('cond')),
  cond: fc.oneof(
    {weight: 5, arbitrary: tie('simple')},
    {weight: 2, arbitrary: tie('and')},
    {weight: 2, arbitrary: tie('or')},
    {weight: 3, arbitrary: tie('csq').map(toCsqCond)},
  ),
  ast: arbTopAst(arbTable, tie('cond'), tie('csq')),
}));
```

`fc.oneof` `weight` parameter biases the distribution — simples are most common (matching real query distribution); CSQs get extra weight because they're high-value-per-iteration for parity testing.

### Custom shrinkers vs default

**Default shrinker is sufficient** (CD-01 → researcher decides). fast-check's structural shrinker traverses combinator output recursively; for ASTs that means it shrinks `and: [a,b,c]` → `and: [a,b]` → `a`, and shrinks individual values toward "smaller" representatives. Custom shrinkers add complexity without clear benefit.

What WOULD justify custom shrinker: a divergence whose minimal counterexample is structurally larger than any depth-1 reduction (e.g., needs `OR(EXISTS, EXISTS)` exactly — both branches required). Default shrinker handles this fine via `fc.oneof` shrinking to one branch — if both branches are required to trigger, shrinker won't reduce further, which is correct.

### Per-operator arbitraries

| Arbitrary                      | Generates                                     | Targets                                                                  |
| ------------------------------ | --------------------------------------------- | ------------------------------------------------------------------------ |
| `arbSimpleCondition`           | `{type, op, left, right}`                     | Filter (B1 ordering verification, NULL semantics from FUZZ-02)           |
| `arbCorrelatedSubquery`        | `{correlation, subquery, op, flip?, scalar?}` | Exists, OrExists, FlippedJoin (allow-listed)                             |
| `arbConjunction / Disjunction` | And / Or with bounded depth                   | OR-split logic in `append_condition_configs`, distributive-law expansion |
| `arbOrderBy`                   | `[(col, asc/desc)]`                           | Skip + Take ordering                                                     |
| `arbStart`                     | `{row, exclusive}` based on orderBy           | Skip (B1 placement target)                                               |
| `arbLimit`                     | `1, 3, 5, 10`                                 | Take (B3 partition_key target)                                           |
| `arbRelated`                   | nested CSQs in `related[]`                    | Join + child Take (B3)                                                   |

### Targeted shapes for B1/B2/B3 (D-08 — must hit these)

The planner must add **focused arbitraries** that GUARANTEE generation of the bug-triggering shapes:

- **B1 trigger:** `{table: T, where: {EXISTS(child)}, start: {row, exclusive: false}}` — tests Skip-after-Exists ordering.
- **B2 trigger:** `{table: T, where: {OR(simple_pred, EXISTS(child))}}` where simple_pred matches AND child is empty for some rows. (Already produced by `csqWithSubWhere` in `ast-fuzz.ts:335` — verify after fix.)
- **B3 trigger:** `{table: T, related: [{correlation: …, subquery: {table: child, limit: N}}]}` — child Take with limit. Mutation block must mutate the child table to exercise push (line 119: `update m-1 body`, line 137: `delete m-2`).

A focused arb can be `fc.oneof(arbAst, arbB3Targeted)` where `arbB3Targeted` always emits a related-with-limit shape. This is "hope" → "guarantee" conversion — every iteration draws AT LEAST one of these.

---

## Two-Process Harness Reuse

### Reusable primitives

| File                                  | Primitive                                                                  | New consumer                                         |
| ------------------------------------- | -------------------------------------------------------------------------- | ---------------------------------------------------- |
| `harness-coverage.ts:100-224`         | `subscribeAndHydrate(url, hash, ast, schema)`                              | `harness-fuzz.ts` for hydration-only iterations      |
| `harness-coverage.ts:228-289`         | `canon`, `hashRows`, `summarizeDiff`                                       | Per-iteration parity assertion                       |
| `harness-advance-coverage.ts:279-396` | `subscribe(url, hash, ast, schema)` (returns `Sub` with `lastPokeEndAtMs`) | Long-lived sockets across many iterations in a batch |
| `harness-advance-coverage.ts:68-196`  | `MUTATIONS[]` array (17 steps with `run`/`undo`)                           | Reused verbatim as the per-batch mutation block      |
| `harness-advance-coverage.ts:614-687` | `runBatch(batch, schema, sql)`                                             | The fast-check iteration driver wraps and calls this |
| `harness-advance-coverage.ts:551-608` | `cleanupDb(sql)` with retry loop                                           | Per-batch cleanup; leaves seed in baseline state     |
| `dual-executor.ts:283`                | `compareChanges(ts, rs)` (multiset-aware)                                  | Per-iteration TS↔RS delta diff                       |

### Per-iteration flow (recommended)

`zero-cache` startup is **slow** — measured ~5-15s per process per cold start, with replicator catch-up. Per-AST restart is the budget killer. Strategy:

```
Build phase (once per `npm run fuzz:check`)
├── start-ts  (port 4858)        ─┐
├── start-rs  (port 4868)        ─┤  background, KEEP RUNNING
└── pg-connect (port 6434)       ─┘

Iteration phase (1000 times per FUZZ_NUM_RUNS=1000)
├── batch fill: collect BATCH_SIZE ASTs from fast-check
├── batch hydrate: open BATCH_SIZE×2 sockets, await all gotPatch
├── batch snapshot: per-AST initial row mirror
├── batch mutate: run MUTATIONS once per batch (PG)
├── batch await: await pokeEnd quiescence (event-driven, not blind sleep)
├── batch snapshot: per-AST final row mirror
├── batch diff: per-AST delta TS vs RS
├── batch cleanup: cleanupDb (PG); loop until verified gone
└── batch close: close all 2×BATCH_SIZE sockets
```

With `BATCH_SIZE=30` and `FUZZ_NUM_RUNS=1000`: **34 batches**. Each batch ~3-6s on local hardware (per existing `harness-advance-coverage` ~110-220ms/AST after batching). **Estimated total: 100-200 seconds.** That fits under the 2-min budget for 1000 iterations only if batching works.

**Critical detail:** fast-check's standard `fc.assert` model is property-per-iteration: the property runs once per generated value. To batch, the property's body must enqueue and not block until the batch flushes. Use a `BatchedRunner` that:

1. `enqueue(ast)` returns a Promise that resolves when the batch containing this AST flushes.
2. When `enqueue.length >= BATCH_SIZE`, `runBatch` is invoked.
3. Final flush after all 1000 iterations enqueued (in a `finalizer` hook).
4. fast-check shrinker still works because the runner records ast→result mapping and the property reads its result from the map.

### Postgres truncate-and-reseed vs schema-per-iteration (CD-02)

**Recommendation: keep the existing batch model with `cleanupDb` retry-verify loop.** TRUNCATE-and-reseed adds 100-300ms per iteration to wipe and re-INSERT the seed; with replicator catch-up it's worse. The `MUTATIONS[].undo` pattern is precise: only the test-introduced rows are reverted; baseline seed stays. `cleanupDb` (`harness-advance-coverage.ts:551-608`) already handles retry until verified.

**Do NOT** schema-per-iteration. Re-applying schema would force replicator full-resync (multi-second) and invalidate `ast_corpus.json` deduplication.

**One caveat:** if Track 1 generates ASTs that exercise tables NOT touched by `MUTATIONS[]`, those iterations don't observe push; the result is hydration-parity only. That's fine for FUZZ-01 — diff diff hydration is itself valuable. The advance sweep's ASTs that DO match an intersection of (where|related touches a mutated table) get push coverage. Phase 34 should add to `MUTATIONS[]` if FUZZ-02 introduces tables not currently mutated (e.g., a new jsonb-column table).

---

## Schema Extension Layout (FUZZ-02)

### Density target: `apps/zbugs/shared/schema.ts`

[CITED: `apps/zbugs/shared/schema.ts:1-200`]

zbugs has 12+ tables, mix of single-PK and compound-PK, multiple `enumeration<>` columns, optional `string()`/`number()` columns, and one boolean per table on average. The current `tools/ivm-parity/zero-schema.ts` has 13 tables but is heavy on `string()` and `number()` — no `boolean()` in many tables, no `enumeration`, and (because Zero's typed schema doesn't expose JSON/timestamptz/numeric directly) no rich PG types.

### Concrete column additions

The fuzzer's `zero-schema.ts` uses Zero's TypeScript schema builder. Native primitives are `string`, `number`, `boolean`. To exercise `jsonb / timestamptz / numeric`, the strategy is:

1. **At the PG/SQLite layer (real types):** add columns with `JSONB`, `TIMESTAMPTZ`, `NUMERIC` to `schema.sql`. Zero replicates these as `string` in the wire protocol (timestamps as ISO 8601 string, jsonb as JSON-string), so the AST-level fuzzer sees them as `string` columns but the PG/SQLite side carries the real semantics.

2. **At the Zero schema layer:** declare these columns as `string()` (which is what Zero supports) — Zero stores them as text and the conversion happens at the PG client level.

3. **For numeric precision (B8/B9 organic discovery):** add a `bigint` column. Zero's `number()` is JS Number (f64). For an i64 column with values > 2^53, Zero replicates as `string` (per zero-schema docs) — so the column appears as `string()` in the schema and the i64 lives in PG / SQLite. This will surface B8/B9 if the predicate path gets exercised. [ASSUMED — confirm Zero's actual handling; the deep audit §B8 says `compare_values` uses `as_f64().unwrap_or(0.0)` which loses precision regardless of how Zero declares the column.]

### Proposed additions to `zero-schema.ts`

```ts
// New table: production-shape jsonb + timestamptz + numeric
const event = table('events')
  .columns({
    id: string(),
    occurredAt: string(), // PG TIMESTAMPTZ — replicated as ISO string
    metadataJson: string(), // PG JSONB — replicated as JSON string
    amount: string(), // PG NUMERIC — replicated as string (preserves precision)
    quantity: number(), // f64 — for the i64 > 2^53 boundary, see below
    isProcessed: boolean(),
    actorUserId: string().optional(),
    notes: string().optional(), // nullable text — NULL semantics fixtures
    relatedTicketId: string().optional(),
  })
  .primaryKey('id');

// New table with i64-overflow PKs (B8/B9 organic surface)
const bigIdRecord = table('big_id_records')
  .columns({
    id: string(), // PG BIGINT > 2^53 stored as text via ::TEXT cast in seed
    label: string(),
    parentBigId: string().optional(), // nullable FK to test NULL ≠ NULL in JOIN
    createdAt: string(),
  })
  .primaryKey('id');

// Composite-key + jsonb table for Take partition_key fuzz with rich type
const eventTag = table('event_tags')
  .columns({
    eventId: string(),
    tagKey: string(),
    tagValue: string(),
    confidence: number(), // 0..1; numeric in PG, NaN-free per D-13
    metadataJson: string(), // jsonb (testing nested where on jsonb fields)
  })
  .primaryKey('eventId', 'tagKey');

const eventRelationships = relationships(event, ({many, one}) => ({
  actor: one({
    sourceField: ['actorUserId'],
    destField: ['id'],
    destSchema: user,
  }),
  ticket: one({
    sourceField: ['relatedTicketId'],
    destField: ['id'],
    destSchema: ticket,
  }),
  tags: many({
    sourceField: ['id'],
    destField: ['eventId'],
    destSchema: eventTag,
  }),
}));

const eventTagRelationships = relationships(eventTag, ({one}) => ({
  event: one({sourceField: ['eventId'], destField: ['id'], destSchema: event}),
}));
```

### Proposed additions to `schema.sql`

```sql
-- FUZZ-02: production-shape rich-type table
CREATE TABLE events (
  id              TEXT PRIMARY KEY,
  "occurredAt"    TIMESTAMPTZ NOT NULL,
  "metadataJson"  JSONB NOT NULL DEFAULT '{}'::JSONB,
  amount          NUMERIC(20,6) NOT NULL DEFAULT 0,
  quantity        BIGINT NOT NULL,
  "isProcessed"   BOOLEAN NOT NULL DEFAULT false,
  "actorUserId"   TEXT NULL REFERENCES users(id),
  notes           TEXT NULL,
  "relatedTicketId" TEXT NULL REFERENCES tickets(id)
);
CREATE INDEX events_occurredAt_idx ON events ("occurredAt" DESC);
CREATE INDEX events_actorUserId_idx ON events ("actorUserId");
CREATE INDEX events_metadataJson_gin ON events USING GIN ("metadataJson");

-- B8/B9 organic surface: i64 > 2^53
CREATE TABLE big_id_records (
  id              TEXT PRIMARY KEY,        -- string-encoded; values include ones > 2^53
  label           TEXT NOT NULL,
  "parentBigId"   TEXT NULL,                -- self-FK; NULL allowed for NULL ≠ NULL test
  "createdAt"     TIMESTAMPTZ NOT NULL
);
CREATE INDEX big_id_records_parentBigId_idx ON big_id_records ("parentBigId");

-- Composite key + jsonb for partition-Take coverage
CREATE TABLE event_tags (
  "eventId"       TEXT NOT NULL REFERENCES events(id),
  "tagKey"        TEXT NOT NULL,
  "tagValue"      TEXT NOT NULL,
  confidence      NUMERIC(5,4) NOT NULL,     -- 0..1
  "metadataJson"  JSONB NOT NULL DEFAULT '{}'::JSONB,
  PRIMARY KEY ("eventId", "tagKey")
);
CREATE INDEX event_tags_eventId_idx ON event_tags ("eventId");
```

### Proposed additions to `seed-extras.sql`

NULL semantics fixtures (D-11):

```sql
-- NULL ≠ NULL in equality
INSERT INTO events (id, "occurredAt", "metadataJson", amount, quantity, "isProcessed",
                    "actorUserId", notes, "relatedTicketId") VALUES
  ('ev-null-1', '2026-04-01T00:00:00Z', '{}', 0, 0, false,
   NULL, NULL, NULL),
  ('ev-null-2', '2026-04-01T01:00:00Z', '{}', 0, 0, false,
   NULL, NULL, NULL),  -- two rows with both NULL FKs — must not equi-join

-- NULL in OR (three-valued logic)
  ('ev-pure-or-1', '2026-04-02T00:00:00Z', '{"k":"v"}', 1.5, 100, true,
   'u1', 'note', 't-1'),

-- NULL in JOIN keys — relatedTicketId IS NULL must produce no row when joining
  ('ev-no-ticket', '2026-04-03T00:00:00Z', '{}', 2.0, 200, false,
   'u2', NULL, NULL);

-- Type coercion: numeric precision boundaries (B8/B9 organic surface)
-- 2^53 = 9007199254740992. Use values just above and below.
INSERT INTO big_id_records (id, label, "parentBigId", "createdAt") VALUES
  ('9007199254740991', 'safe-int-max',          NULL,                '2026-04-01T00:00:00Z'),
  ('9007199254740992', '2^53-exact',            '9007199254740991',  '2026-04-01T01:00:00Z'),
  ('9007199254740993', 'first-unsafe',          '9007199254740992',  '2026-04-01T02:00:00Z'),
  ('9007199254740994', 'second-unsafe',         '9007199254740993',  '2026-04-01T03:00:00Z'),
  ('9999999999999999', 'far-unsafe',            '9007199254740994',  '2026-04-01T04:00:00Z');

-- Date round-trips across DST: pick spring-forward and fall-back days
-- US Eastern DST 2026: spring forward 2026-03-08 02:00 → 03:00; fall back 2026-11-01 02:00 → 01:00
INSERT INTO events (…) VALUES
  ('ev-dst-spring', '2026-03-08T06:30:00Z', '{}', 0, 0, false, 'u1', 'spring forward', NULL),
  ('ev-dst-fall',   '2026-11-01T05:30:00Z', '{}', 0, 0, false, 'u1', 'fall back', NULL);

-- jsonb fixtures
INSERT INTO events (id, "occurredAt", "metadataJson", amount, quantity, "isProcessed",
                    "actorUserId", notes, "relatedTicketId") VALUES
  ('ev-jsonb-1', '2026-04-04T00:00:00Z', '{"priority": "high", "tags": ["a", "b"]}', 0, 0, false,
   'u1', NULL, NULL),
  ('ev-jsonb-empty', '2026-04-05T00:00:00Z', '{}', 0, 0, false,
   'u2', NULL, NULL);

-- event_tags for compound-key + Take(partition) coverage
INSERT INTO event_tags ("eventId", "tagKey", "tagValue", confidence, "metadataJson") VALUES
  ('ev-pure-or-1', 'priority', 'high', 0.95, '{}'),
  ('ev-pure-or-1', 'category', 'bug',  0.85, '{}'),
  ('ev-jsonb-1',   'priority', 'low',  0.50, '{}'),
  ('ev-jsonb-1',   'category', 'task', 0.75, '{}');
```

### Required mutation block additions

Add to `MUTATIONS[]` in `harness-advance-coverage.ts:68`:

```ts
{
  label: 'insert event ev-test-1 (jsonb + timestamptz coverage)',
  run: `INSERT INTO events (id, "occurredAt", "metadataJson", amount, quantity,
        "isProcessed", "actorUserId", notes, "relatedTicketId")
        VALUES ('ev-test-1', '2026-04-29T12:00:00Z', '{"k":"v"}', 99.99, 1, false,
        'u1', 'test event', 't-1')`,
  undo: `DELETE FROM events WHERE id = 'ev-test-1'`,
},
{
  label: 'edit event ev-test-1 metadataJson (jsonb edit)',
  run: `UPDATE events SET "metadataJson" = '{"k":"v2","added":true}' WHERE id = 'ev-test-1'`,
  undo: ``,
},
{
  label: 'flip events.actorUserId NULL→u2 (NULL semantics edit)',
  run: `UPDATE events SET "actorUserId" = 'u2' WHERE id = 'ev-null-1'`,
  undo: `UPDATE events SET "actorUserId" = NULL WHERE id = 'ev-null-1'`,
},
{
  label: 'add tag (eventId, tagKey) compound-key partition Take',
  run: `INSERT INTO event_tags VALUES ('ev-test-1', 'priority', 'high', 0.99, '{}')`,
  undo: `DELETE FROM event_tags WHERE "eventId" = 'ev-test-1' AND "tagKey" = 'priority'`,
},
```

### Cleanup additions to `cleanupDb`

```ts
await sql`DELETE FROM event_tags WHERE "eventId" = 'ev-test-1'`;
await sql`DELETE FROM events WHERE id = 'ev-test-1'`;
```

---

## Runtime State Inventory (refactor-track only)

> Phase 34 has both refactor (Track 2 fixes) AND greenfield (FUZZ-01 generator) elements. This section covers Track 2 only.

| Category                             | Items Found                                                                                                                                                                                                                                                                              | Action Required                                                                                                             |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| Stored data                          | None — `parent_sizes` cache (B2) is in-memory per-pipeline; cleared on `swap_snapshot.reset_state()` (`advance.rs:1890`). No persistent caches.                                                                                                                                          | None — fix in code; no migration.                                                                                           |
| Live service config                  | None — Phase 34 fixes affect ONLY the running zero-cache binary. No external service config.                                                                                                                                                                                             | None.                                                                                                                       |
| OS-registered state                  | None.                                                                                                                                                                                                                                                                                    | None.                                                                                                                       |
| Secrets/env vars                     | `ZERO_USE_RUST_IVM_V2=1` is the runtime gate for using Rust IVM. Unchanged. `ZERO_APP_ID=parity_ts` / `parity_rs` distinguish the two harness caches; unchanged. New env vars: `FUZZ_NUM_RUNS`, `FUZZ_SEED` (fast-check standard).                                                       | None — additive env vars only, defaults present.                                                                            |
| Build artifacts / installed packages | `packages/zqlite-rs/target/...` — Rust unit tests must be re-run after B1/B2/B3/B11 edits; cargo will recompile. `packages/zero/out/zero-cache-shadow-ffi/*.darwin-arm64.node` — must be rebuilt + copied per `tools/ivm-parity/SKILL.md` "RS cache not seeing changes" troubleshooting. | After every Track 2 fix: `cargo build --release` in `packages/zqlite-rs` + `packages/zero-ivm-rs`; restart RS cache by PID. |

**Nothing found in OS / secrets / live-config categories — verified by inspection of `tools/ivm-parity/package.json` scripts and `RustPipelineManager` source.**

---

## CI Budget Compliance (D-05 — 1k iterations <2 min)

### Cost model

| Operation                   | Cost              | Notes                                                                                       |
| --------------------------- | ----------------- | ------------------------------------------------------------------------------------------- |
| zero-cache TS startup       | 5-15 s × 1 (once) | Dominant fixed cost; reuse across all iterations.                                           |
| zero-cache RS startup       | 5-15 s × 1 (once) | Same.                                                                                       |
| Per-AST subscribe + hydrate | 50-200 ms         | WS handshake + replicator → cache → hydration poke.                                         |
| Per-batch mutate + await    | 200-1000 ms       | PG mutate ~50ms; replicator → cache propagation 100-500ms; pokeEnd quiescence event-driven. |
| Per-batch cleanup           | 50-200 ms         | DELETE + verification poll.                                                                 |

### Throughput math

`harness-advance-coverage.ts` last full-corpus run: **221.2s for 1084 ASTs** = **~204 ms/AST** averaged. With BATCH_SIZE=30 that's ~6.1s/batch (180×34 batches). Phase 34 fast-check 1000 iterations at the same rate: **204s — JUST OVER the 2-min budget.**

### Mitigations

1. **Increase BATCH_SIZE.** Current default is 30. Doubling to 60 amortizes per-batch fixed costs but increases socket count. Local zero-cache handles 60+ concurrent subs comfortably (Phase 31 streaming tests).
2. **Skip the cleanup verification poll on iterations where the previous batch's PG state is already clean.** A fast-path: if the previous batch's cleanupDb returned on the first poll, skip the verification loop on the next batch's pre-mutate step. Saves 20-100ms × 34 batches.
3. **Reduce HYDRATE_TIMEOUT for Phase 34's fuzz mode.** Default 20000ms. Set 5000ms — if hydration takes longer than 5s, the AST is pathological and should be reported as a hydration error (which fast-check then shrinks).
4. **Run fewer mutation steps per batch when fuzz iteration AST doesn't intersect any mutation's table.** The 17-step block currently runs unconditionally. A pre-check: for each batch, compute the set of tables touched by ANY AST; only run mutation steps that touch those tables. Saves 100-300ms/batch when ASTs are filter-only over a single table.
5. **Parallelize batches via `Promise.all`.** Currently batches run sequentially. Two zero-cache sub pools could run in parallel with separate PG schemas — but this requires schema isolation which Phase 34 doesn't have, so SKIP.

The minimum viable goal: 1000 iterations in <120s = **120 ms/AST**. With BATCH_SIZE=60 and pruned mutations, this is achievable but tight. **If the budget is missed, Phase 34's verification gate (D-21 #3) becomes the deferring decision.** The planner should add a probing task: "Measure actual fuzz iteration cost in early Wave 1 with skeleton fast-check generator; if >120 ms/AST average, optimize before extending corpus."

### Shrinking budget

fast-check shrinking activates ONLY when a property fails. Successful 1000 iterations = 0 shrink steps. After Track 2 fixes land, the expected outcome is 0 unexpected divergences (allow-list filters the 5 known + 4 deferred). When shrinking DOES fire (during dev iterations), it's ~10-100 additional iterations to reach a minimal counterexample. This is bounded and not part of the 2-min budget for clean runs.

---

## Common Pitfalls

### Pitfall 1: Per-AST cache restart blowing the CI budget

**What goes wrong:** Implementer naively wraps fast-check around `subscribeAndHydrate` per iteration, restarting WS and replaying replicator catch-up.
**Why it happens:** fast-check's idiomatic `fc.assert(fc.asyncProperty(arb, async ast => {...}))` runs property body once per generated value. Without batching, each iteration is full setup/teardown.
**How to avoid:** BatchedRunner pattern (§Two-process harness reuse). Property body returns Promise that resolves when batch flushes.
**Warning signs:** Total fuzz time > 5 minutes for 1k iterations. Per-iteration log lines spaced > 200ms apart.

### Pitfall 2: TS oracle process startup masquerading as "test framework slowness"

**What goes wrong:** First 1-2 iterations time out; tests appear flaky.
**Why it happens:** zero-cache replicator catch-up after schema change can take 5-15s. If FUZZ-02 schema changes are applied just before fuzz run, the first iterations hit pre-replicator-ready cache.
**How to avoid:** Add a warm-up phase: run a single trivial AST through both caches before starting fuzz iterations. Wait for both `gotPatch` events.
**Warning signs:** First 5 iterations error with "hydrate timeout" but subsequent 995 succeed.

### Pitfall 3: Postgres connection-pool exhaustion under concurrent batches

**What goes wrong:** PG `FATAL: too many connections for role "user"` mid-fuzz.
**Why it happens:** zero-cache opens N connections per cache (N=NUM_SYNC_WORKERS=2). Two caches × 2 workers + harness mutator = 5+. Local PG default is 100. Multi-batch Promise.all attempts add 30 more per batch → 30 × 34 = 1020 attempted ports.
**How to avoid:** Don't parallelize batches (just run sequentially). Cap `MAX_PARALLEL` in harness at 8 (already current default). Monitor `pg_stat_activity` if errors appear.
**Warning signs:** PG ECONNRESET errors mid-batch; postgres.js error `MaxListenersExceededWarning`.

### Pitfall 4: fast-check seed reproducibility lost on shrinking failure

**What goes wrong:** A divergence is found on iteration 873 with seed S. Shrinking starts but takes too long; user kills the run. They re-run with seed S but the same divergence doesn't reproduce.
**Why it happens:** fast-check's seed determines INITIAL value sequence; shrinking is itself non-deterministic relative to the seed when the property body has side effects (PG state, socket close timing). Each re-run sees slightly different timing → slightly different divergence pattern.
**How to avoid:** When a divergence is found, IMMEDIATELY persist the AST JSON + seed to `ast_corpus.regressions.json` BEFORE shrinking. Shrink in a separate process. Reproduction uses the persisted AST directly, not the seed.
**Warning signs:** "I can't reproduce the bug" comments after a fuzz run.

### Pitfall 5: B3 fix surfaces latent assumptions in tests that masked the bug

**What goes wrong:** Track 2 B3 fix lands. Some existing tests fail because they relied on the buggy "global Take state shared across constraints" behavior.
**Why it happens:** Hydrate currently works (per-constraint fallback in `take_op.rs:317-329`). Some tests may have explicit constraint wiring that worked accidentally.
**How to avoid:** Run full vitest suite + `cargo test` BEFORE landing B3 to capture baseline. After B3, run again. Investigate every newly-failing test — it's likely revealing a latent buggy expectation, not an actual regression. Per D-17, do not work around.
**Warning signs:** New failures in `take_op.rs::tests` or `pipeline-driver.*.test.ts` after B3 lands. Tests with `partition_key: None` AND constraint passes.

### Pitfall 6: B11 fix needs prev-snapshot lifecycle handled correctly

**What goes wrong:** `set_prev_snapshot` is called once at startup but never updated. Or it's called AFTER `swap_snapshot`. Either way, descendant SQL reads from the wrong snapshot.
**Why it happens:** TS pipeline-driver maintains `prev` and `curr` snapshots leapfrogging across advances. After advance, `prev` for the NEXT advance is the CURRENT `curr`. The set_prev call must happen on each advance, BEFORE the swap.
**How to avoid:** In TS, sequence is `set_prev_snapshot(prev.db.db.name); swap_snapshot(curr.db.db.name); advance(...)`. Documented in pipeline-driver.ts via comment citing `// snapshot lifecycle: see B11 fix`. Rust unit test must verify the sequence.
**Warning signs:** Cascade-delete differential tests pass for the first advance but fail on the second.

### Pitfall 7: Schema reload requiring replica wipe

**What goes wrong:** FUZZ-02 schema changes applied. Replicator starts but immediately fails because old replica file has stale schema.
**Why it happens:** SQLite replica files (`/tmp/ivm-parity-ts.db*`, `/tmp/ivm-parity-rs.db*`) are not auto-rebuilt on schema change.
**How to avoid:** README.md "Want to change the schema" procedure — `npm run deploy-schema && npm run db-migrate` then `rm /tmp/ivm-parity-*.db*` before restarting caches.
**Warning signs:** Replicator startup error mentioning "table already exists" or column-mismatch.

### Pitfall 8: jsonb / NUMERIC / TIMESTAMPTZ replication semantics

**What goes wrong:** A predicate like `events.where(e => e.metadataJson === '{"k":"v"}')` produces different output on TS vs RS because one side normalizes JSON whitespace and the other doesn't.
**Why it happens:** Zero replicates jsonb as text. PG's `JSONB::TEXT` cast canonicalizes whitespace on the way out; SQLite stores whatever Zero writes. If the wire protocol passes JSON strings through unchanged, normalization is consistent. If somewhere a `JSON.parse + JSON.stringify` happens, it's not.
**How to avoid:** First fuzz batch SHOULD include a literal-equality predicate over jsonb to detect this immediately. If divergence appears, this is a NEW divergence and goes into `PARITY_STATUS.md` (per D-20) — don't try to fix mid-Phase 34.
**Warning signs:** Divergences clustered on jsonb columns immediately after FUZZ-02 lands.

---

## Code Examples

### Example 1: Track 2 B1 fix (Skip ordering)

```rust
// Source: packages/zqlite-rs/src/ast_to_config.rs:200-265
// mirrors TS packages/zql/src/builder/builder.ts:302-345
pub fn ast_to_operator_configs(
    schema: &mut SchemaCache,
    ast: &Ast,
    primary_key: &[String],
    partition_key: Option<Vec<String>>,        // NEW per B3
) -> Result<Vec<OperatorConfig>, String> {
    let table_name = &ast.table;
    let columns = schema.get_columns(table_name)?;
    let order_by: Vec<(String, String)> = ast.order_by.as_ref().cloned().unwrap_or_default();
    let mut sort = order_by.clone();
    let existing: HashSet<String> = sort.iter().map(|(f, _)| f.clone()).collect();
    for pk_col in primary_key {
        if !existing.contains(pk_col) {
            sort.push((pk_col.clone(), "asc".to_string()));
        }
    }

    let mut configs = Vec::new();

    // 1. Source — TS builder.ts:291-296
    configs.push(OperatorConfig::Source {
        table_name: table_name.clone(),
        columns,
        primary_key: primary_key.to_vec(),
        sort: sort.clone(),
    });

    // 2. Skip — MOVED HERE per B1. TS builder.ts:302-306.
    if let Some(start) = &ast.start {
        configs.push(OperatorConfig::Skip {
            bound_row: start.row.clone(),
            exclusive: start.exclusive,
            sort: sort.clone(),
        });
    }

    // 3. Where conditions → CSQ-Exists then Filter. TS builder.ts:308-333.
    if let Some(cond) = ast.where_cond.as_deref() {
        append_condition_configs(schema, &mut configs, cond, primary_key)?;
    }

    // 4. Take with partition_key threaded per B3. TS builder.ts:335-345.
    if let Some(limit) = ast.limit {
        configs.push(OperatorConfig::Take {
            limit, sort: sort.clone(),
            partition_key: partition_key.clone(),
        });
    }

    // 5. Related Joins — pass child_field as child's partition_key. TS builder.ts:626-632.
    if let Some(related) = &ast.related {
        for rel in related {
            let child_pk = schema.get_primary_key(&rel.subquery.table)?;
            let child_partition = Some(rel.correlation.child_field.clone());
            let child_configs = ast_to_operator_configs(
                schema, &rel.subquery, &child_pk, child_partition,
            )?;
            configs.push(OperatorConfig::Join {
                parent_key: rel.correlation.parent_field.clone(),
                child_key: rel.correlation.child_field.clone(),
                relationship_name: relationship_name(rel),
                child: child_configs,
            });
        }
    }

    Ok(configs)
}
```

### Example 2: B11 set_prev_snapshot + advance

```rust
// Source: packages/zqlite-rs/src/advance.rs (RustPipelineManager)
// mirrors TS pipeline-driver.ts:1542-1577 snapshot lifecycle.
struct RustPipelineManager {
    db_path: Mutex<String>,
    prev_db_path: Mutex<Option<String>>,    // NEW
    pipelines: RwLock<Vec<Mutex<PipelineState>>>,
    shared_pool: Arc<ConnectionPool>,
    // …
}

#[napi]
impl RustPipelineManager {
    /// Set the path of the snapshot whose state should be queried for
    /// cascade-delete enumeration. Must be called BEFORE swap_snapshot
    /// on each advance. Mirrors TS pipeline-driver.ts diff-from-prev.
    #[napi]
    pub fn set_prev_snapshot(&self, prev_db_path: String) -> napi::Result<()> {
        *self.prev_db_path.lock().unwrap() = Some(prev_db_path);
        Ok(())
    }

    #[napi]
    pub fn advance(&self, changes_json: String) -> napi::Result<Buffer> {
        let changes: Vec<Change> = serde_json::from_str(&changes_json)?;
        if changes.is_empty() { … }

        let pipelines = self.pipelines.read()?;
        let db_path = self.db_path.lock().unwrap().clone();
        // B11: descendants are enumerated against prev so deletes-in-tx
        // don't get silently elided.
        let prev_db_path = self.prev_db_path.lock().unwrap().clone()
            .unwrap_or_else(|| db_path.clone());  // back-compat: if not set, use curr (buggy)

        let all_row_changes: Vec<RowChange> = pipelines.iter().flat_map(|pm| {
            let mut p = pm.lock().unwrap();
            advance_persistent_pipeline(&mut p, &changes, &db_path, &prev_db_path)
        }).collect();
        // …
    }
}
```

### Example 3: fast-check arb-ast composition

```ts
// Source: tools/ivm-parity/arb-ast.ts (NEW)
// extends the pattern from packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts:14-100
import fc from 'fast-check';
import type {
  AST,
  Condition,
  CorrelatedSubquery,
  SimpleCondition,
  SimpleOperator,
} from '../../packages/zero-protocol/src/ast.ts';

export interface AdaptedSchema {
  tables: Record<string, TableDef>;
}

export function buildArbitraries(adapted: AdaptedSchema): {
  arbAst: fc.Arbitrary<AST>;
} {
  const tables = Object.values(adapted.tables);

  const arbSimple = (): fc.Arbitrary<SimpleCondition> =>
    fc.constantFrom(...tables).chain(table =>
      fc.constantFrom(...table.columns).chain(col =>
        fc.constantFrom(...opsForColumn(col)).chain(op =>
          fc.constantFrom(...valuesForOp(col, op)).map(value => ({
            type: 'simple' as const,
            op,
            left: {type: 'column' as const, name: col.name},
            right: {type: 'literal' as const, value},
          })),
        ),
      ),
    );

  const arb = fc.letrec<{cond: Condition; csq: CorrelatedSubquery; ast: AST}>(
    tie => ({
      cond: fc.oneof(
        {weight: 5, arbitrary: arbSimple()},
        {
          weight: 2,
          arbitrary: fc.record({
            type: fc.constantFrom('and' as const, 'or' as const),
            conditions: fc.array(tie('cond'), {minLength: 2, maxLength: 3}),
          }),
        },
        {
          weight: 3,
          arbitrary: tie('csq').map(c => ({
            type: 'correlatedSubquery' as const,
            related: c,
            op: 'EXISTS' as const,
          })),
        },
      ),
      csq: fc.constantFrom(...tables).chain(table => {
        const rels = table.relationships;
        if (rels.length === 0) return arbSimple().map(() => null as never); // skip
        return fc.constantFrom(...rels).map(rel => ({
          correlation: {
            parentField: rel.parentField,
            childField: rel.childField,
          },
          subquery: {
            table: rel.childTable,
            alias: `arb_${table.name}_${rel.name}`,
          },
        }));
      }),
      ast: fc.record({
        table: fc.constantFrom(...tables.map(t => t.name)),
        where: fc.option(tie('cond'), {nil: undefined}),
        limit: fc.option(fc.integer({min: 1, max: 10}), {nil: undefined}),
        // start, related, orderBy added similarly
      }),
    }),
  );

  return {arbAst: arb.ast};
}
```

---

## State of the Art

| Old Approach                                           | Current Approach                                                    | When Changed | Impact                                                                       |
| ------------------------------------------------------ | ------------------------------------------------------------------- | ------------ | ---------------------------------------------------------------------------- |
| BFS-only corpus (`ast-fuzz.ts`)                        | BFS + fast-check hybrid (Phase 34 D-04)                             | This phase   | Fast-check finds shapes BFS missed; shrinking gives minimal counterexamples. |
| Basic-types schema (string + number + boolean)         | jsonb + timestamptz + numeric + nullable + composite (D-09)         | This phase   | Production-shape fuzzing surfaces type-coercion bugs (B8/B9 organic).        |
| `partition_key: None` in child Take                    | Threaded `partition_key` through `ast_to_operator_configs` (B3 fix) | This phase   | Closes silent push-as-no-op for related-with-limit.                          |
| `parent_sizes.insert(pk, count.max(1))`                | `parent_sizes.insert(pk, count)` (B2 fix)                           | This phase   | Real count cached; or_predicate decision re-evaluated.                       |
| `emit_descendant_removals` reads from `db_path` (curr) | Reads from `prev_db_path` (B11 fix)                                 | This phase   | Same-tx descendant deletes are no longer elided.                             |
| Skip after Filter+Exists (`ast_to_config.rs`)          | Skip before conditions (B1 fix)                                     | This phase   | Architectural alignment with TS `builder.ts:302-329`.                        |

**Deprecated/outdated:**

- Per-AST WebSocket teardown — was acceptable when corpus was 1k BFS items in ~2 min. With fast-check shrinking iterations + production-shape schema, must batch.
- `debug_assert!` for framework invariants — already addressed in Phase 30 (AUDIT-03). Confirm B3 fix continues this discipline (use `assert!` for "child Take constraint without partition_key" if such a case is reachable post-fix).

---

## Assumptions Log

| #   | Claim                                                                                                                                                                                                                        | Section                                              | Risk if Wrong                                                                                                                                                                                                                                                                                                                            |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1  | Zero replicates BIGINT/i64 values > 2^53 as `string()` columns at the schema layer, with the actual i64 in PG and the wire protocol carrying the integer via the JSON-fallback tag (tag 6 from `decode-advance-buf.ts:229`). | §Schema Extension Layout — concrete column additions | If Zero replicates as `number()` (f64), B8/B9 surface immediately on every i64 > 2^53; the schema layout still works but the failure mode shifts from "compare_values lossy" to "wire-protocol decode lossy". Either way the row gets surfaced. Verify by inspecting `packages/zero-cache/src/services/replicator/` for BIGINT handling. |

**No other claims tagged `[ASSUMED]` in this research.** All B1/B2/B3/B11 references and TS-spec citations are verified directly against the local codebase or the upstream reference repo at `/private/tmp/ivm-parity-ts-ref`.

---

## Open Questions

1. **Should the advance napi method's signature for B11 use `set_prev_snapshot` (Option B — additive method) or extend `advance(changes_json, prev_db_path?)` (Option A — new optional arg)?**
   - What we know: D-22 / CLAUDE.md verification gate #4 says "no signature change to existing buffered methods (`advance`, ...)". Option A is technically a signature change (additive arg); Option B is purely additive (new method).
   - What's unclear: is the spirit of the constraint "no breaking change" (Option A is fine) or "no surface-area change at all" (Option B is required)?
   - Recommendation: **Option B** by default (safer interpretation of the constraint, no risk to TS callers, no napi-rs Option<T> ergonomics surprises). Planner can flag for user input if they prefer Option A.

2. **Should `tools/ivm-parity/synth-p6.ts` and `synth-p7.ts` be merged into the new fast-check generator (CD-03)?**
   - What we know: They are existing shipped-query archetype synthesizers. fast-check generates random ASTs.
   - What's unclear: do they cover shapes the random generator won't reach probabilistically?
   - Recommendation: **Keep separate**. They feed `seed-extractor.ts` which produces `seed_*` corpus entries — these are deterministic and high-confidence. Fast-check is exploratory. Both exist; both run.

3. **Is the 2-min budget for 1k iterations achievable with current zero-cache startup overhead?**
   - What we know: Last `harness-advance-coverage.ts` full run was 221.2s for 1084 ASTs.
   - What's unclear: Phase 34's batch size, mutation pruning, and concurrency optimizations get to <120s. But existing 221s is JUST OVER budget already.
   - Recommendation: **Wave 1 must include a probing task** that times the skeleton fuzz harness with 100 iterations and projects to 1000. If projected total > 150s, optimize (BATCH_SIZE=60, mutation pruning) before extending FUZZ-02 schema.

4. **What is the right allow-list format (CD-04)?**
   - What we know: 5 catalogued divergences exist (`PARITY_STATUS.md`). 4 deferred (B5/B6/B7/B12) shapes need pre-filtering.
   - What's unclear: per-AST exact-match (canonical key) vs shape-pattern match (e.g., "any AST with `flip: true`")?
   - Recommendation: **Per-canonical-key** (use existing `canonicalKey` from `ast-fuzz.ts:834`). Pattern-match adds complexity and risks over-suppressing real divergences. List in `tools/ivm-parity/allow-list.json`.

---

## Environment Availability

| Dependency               | Required By                                  | Available                                                   | Version                                   | Fallback |
| ------------------------ | -------------------------------------------- | ----------------------------------------------------------- | ----------------------------------------- | -------- |
| Node.js                  | All TS code                                  | ✓                                                           | ≥18 (per `tools/ivm-parity/package.json`) | —        |
| npm                      | Workspace                                    | ✓                                                           | (project std)                             | —        |
| `tsx`                    | run scripts in `tools/ivm-parity/`           | ✓                                                           | (per package.json)                        | —        |
| `ws`                     | WebSocket client                             | ✓                                                           | (per package.json)                        | —        |
| `postgres` (postgres.js) | PG client                                    | ✓                                                           | ^3.4.5                                    | —        |
| `fast-check`             | random AST generator                         | ✓                                                           | 3.23.2                                    | —        |
| `vitest`                 | full test suite gate                         | ✓                                                           | 4.1.3                                     | —        |
| Postgres 16              | parity DB on port 6434                       | ✓ (per `apps/zbugs/npm run db-up`)                          | —                                         | —        |
| Cargo / Rust             | `cargo test` for `zqlite-rs` + `zero-ivm-rs` | ✓ (existing build artifacts)                                | per `Cargo.toml`                          | —        |
| TS reference repo        | TS oracle (`:4858`)                          | ✓ at `/private/tmp/ivm-parity-ts-ref` (per CONTEXT.md D-01) | git worktree of `rocicorp/mono`           | —        |

**Missing dependencies:** None — all required infrastructure is already in place. Phase 34 adds NO new dependencies.

---

## Validation Architecture

> Per `.planning/config.json`, `workflow.nyquist_validation: true`. This section drives `34-VALIDATION.md` construction.

### Test Framework

| Property                       | Value                                                                                                                                                                                                  |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Frameworks                     | `cargo test` (Rust unit) + `vitest` 4.1.3 (TS unit/integration) + `tsx` (harness scripts)                                                                                                              |
| Config files                   | `packages/zqlite-rs/Cargo.toml`, `packages/zero-ivm-rs/Cargo.toml`, `vitest.config.ts` (per package), `tools/ivm-parity/package.json`                                                                  |
| Quick run (Track 2 B1)         | `cd packages/zqlite-rs && cargo test --release --lib ast_to_config::tests::test_b1_skip_before_conditions`                                                                                             |
| Quick run (Track 2 B2)         | `cd packages/zero-ivm-rs && cargo test --release --lib exists_op::tests::test_b2_parent_sizes_real_count`                                                                                              |
| Quick run (Track 2 B3)         | `cd packages/zero-ivm-rs && cargo test --release --lib take_op::tests::test_b3_partition_key_threaded` + `cd packages/zqlite-rs && cargo test --release --lib ast_to_config::tests::test_b3_threading` |
| Quick run (Track 2 B11)        | `cd packages/zqlite-rs && cargo test --release --lib advance::tests::test_b11_descendants_from_prev`                                                                                                   |
| Quick run (FUZZ-01 dev)        | `cd tools/ivm-parity && FUZZ_NUM_RUNS=50 npm run fuzz-check` (proposed new script)                                                                                                                     |
| Full Rust suite                | `cargo test --release -p zero-ivm-rs && cargo test --release -p zqlite-rs --lib -- --test-threads=1`                                                                                                   |
| Full TS suite                  | `cd packages/zero-cache && npx vitest run src/services/view-syncer/`                                                                                                                                   |
| Full parity sweep (BFS + fuzz) | `cd tools/ivm-parity && npm test` (existing) + new `npm run fuzz-check` (Track 1)                                                                                                                      |
| Phase 33 bench re-run          | `cd packages/zero-cache && npx vitest run src/services/view-syncer/rust-ivm-streaming-bench.test.ts -t "TTFB"` and `-t "memory"`                                                                       |

### Phase Requirements → Test Map

| Req ID          | Behavior                                                                                                       | Test Type   | Automated Command                                                                                                                                                          | File Exists?                                                   |
| --------------- | -------------------------------------------------------------------------------------------------------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| FUZZ-01         | fast-check generates random ASTs across full operator surface; 1000 iterations <2 min                          | integration | `cd tools/ivm-parity && FUZZ_NUM_RUNS=1000 npm run fuzz-check`                                                                                                             | ❌ Wave 0 (new script `fuzz-check`)                            |
| FUZZ-01         | Allow-list filters 5 known + 4 deferred divergences; all others fail                                           | integration | Same command; expects exit 0 with `unexpected_divergences == 0`                                                                                                            | ❌ Wave 0                                                      |
| FUZZ-01         | Shrinking produces minimal counterexample on divergence                                                        | unit        | `cd tools/ivm-parity && FUZZ_NUM_RUNS=10 FORCE_DIVERGENCE=1 npm run fuzz-check` (test mode that injects a known-bad AST)                                                   | ❌ Wave 0                                                      |
| FUZZ-02         | jsonb / timestamptz / numeric columns exist in `events` table; replication produces matching row data on TS+RS | integration | `cd tools/ivm-parity && npm test` (existing sweep, post-FUZZ-02 schema)                                                                                                    | ✅ existing                                                    |
| FUZZ-02         | NULL semantics fixtures present in seed                                                                        | unit        | `psql -c "SELECT count(*) FROM events WHERE \"actorUserId\" IS NULL"` from harness boot                                                                                    | ❌ Wave 0 (assertion in `harness-advance-coverage.ts` startup) |
| FUZZ-02         | i64 > 2^53 fixtures roundtrip via wire protocol                                                                | integration | `cd tools/ivm-parity && CORPUS_LIMIT=10 BIG_ID_FOCUS=1 npm test`                                                                                                           | ❌ Wave 0 (focused harness mode)                               |
| B1              | Skip placement order matches TS `builder.ts:302`                                                               | unit (Rust) | `cargo test --release -p zqlite-rs --lib ast_to_config::tests::test_b1_skip_before_conditions`                                                                             | ❌ Wave 0                                                      |
| B1              | Differential test for `where: {EXISTS} + start: {row, exclusive}` shape                                        | integration | `cd tools/ivm-parity && CORPUS_LIMIT=200 npm test` (existing corpus exercises this)                                                                                        | ✅ existing                                                    |
| B2              | parent_sizes caches real count                                                                                 | unit (Rust) | `cargo test --release -p zero-ivm-rs --lib exists_op::tests::test_b2_parent_sizes_real_count`                                                                              | ❌ Wave 0                                                      |
| B2              | Differential test for `OR(simple, EXISTS)` with empty children                                                 | integration | `cd tools/ivm-parity && npm test` (existing fuzz_001xx corpus)                                                                                                             | ✅ existing                                                    |
| B3              | partition_key threaded into child Take                                                                         | unit (Rust) | `cargo test --release -p zqlite-rs --lib ast_to_config::tests::test_b3_partition_key_threading`                                                                            | ❌ Wave 0                                                      |
| B3              | Take fetch and push state keys match                                                                           | unit (Rust) | `cargo test --release -p zero-ivm-rs --lib take_op::tests::test_b3_state_key_consistency`                                                                                  | ❌ Wave 0                                                      |
| B3              | Differential test for `related[].limit + child mutation`                                                       | integration | `cd tools/ivm-parity && npm test` (advance sweep with extended MUTATIONS)                                                                                                  | ✅ existing                                                    |
| B11             | Descendants enumerated from prev snapshot                                                                      | unit (Rust) | `cargo test --release -p zqlite-rs --lib advance::tests::test_b11_descendants_from_prev`                                                                                   | ❌ Wave 0                                                      |
| B11             | Differential test for cascade-delete on multi-table tx                                                         | integration | `cd tools/ivm-parity && npm test` (existing channel-delete cascade in MUTATIONS)                                                                                           | ✅ existing                                                    |
| (no-regression) | All Phase 30-33 tests pass                                                                                     | suite       | `cargo test --release -p zero-ivm-rs && cargo test --release -p zqlite-rs --lib -- --test-threads=1 && cd packages/zero-cache && npx vitest run src/services/view-syncer/` | ✅ existing                                                    |
| (no-regression) | `streaming-vs-buffered-parity.fuzz.test.ts` passes 1k iterations                                               | suite       | `cd packages/zero-cache && FUZZ_NUM_RUNS=1000 npx vitest run src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts`                                           | ✅ existing                                                    |
| (no-regression) | Phase 33 benches do not regress                                                                                | bench       | `cd packages/zero-cache && npx vitest run src/services/view-syncer/rust-ivm-streaming-bench.test.ts` and append to `.planning/milestones/v5.0-bench-results.md`            | ✅ existing                                                    |

### Sampling Rate

- **Per task commit:** Quick run for the touched plan/Track item (Rust unit test for B-fix; fuzz-check skeleton run for fast-check work).
- **Per wave merge:** Full Rust suite + full TS view-syncer suite + 100-iteration fuzz check.
- **Phase gate (D-21):** Full Rust + full TS + 1k fuzz-check (<2 min) + Phase 33 benches re-run (no regression) + allow-list verification.
- **Max feedback latency:** 30s for unit tests; 2 min for full sweep; 5 min for full no-regression gate.

### Wave 0 Gaps

- [ ] `tools/ivm-parity/arb-ast.ts` — fast-check arbitraries
- [ ] `tools/ivm-parity/random-ast-fuzz.ts` — fast-check driver script
- [ ] `tools/ivm-parity/harness-fuzz.ts` — batched runner adapting `harness-advance-coverage.ts` to fast-check property model
- [ ] `tools/ivm-parity/allow-list.json` — 5 known divergences + B5/B6/B7/B12 deferred (canonical-key form)
- [ ] `tools/ivm-parity/ast_corpus.regressions.json` — append-only minimal counterexamples (gitignore? — discuss in plan-check)
- [ ] `tools/ivm-parity/package.json` — new scripts: `fuzz-check`, `fuzz-shrink`, `fuzz-record`
- [ ] FUZZ-02 schema additions: `zero-schema.ts`, `schema.sql`, `seed-extras.sql`, `MUTATIONS[]` extensions in `harness-advance-coverage.ts`
- [ ] Rust unit tests for B1 / B2 / B3 / B11 (1 file in each of `ast_to_config.rs::tests`, `exists_op.rs::tests`, `take_op.rs::tests`, `advance.rs::tests`)
- [ ] No new framework install (fast-check 3.23.2 already in node_modules)
- [ ] No new env vars beyond `FUZZ_NUM_RUNS`, `FUZZ_SEED` (fast-check standard)

---

## Security Domain

> `security_enforcement` is not explicitly set in `.planning/config.json` — treat as enabled.

### Applicable ASVS Categories

| ASVS Category         | Applies                                                                                                                                                | Standard Control                                                                                                                                                                    |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| V2 Authentication     | no                                                                                                                                                     | Phase 34 changes are server-side IVM internals + test harness; no auth surface modified.                                                                                            |
| V3 Session Management | no                                                                                                                                                     | Same.                                                                                                                                                                               |
| V4 Access Control     | partial — B6 deferred (PERMISSIONS_EXISTS_LIMIT not honored in Rust) is a security-adjacent permissions issue but is OUT OF SCOPE for Phase 34 (D-16). | Existing: `definePermissions` in `zero-schema.ts` (already `ANYONE_CAN_DO_ANYTHING` for parity testing).                                                                            |
| V5 Input Validation   | yes — fuzz harness sends arbitrary AST JSON to zero-cache; harness must NOT crash either side                                                          | Existing: zero-cache validates AST shape at the protocol boundary; if RS panics on malformed AST, the harness catches via WS error handler (`harness-advance-coverage.ts:380-385`). |
| V6 Cryptography       | no                                                                                                                                                     | No crypto in scope.                                                                                                                                                                 |
| V7 Error Handling     | yes — divergences must be REPORTED, never SILENCED                                                                                                     | `harness-coverage.ts::describeError` already does this; D-20 says even unfixable divergences become regression tests.                                                               |
| V12 Files & Resources | yes — `tools/ivm-parity/seed-extras.sql` and replica DB files                                                                                          | Existing: SKILL.md hard rule #2 (additive only); replica wipe procedure documented.                                                                                                 |

### Known Threat Patterns for fuzz harness

| Pattern                                                 | STRIDE                                  | Standard Mitigation                                                                                                                                               |
| ------------------------------------------------------- | --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Hand-crafted malicious AST crashes one side             | DoS                                     | Harness wraps each iteration in try/catch; treat WS errors as `error` outcome (`harness-coverage.ts:60-62`); fast-check's `fc.assert` catches throws and shrinks. |
| Resource exhaustion (PG conn pool, sockets)             | DoS                                     | `MAX_PARALLEL=8` cap; `cleanupDb` retry loop; documented troubleshooting.                                                                                         |
| Stale replica file masks correctness regression         | Tampering (data integrity)              | Replica wipe + restart cache documented in README.md. Phase 34 schema changes MUST trigger this procedure.                                                        |
| Allow-list over-suppresses real divergences             | Information Disclosure (silent failure) | Per-canonical-key match (not pattern); each entry has explicit `phase35Issue` reference; reviewer checks during plan-check.                                       |
| Test-introduced rows leak into baseline state           | Tampering                               | `MUTATIONS[].undo` pattern; `cleanupDb` retry-verify loop; checked into `harness-advance-coverage.ts:551-608`.                                                    |
| B11 fix exposes prev-snapshot path with stale data race | Tampering (correctness)                 | Lifecycle assertion in TS (`set_prev_snapshot` MUST happen before `swap_snapshot`); Rust unit test verifies sequence.                                             |

---

## Sources

### Primary (HIGH confidence)

- `tools/ivm-parity/ast-fuzz.ts:1-972` — BFS enumerator, `adaptSchema`, schema-as-data pattern. [VERIFIED]
- `tools/ivm-parity/harness-coverage.ts:1-558` — hydrate sweep pattern, multiset diff. [VERIFIED]
- `tools/ivm-parity/harness-advance-coverage.ts:1-700` — advance batch model, MUTATIONS[], cleanupDb. [VERIFIED]
- `tools/ivm-parity/zero-schema.ts:1-384` — current 13-table schema. [VERIFIED]
- `tools/ivm-parity/schema.sql:1-143` — current PG DDL. [VERIFIED]
- `tools/ivm-parity/seed-extras.sql:1-59` — additive seed pattern. [VERIFIED]
- `tools/ivm-parity/SKILL.md` and `README.md` — hard rules and run procedure. [VERIFIED]
- `tools/ivm-parity/PARITY_STATUS.md` (in `/Users/kartik.parsoya/Documents/Zero/mono-rs/PARITY_STATUS.md`) — 1079/1084 OK, 5 known divergences. [VERIFIED]
- `tools/ivm-parity/divergence_report.md:1-80` — divergence categorization template. [VERIFIED]
- `packages/zql/src/builder/builder.ts:280-356, 611-647` — TS canonical pipeline construction (B1, B3 specs). [VERIFIED — same as upstream `/private/tmp/ivm-parity-ts-ref` for the relevant lines]
- `packages/zql/src/ivm/take.ts:55-105, 219, 710-757` — TS Take partition_key semantics (B3 spec). [VERIFIED]
- `packages/zqlite-rs/src/ast_to_config.rs:200-265, 316-349` — Rust AST→config translator (B1, B3 fix sites). [VERIFIED]
- `packages/zero-ivm-rs/src/exists_op.rs:139-300` — Rust Exists operator (B2 fix site). [VERIFIED]
- `packages/zero-ivm-rs/src/take_op.rs:1-150, 300-400` — Rust Take (B3 secondary fix site). [VERIFIED]
- `packages/zqlite-rs/src/advance.rs:464-578, 1316-1320, 1856-1896` — Rust advance, descendant SQL, swap_snapshot (B11 fix site). [VERIFIED]
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:1990-2110` — mono-rs streaming advance with swapSnapshot at 2081. [VERIFIED]
- `/private/tmp/ivm-parity-ts-ref/packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:1530-1612` — upstream TS reference for #rustAdvance and snapshot lifecycle (B11 spec). [VERIFIED]
- `.planning/IVM-PORT-AUDIT-DEEP.md` — full audit including B1/B2/B3/B11 root-cause analyses. [VERIFIED]
- `.planning/IVM-PORT-AUDIT.md` — original audit including Risk #1 (same root as B3). [VERIFIED]
- `.planning/REQUIREMENTS.md:67-69` — FUZZ-01, FUZZ-02 spec. [VERIFIED]
- `.planning/ROADMAP.md:139-156` — Phase 34 success criteria. [VERIFIED]
- `apps/zbugs/shared/schema.ts:1-200` — production-shape density target. [VERIFIED]
- `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts:1-150` — existing fast-check pattern. [VERIFIED]
- `packages/zero-cache/src/services/view-syncer/dual-executor.ts:283` — `compareChanges` primitive. [CITED via `IVM-PORT-AUDIT-DEEP.md`]

### Secondary (MEDIUM confidence)

- fast-check 3.23.2 capabilities (combinators, `fc.letrec`, default shrinker, `fc.assert.numRuns`). [VERIFIED via `node_modules/fast-check/package.json` + existing usage in repo]
- Phase 33 bench results pattern (`.planning/milestones/v5.0-bench-results.md` — append-only). [CITED via Phase 33 33-VALIDATION.md]
- `MUTATIONS[]` 17-step block content and ordering (`harness-advance-coverage.ts:68-196`). [VERIFIED]

### Tertiary (LOW confidence — single source / inferred)

- A1 (Zero replicates BIGINT as `string()`): inferred from B8/B9 audit findings. Marked in §Assumptions Log; planner should verify by checking `packages/zero-cache/src/services/replicator/`.

---

## Metadata

**Confidence breakdown:**

- Standard stack: HIGH — fast-check 3.23.2 verified installed; existing usage patterns in two test files confirm idioms.
- Track 2 fix specifications (B1/B2/B3/B11): HIGH — all four cite specific TS file:line and current Rust file:line, verified by reading both. B11 has implementation choice (Option A vs B) flagged as Open Question.
- Architecture & two-process harness reuse: HIGH — every reusable primitive cited by file:line.
- Schema extension layout: MEDIUM — column additions are concrete, but interaction between Zero's typed schema (`string()`/`number()`) and PG types (`JSONB`/`NUMERIC`/`TIMESTAMPTZ`) flagged as A1 [ASSUMED].
- CI budget compliance: MEDIUM — math projects to 100-200s for 1000 iterations based on existing 221s for 1084 ASTs; but actual depends on schema size (FUZZ-02 grows it) and BATCH_SIZE tuning. Probing task recommended.
- Common pitfalls: HIGH — derived from existing run logs, troubleshooting docs, and direct reading of harness code.

**Research date:** 2026-04-29

**Valid until:** 2026-05-29 (30 days — codebase moves quickly during a port; re-validate Track 2 fix sites if Phase 30 follow-ups land before Phase 34 begins).
