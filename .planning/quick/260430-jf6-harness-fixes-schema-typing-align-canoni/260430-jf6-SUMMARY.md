---
phase: quick-260430-jf6
plan: 01
subsystem: tools/ivm-parity
type: execute
tags: [ivm-parity, fuzz, schema-typing, divergence-catalog, bisect-hygiene]
dependency_graph:
  requires: []
  provides:
    - ivm-parity-live-fuzz-unblock
    - ivm-parity-canonical-row-diff
    - ivm-parity-divergence-triage-catalog
  affects:
    - tools/ivm-parity/zero-schema.ts
    - tools/ivm-parity/harness-fuzz.ts
    - tools/ivm-parity/random-ast-fuzz.ts
    - tools/ivm-parity/multi-seed-fuzz.sh
    - tools/ivm-parity/categorize-divergences.ts
    - tools/ivm-parity/PARITY-DIVERGENCES-CATALOG.md
tech_stack:
  added: []
  patterns:
    - canonical-key JSON serialization (sorted-key recursion) for cross-runtime row equality
    - env-var feature flag (FUZZ_KEEP_GOING) for opt-in fuzz enumeration semantics
key_files:
  created:
    - tools/ivm-parity/multi-seed-fuzz.sh
    - tools/ivm-parity/categorize-divergences.ts
    - tools/ivm-parity/PARITY-DIVERGENCES-CATALOG.md
  modified:
    - tools/ivm-parity/zero-schema.ts
    - tools/ivm-parity/harness-fuzz.ts
    - tools/ivm-parity/random-ast-fuzz.ts
decisions:
  - 'Land three independently-bisectable commits in foundation-first order: schema-align (fix) -> canonical-key (fix) -> KEEP_GOING+tooling (feat). Preserves bisect value if any single commit must later be reverted.'
  - 'Stage with explicit pathspec only (no `git add -A`). 9 OUT-OF-SCOPE untracked files (golden-harness scaffolding, prod_*_asts.json, _gen_*.py) deliberately left untracked — outside scope of this plan.'
metrics:
  duration_minutes: ~3
  completed_date: 2026-04-30
  tasks_completed: 3
  files_changed: 6
  commits_landed: 3
---

# Phase quick-260430-jf6 Plan 01: Harness Fixes (Schema Typing + Canonical Diff + KEEP_GOING) Summary

Three atomic, bisect-friendly commits inside `tools/ivm-parity/` landing schema typing alignment (live-fuzz unblocker), canonical-key row diff (false-positive eliminator), and FUZZ_KEEP_GOING env flag plus 23-shape divergence catalog.

## What Was Built

Three commits on `main`, in bisect order:

| #   | SHA         | Type | Scope      | Subject                                                                |
| --- | ----------- | ---- | ---------- | ---------------------------------------------------------------------- |
| 1   | `8e49ef5ce` | fix  | ivm-parity | align zero-schema.ts column types with upstream pg-data-type map       |
| 2   | `4fc1fdff8` | fix  | ivm-parity | canonical-key diff in harness-fuzz.ts                                  |
| 3   | `571dae669` | feat | ivm-parity | FUZZ_KEEP_GOING + breadth-survey tooling + 23-shape divergence catalog |

Pre-plan baseline HEAD: `a2f3f00ca` (`docs(quick-260430-gqe): wire parity check into streaming + drop bench dead-code path`).

### Commit 1 — Schema typing alignment (`8e49ef5ce`)

Single file: `tools/ivm-parity/zero-schema.ts` (+19 / −12).

Aligned column types with the canonical map at `packages/zero-cache/src/types/pg-data-type.ts:43-66`:

- `events.occurredAt`, `events.processedAt` → `number()` (was `string()`)
- `event_tags.amount` → `number()`
- `big_id_records.createdAt` → `number()`
- `events.metadataJson`, `event_tags.auditTrail` → `json()`
- Added `json` import.

