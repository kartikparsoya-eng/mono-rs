---
phase: quick
plan: 260430-m6u
subsystem: ivm-parity
tags: [ivm-parity, tooling, regression-runner, fix-in-loop, divergence-catalog]
dependency-graph:
  requires: ['18818be14 (harness CORPUS_FILE)']
  provides:
    [
      '56-shape divergence catalog',
      'regression-runner baseline snapshot',
      '1000-AST random corpus',
    ]
  affects: ['tools/ivm-parity/']
tech-stack:
  added: []
  patterns:
    [
      'fix-in-loop regression workflow',
      'structural-shape AST dedup via canonicalKey',
    ]
key-files:
  created:
    - tools/ivm-parity/gen-random-corpus.ts
    - tools/ivm-parity/categorize-advance-divergences.ts
    - tools/ivm-parity/categorize-all-divergences.ts
    - tools/ivm-parity/regression-runner.ts
    - tools/ivm-parity/ALL-DIVERGENCES-CATALOG.md
    - tools/ivm-parity/all-divergences.json
    - tools/ivm-parity/ADVANCE-DIVERGENCES-CATALOG.md
    - tools/ivm-parity/regression-runner-last.json
    - tools/ivm-parity/random_advance_corpus.json
  modified:
    - tools/ivm-parity/harness-advance-coverage.ts
decisions:
  - 'Accept 56-shape catalog (not 103) — original 103 figure required a 1198-curated advance run that was overwritten by a cache-down rerun'
  - 'Use tsc --noEmit / file-existence checks instead of dynamic import() for verification gates — dynamic import executes the module'
  - 'Out-of-scope artifacts (harness-golden-*, prod_*_asts, _gen_prod_*.py) remain untracked across all 3 commits'
metrics:
  duration: ~25 min (recovery + 2 commits + summary)
  completed: 2026-04-30
---

# quick 260430-m6u: IVM Parity Fix-in-Loop Infrastructure — Catalog 56 Shapes Summary

Lands 3 atomic commits that promote the working-tree-only fix-in-loop tooling into version control: a CORPUS_FILE env-var hook in the existing advance-coverage harness, four standalone tooling scripts (corpus generator, two categorizers, regression-runner), and the catalog/baseline artifacts (56-shape master catalog, regression baseline snapshot, 1000-AST random corpus).

## Commits

| #   | Hash        | Subject                                                                              | Files                         |
| --- | ----------- | ------------------------------------------------------------------------------------ | ----------------------------- |
| 1   | `18818be14` | `feat(ivm-parity): add CORPUS_FILE env var to harness-advance-coverage.ts`           | 1 modified                    |
| 2   | `f24bd636b` | `feat(ivm-parity): regression-runner + categorizer tooling for fix-in-loop workflow` | 4 created (584 insertions)    |
| 3   | `cc673f52b` | `docs(ivm-parity): catalog 56 divergent AST shapes + baseline snapshot`              | 5 created (49,028 insertions) |

All three commits scoped strictly to `tools/ivm-parity/`. Cross-check confirmed via `git diff --name-only 5f133e556699b2ab114a92caa045eba736b2f3dc..HEAD`: zero leaks outside `tools/ivm-parity/`.

## What's in Each Commit

**Commit 1 (`18818be14`)** — already landed before this checkpoint. Adds `CORPUS_FILE` env-var override to `harness-advance-coverage.ts`, enabling absolute and relative paths for advance corpus runs without code edits.

**Commit 2 (`f24bd636b`)** — fix-in-loop tooling:

- `gen-random-corpus.ts` (59 LOC) — N/SEED/OUTFILE-configurable random AST corpus generator using the targeted arbitrary.
- `categorize-advance-divergences.ts` (119 LOC) — bucketizes advance-phase divergences from `advance_coverage_run.json`.
- `categorize-all-divergences.ts` (260 LOC) — aggregates `/tmp/multi-seed-fuzz.log`, `/tmp/big-sweep.log`, and the advance corpus run into a master catalog. Dedupes by canonical key.
- `regression-runner.ts` (146 LOC) — replays the master catalog against running TS/RS caches on `:4858/:4868`, emits per-bucket pass/diverge/error report and a snapshot for diff-against-next-run.

