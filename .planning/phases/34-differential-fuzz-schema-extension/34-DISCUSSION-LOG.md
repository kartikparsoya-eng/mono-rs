# Phase 34: Differential Fuzz + Schema Extension - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-29
**Phase:** 34-differential-fuzz-schema-extension
**Areas discussed:** Tooling location, fast-check integration mode, CI integration, deep-audit hardening scope, schema extension breadth, track ordering

---

## Tooling Location

| Option                       | Description                                                                                                    | Selected |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------- | -------- |
| Option A — Two-cache harness | Extend `tools/ivm-parity/`'s existing two-process setup. Reuses real `zero-cache` binaries on shared Postgres. | ✓        |
| Option B — In-process vitest | New `random-ast-parity.fuzz.test.ts` importing TS as a library and calling Rust napi side-by-side.             |          |

**User's choice:** Option A — "lets go with A".
**Notes:** The user identified `tools/ivm-parity/` already exists and is finding bugs (1079/1084 parity, 5 known divergences). Option B was rejected because mono-rs's in-process `tsAdvance` is necessarily empty (no surviving TS pipeline state), so a library-import oracle has the same handicap.

## Deep-Audit Reframing

After reading `.planning/IVM-PORT-AUDIT-DEEP.md` (175 lines, 14 new findings beyond the original 6), the discussion expanded to include hardening scope.

| Option                              | Description                                                                 | Selected |
| ----------------------------------- | --------------------------------------------------------------------------- | -------- |
| (i) Just the harness                | Build fuzzer; divergences spill to Phase 35+.                               |          |
| (ii) Harness + targeted fixes first | Fix B1/B2/B3/B11 BLOCKING items, then run fuzz against known-good baseline. |          |
| (iii) Harness + parallel fix track  | Fuzz and fix concurrently. Two parallel tracks landing in one phase.        | ✓        |

**User's choice:** (iii).
**Notes:** User added: "make sure nothing regresses, dont hack your way out find root cause and actual fixes refer to TS IVM for spec." This shapes D-17 (TS-as-spec discipline) and D-18 (no regressions gate).

## fast-check Integration Mode

| Option                                  | Description                                                                                            | Selected |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------ | -------- |
| Replace BFS with fast-check entirely    | Single fuzz mode, simpler.                                                                             |          |
| Add fast-check as parallel mode         | BFS keeps deterministic regression coverage; fast-check explores random space. Both run in `npm test`. |          |
| Hybrid: BFS-seed + fast-check shrinking | BFS produces seed corpus; fast-check shrinks new failures.                                             | ✓        |

**User's choice:** Hybrid (default applied — user said "rest proceed with default").
**Notes:** Hybrid wins on "no regressions" criterion: BFS keeps deterministic regression baseline, fast-check adds shrinking exploration.

## CI Integration Depth

| Option                                            | Description                                  | Selected |
| ------------------------------------------------- | -------------------------------------------- | -------- |
| GitHub Action with PG service container, every PR | Strongest gate, slowest CI.                  |          |
| Phase gate / pre-merge local script               | Fastest CI, depends on dev discipline.       |          |
| Nightly scheduled run                             | Catches drift without blocking PRs.          |          |
| **Defer entirely (out of phase)**                 | Build runnable harness only; automate later. | ✓        |

**User's choice:** "lets not do CI intergration for now."
**Notes:** Captured as D-22. Rationale: build the tool, prove it finds bugs, fix the bugs, THEN automate. Don't ship a CI gate that knowingly fails.

## Open Divergence Handling

| Option                                     | Description                                                                                                          | Selected |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- | -------- |
| Fix all 5 + new findings in Phase 34       | Bigger phase, ships v5.0 with full parity.                                                                           |          |
| Document and assert-skip                   | Phase 34 stays harness-only, divergences carry to v6.0.                                                              |          |
| Skip allow-list, fix in v6.0               | Known issue, transparent.                                                                                            |          |
| **Folded into deep-audit hardening track** | The 5 catalogued divergences are not the same as the 14 deep-audit findings; partial fix via Track 2 (B1/B2/B3/B11). | ✓        |

**User's choice:** Folded into Track 2 (default applied).
**Notes:** The 5 catalogued divergences in `PARITY_STATUS.md` map to deeper structural work — 2× FlippedJoin (B7), 2× scalar EXISTS (separate), 1× CSQ alias dup (won't-fix). These are NOT in Track 2's B1/B2/B3/B11 scope. Phase 35 will address.

## FUZZ-02 Schema Extension Breadth

| Option           | Description                                                                 | Selected |
| ---------------- | --------------------------------------------------------------------------- | -------- |
| Minimal viable   | One column of each type, NULL semantics, no composite/array.                |          |
| Production-shape | Match `apps/zbugs/shared/schema.ts` density, full composite/array coverage. | ✓        |
| Adversarial      | NaN, ∞, 0-length arrays, deeply-nested jsonb.                               |          |

**User's choice:** Production-shape (default applied — user said "rest proceed with default").
**Notes:** Adversarial cases deferred to Phase 35 unless production-shape fuzz hits them organically. Reference: zbugs is the project's own Zero reference application; its schema is the concrete target for "production density."

## Track Ordering

| Option                                  | Description                                                                                                            | Selected |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | -------- |
| Sequential: fix first, fuzz after       | Simpler, but slower phase.                                                                                             |          |
| Sequential: fuzz first, fix after       | User said "concurrent" — rejected.                                                                                     |          |
| **Parallel with synchronization point** | Track 1 (fuzzer) and Track 2 (fixes) develop in parallel; full-corpus fuzz waits for Track 2 to land before reporting. | ✓        |

**User's choice:** Parallel (default applied).
**Notes:** Captured as D-19. Track 1 can develop and self-test against existing schema in parallel with Track 2 fixes. Synchronization at the full-corpus run prevents the fuzzer from reporting against a known-broken baseline.

## Claude's Discretion

- **CD-01** — fast-check arbitrary structure (combinator composition, custom shrinkers vs default). Researcher/planner picks.
- **CD-02** — Harness fixture reuse strategy (Postgres truncate-and-reseed vs deterministic schema-per-iteration).
- **CD-03** — Whether existing `synth-p6.ts`/`synth-p7.ts` AST synthesizers fold into the new fast-check generator.
- **CD-04** — Allow-list format in the harness for deferred items.

## Deferred Ideas

- B5–B14 deep-audit findings (Phase 35 — see CONTEXT.md `<deferred>` for rationale per item).
- Adversarial schema cases (NaN, ∞, 0-length arrays).
- CI integration of the fuzz suite (post-divergence-drain).
- 5 catalogued divergences in `PARITY_STATUS.md` mapping to FlippedJoin/scalar EXISTS/CSQ alias work.

## Out-of-Cycle User Directives Captured

- **TS as spec** — D-17. Every Track 2 commit cites TS file:line.
- **No shortcuts** — D-18 requires Rust unit test + differential test + full suite green for every fix.
- **No perf compromise** — D-18 includes Phase 33 bench re-run gate (TTFB ≤1.5×, MemPeak ≥4×).
- **Auto-advance preference** — User reiterated "always do gsd next and continue implementing." Workflow proceeds via /gsd-next chain after this CONTEXT.md is written.