This unblocks Phase 34 live fuzz mode (previously `SchemaVersionNotSupported` on every connect).

### Commit 2 — Canonical-key row diff (`4fc1fdff8`)

Single file: `tools/ivm-parity/harness-fuzz.ts` (+37 / −19).

`rowsHash` and `diffRows` previously hashed via `JSON.stringify`, which preserves insertion order. TS and Rust IVM emit row objects with different column orders, so semantically-equal rows produced different hashes — every prior `value_level_diff` fuzz entry was likely a false positive. Added `canonicalize()` helper that recursively sorts object keys before serializing. Public API surface unchanged.

### Commit 3 — KEEP_GOING + breadth tooling + catalog (`571dae669`)

Four files (+2306 / −6):

- `tools/ivm-parity/random-ast-fuzz.ts` (+18 / −6) — `FUZZ_KEEP_GOING=1` env var disables fast-check's stop-on-first-failure so a single run enumerates every distinct divergence. Default behavior unchanged (CI gating preserved).
- `tools/ivm-parity/multi-seed-fuzz.sh` (new, mode 100755) — runs `random-ast-fuzz` across 15 seeds, aggregates unique canonicalKeys.
- `tools/ivm-parity/categorize-divergences.ts` (new) — parses fuzz logs, dedupes by canonicalKey, emits `PARITY-DIVERGENCES-CATALOG.md` grouped by AST features.
- `tools/ivm-parity/PARITY-DIVERGENCES-CATALOG.md` (new) — current snapshot: 23 unique shapes across 4 buckets (A: nested-OR with EXISTS = 8; B: OR-of-AND-of-EXISTS = 4; D: simple OR-with-EXISTS = 10; G: no-EXISTS divergence = 1). Note the breakdown 22 OR-with-EXISTS + 1 NOT LIKE in the commit message body collapses A+B+D into one family-of-22 against the 1 G-bucket entry.

## Verification Gates

Each gate ran **before** staging the corresponding commit. All exited 0.

| Commit | Gate command                                                                                                                           | Exit | Observation                                                                                                             |
| ------ | -------------------------------------------------------------------------------------------------------------------------------------- | ---- | ----------------------------------------------------------------------------------------------------------------------- |
| 1      | `cd tools/ivm-parity && npm run deploy-schema` (resolves to `npx zero-deploy-permissions -p zero-schema.ts --output-file schema.json`) | 0    | "Wrote sql permissions to schema.json"; only deprecation notices in stderr; no `SchemaVersionNotSupported`              |
| 2      | `FUZZ_NUM_RUNS=50 npx tsx tools/ivm-parity/random-ast-fuzz.ts --dry-run`                                                               | 0    | `random-ast-fuzz: ok=50 allowed=0 unexpected=0 error=0`; module-load and diff-callsite exercised cleanly                |
| 3a     | `FUZZ_NUM_RUNS=20 FUZZ_KEEP_GOING=1 npx tsx tools/ivm-parity/random-ast-fuzz.ts --dry-run`                                             | 0    | `random-ast-fuzz: ok=20 allowed=0 unexpected=0 error=0`; `FUZZ_KEEP_GOING=1` env-var branch loads without throwing      |
| 3b     | `npx tsx tools/ivm-parity/categorize-divergences.ts`                                                                                   | 0    | "Wrote PARITY-DIVERGENCES-CATALOG.md with 23 unique shapes across 4 buckets" — script handled `/tmp` log inputs cleanly |

The actual `deploy-schema` command resolves identically to what the plan asserted (verified via `tools/ivm-parity/package.json` `scripts.deploy-schema`).

## Cross-Cut Verification (post-all-commits)