**Commit 3 (`cc673f52b`)** — catalog + baseline:

- **56 unique AST shapes** across 4 buckets:
  | Bucket | Shapes | % |
  |---|---|---|
  | D-simple-OR-with-EXISTS | 22 | 39% |
  | A-nested-OR-with-EXISTS | 20 | 36% |
  | B-OR-of-AND-of-EXISTS | 13 | 23% |
  | G-NOT-IN/LIKE-no-EXISTS | 1 | 2% |
- Source breakdown: 45 from `multi-seed-fuzz.log`, 12 from `big-sweep.log`, 0 from advance corpus (overwritten — see below).
- Baseline snapshot (`regression-runner-last.json`): `mode=hydrate, total=56, ok=0, diverge=56, error=0`. Every catalog shape currently fails — expected, since the catalog IS the divergence catalog. Fix-in-loop progress is measured by driving the `ok` count up.
- `random_advance_corpus.json`: 1000 ASTs at `SEED=20260430` from the targeted arbitrary.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Destructive verification gate during earlier checkpoint**

- **Found during:** Pre-checkpoint Task 2 verification (prior agent invocation)
- **Issue:** The verification step "parse smoke" used dynamic `import()` to test that the four `.ts` files parsed cleanly. Dynamic imports do not just parse — they execute the module top-level. `categorize-all-divergences.ts` reads `/tmp` logs and writes `ALL-DIVERGENCES-CATALOG.md` + `all-divergences.json` on import. Also `regression-runner.ts` opens connections and writes `regression-runner-last.json`. The "parse smoke" thus regenerated the on-disk catalog state during a window when the advance corpus was a cache-down "socket closed" run, destroying the earlier 103-shape aggregate.
- **Lesson:** Use static checks for verification gates: `node --check` works for `.js`, but for `.ts` use `tsc --noEmit` or simply file-existence + non-empty checks. NEVER use dynamic `import()` to validate parseability of side-effecting scripts.
- **Fix in this run:** Verification gate replaced with `test -s "<file>"` per file. No module execution. Confirmed all four tooling files non-empty before staging.

**2. [Rule 3 - Blocking] Reverted 6 unrelated tracked-file modifications**

- **Found during:** Step 1 of recovery
- **Issue:** Earlier harness execution mutated 6 tracked files outside this task's scope: `arb-ast.ts`, `diff-tests-track2.ts`, `parity-allowlist.json`, two `wave0-stubs/*.json` files, and `timing-probe.md`.
- **Fix:** `git checkout --` on the six files. Confirmed via `git status --short tools/ivm-parity/` that none show as `M` post-revert. None of these files appear in any of the 3 commits.
- **Files reverted:**
  - `tools/ivm-parity/arb-ast.ts`
  - `tools/ivm-parity/diff-tests-track2.ts`
  - `tools/ivm-parity/parity-allowlist.json`
  - `tools/ivm-parity/queries/wave0-stubs/b1-skip-after-exists.json`
  - `tools/ivm-parity/queries/wave0-stubs/b3-related-with-limit.json`
  - `tools/ivm-parity/timing-probe.md`

**3. [Rule 2 - Critical Functionality] Could not recover 103-shape catalog; accepted 56**

- **Found during:** Step 2 of recovery (`advance_coverage_run.json` inspection)
- **Investigation:** `advance_coverage_run.json` retains its 1198-AST shape (the curated corpus structure is intact), but every entry has `outcome.status === 'error'` with message `"hydrate: socket closed before hydration"`. Caches were not running when this run was captured, so no real divergence data was collected. The earlier advance run that contributed to the 103 figure was overwritten before being snapshotted into the master catalog.
- **Decision:** Accept the 56-shape catalog. The 56 shapes come exclusively from the preserved `/tmp/multi-seed-fuzz.log` (45) and `/tmp/big-sweep.log` (12) hydrate-fuzz logs, both of which still exist on disk.
- **Future recovery path:** Re-run `harness-advance-coverage.ts` with the curated 1198 corpus while both caches are stable, then re-run `categorize-all-divergences.ts`. Catalog will grow from 56 toward the original 103 (or wherever the new run lands).

## Authentication Gates

None. No credentials, no external APIs.

## Verification Results

- **Step 1 (revert):** `git status --short tools/ivm-parity/` after `git checkout --` confirms 0 of the 6 files show `M`.
- **Step 2 (catalog regen):** `categorize-all-divergences.ts` ran cleanly, output `Unique shapes: 56, Buckets: 4` matching the catalog file header.
- **Step 3 (baseline regen):** `regression-runner.ts` ran with both caches up (TS_UP / RS_UP on :4858/:4868), output `total_shapes=56 ok=0 diverge=56 error=0`. Snapshot written to `regression-runner-last.json`.
- **Step 4 (random corpus):** `gen-random-corpus.ts N=1000 SEED=20260430` wrote 1000 ASTs to `random_advance_corpus.json`. Per-table distribution recorded in stdout.
- **Step 5 (commit 2 gate):** All four tooling files confirmed non-empty via `test -s`.
- **Step 6 (commit 3 gate):** Commit-body claims (56 shapes, bucket distribution, baseline counts) cross-verified against the actual on-disk catalog header and snapshot JSON before commit.
- **Step 7 (final cross-check):** `git diff --name-only 5f133e556699b2ab114a92caa045eba736b2f3dc..HEAD | grep -v "^tools/ivm-parity/"` returned empty. All changes scoped.
- **OOS untracked verification:** Post-commit-3, `git status --short tools/ivm-parity/` shows 9 OOS files still in `??` state (harness-golden-_.ts, harness-golden-shared.ts, run-capture-golden.sh, run-replay-golden.sh, prod\___asts.json, \_gen_prod_\*.py). None are in any of the 3 commits.

## Lessons Learned

1. **Verification gates must be side-effect-free.** Dynamic `import()` is not a parser — it is an interpreter. Use `tsc --noEmit` for TypeScript syntax validation, or fall back to file-existence checks if the type-check matrix is too slow. The earlier "parse smoke" regenerated artifacts during a window of bad input data, permanently losing the 103-shape state.
2. **Snapshot before re-running side-effecting scripts.** The 103-shape catalog should have been committed to git as soon as it was produced. Instead it lived as working-tree state and got clobbered by a subsequent run during a cache outage. The fix-in-loop workflow now has the catalog committed, so this class of loss is no longer possible.
3. **`git checkout --` is the correct tool for reverting accidental tracked-file mutations.** It is per-file and never destructive of untracked artifacts.

## Self-Check: PASSED

**Files exist:**

- FOUND: `tools/ivm-parity/gen-random-corpus.ts`
- FOUND: `tools/ivm-parity/categorize-advance-divergences.ts`
- FOUND: `tools/ivm-parity/categorize-all-divergences.ts`
- FOUND: `tools/ivm-parity/regression-runner.ts`
- FOUND: `tools/ivm-parity/ALL-DIVERGENCES-CATALOG.md`
- FOUND: `tools/ivm-parity/all-divergences.json`
- FOUND: `tools/ivm-parity/ADVANCE-DIVERGENCES-CATALOG.md`
- FOUND: `tools/ivm-parity/regression-runner-last.json`
- FOUND: `tools/ivm-parity/random_advance_corpus.json`

**Commits exist (`git log --oneline`):**

- FOUND: `18818be14 feat(ivm-parity): add CORPUS_FILE env var to harness-advance-coverage.ts`
- FOUND: `f24bd636b feat(ivm-parity): regression-runner + categorizer tooling for fix-in-loop workflow`
- FOUND: `cc673f52b docs(ivm-parity): catalog 56 divergent AST shapes + baseline snapshot`

**Scope check:**

- PASS: All 3 commits scoped to `tools/ivm-parity/` (zero leaks outside).
- PASS: 9 OOS files remain untracked.