```
git log --format='%s' a2f3f00ca..HEAD
  → feat(ivm-parity): FUZZ_KEEP_GOING + breadth-survey tooling + 23-shape divergence catalog
  → fix(ivm-parity): canonical-key diff in harness-fuzz.ts
  → fix(ivm-parity): align zero-schema.ts column types with upstream pg-data-type map

git diff --name-only a2f3f00ca..HEAD | grep -v '^tools/ivm-parity/'
  → (empty) — OK, no leaks outside tools/ivm-parity/

git status --short tools/ivm-parity/ | grep '^??'
  → 9 OUT-OF-SCOPE files still untracked (correct — bundling boundary held)
```

## OUT-OF-SCOPE Files (deliberately left untracked)

Per `<working_tree_inventory>` in the plan, these 9 untracked files remain untracked and are NOT part of any of the three commits:

- `tools/ivm-parity/_gen_prod_gap_asts.py`
- `tools/ivm-parity/_gen_prod_shape_asts.py`
- `tools/ivm-parity/harness-capture-golden.ts`
- `tools/ivm-parity/harness-golden-shared.ts`
- `tools/ivm-parity/harness-replay-golden.ts`
- `tools/ivm-parity/prod_gap_asts.json`
- `tools/ivm-parity/prod_shape_asts.json`
- `tools/ivm-parity/run-capture-golden.sh`
- `tools/ivm-parity/run-replay-golden.sh`

These belong to a separate (golden-harness scaffolding) line of work and will be triaged independently — bundling them here would have polluted bisect history for the schema-typing / canonical-diff / KEEP_GOING changes.

## Deviations from Plan

None. The plan executed exactly as written. Working tree state at start matched the inventory exactly; all gates passed first try; no deviation rules (Rule 1/2/3/4) triggered.

## Follow-up Items

1. **Re-triage allow-list `value_level_diff` entries under canonical comparator.** Per commit 2's body: `fuzz_00168`, `fuzz_00408`, `fuzz_00841` in `PARITY_STATUS` are expected to drop out now that diff uses sorted-key serialization. Re-run the fuzz suite without those entries on the allow-list and confirm they pass; remove from `PARITY_STATUS` if so.
2. **Triage 23-shape divergence catalog.** `tools/ivm-parity/PARITY-DIVERGENCES-CATALOG.md` is now the actionable list for the next correctness pass:
   - 22 OR-with-EXISTS variants (likely B5 short-circuit family)
   - 1 NOT LIKE case-sensitivity divergence (likely AUDIT-01 incomplete on the inverted predicate path)
3. **Decide fate of OUT-OF-SCOPE golden-harness scaffolding.** The 9 untracked files (`harness-{capture,replay,golden-shared}.ts`, `_gen_prod_*.py`, `prod_*_asts.json`, `run-{capture,replay}-golden.sh`) are unfinished and need either a follow-up plan to land them or explicit removal.

## Constraints Honored

- No `--amend` used.
- No `--no-verify` used.
- No `git add -A` / `git add .` / `git add tools/ivm-parity/` used — every stage was explicit pathspec.
- No file outside `tools/ivm-parity/` modified.
- No commit captured an OUT-OF-SCOPE file.
- `multi-seed-fuzz.sh` committed with mode `100755` (executable bit preserved).
- ROADMAP.md / STATE.md / SUMMARY.md NOT committed by this executor (orchestrator handles in steps 7-8).

## Self-Check: PASSED

- [x] Commit `8e49ef5ce` exists and contains only `tools/ivm-parity/zero-schema.ts`
- [x] Commit `4fc1fdff8` exists and contains only `tools/ivm-parity/harness-fuzz.ts`
- [x] Commit `571dae669` exists and contains exactly the four declared paths (`random-ast-fuzz.ts`, `multi-seed-fuzz.sh`, `categorize-divergences.ts`, `PARITY-DIVERGENCES-CATALOG.md`)
- [x] `git log --oneline -3` matches expected bisect order
- [x] `git diff --name-only a2f3f00ca..HEAD | grep -v '^tools/ivm-parity/'` is empty
- [x] OUT-OF-SCOPE untracked files still untracked (9 files)
- [x] SUMMARY.md exists at declared path
