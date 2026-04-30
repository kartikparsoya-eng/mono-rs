---
phase: 260430-mu2-ivm-parity-arb-fix
plan: 01
type: execute
status: complete
completed: 2026-04-30
commits:
  - hash: bf964a585
    subject: 'fix(ivm-parity): scope arb-ast.ts simple+csq arbitraries to root table'
    files: [tools/ivm-parity/arb-ast.ts]
  - hash: a90cdfcf2
    subject: 'docs(ivm-parity): regenerate catalog with fixed arb (56 → 6 real divergences)'
    files:
      - tools/ivm-parity/ALL-DIVERGENCES-CATALOG.md
      - tools/ivm-parity/all-divergences.json
      - tools/ivm-parity/regression-runner-last.json
files_changed: 4
out_of_scope_preserved: 9
base: d6b75515877d834695faa65525b00b36106d86e3
---

# Quick 260430-mu2: IVM Parity Arb Fix — Scope simple+csq to Root Table — Summary

## One-liner

Scoped `arb-ast.ts` simple/csq arbitraries to the AST root table (eliminating cross-table column arb noise) and regenerated the divergence catalog (56 → 6 real B5-family shapes), establishing a clean 0/6 hydrate-only baseline for the upcoming OrExistsOperator fix.

## What landed

Two atomic, bisect-friendly commits on `main` since base `d6b75515877d834695faa65525b00b36106d86e3`:

| #   | Commit      | Subject                                                                         | Files                                                                                                                                  |
| --- | ----------- | ------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | `bf964a585` | `fix(ivm-parity): scope arb-ast.ts simple+csq arbitraries to root table`        | `tools/ivm-parity/arb-ast.ts`                                                                                                          |
| 2   | `a90cdfcf2` | `docs(ivm-parity): regenerate catalog with fixed arb (56 → 6 real divergences)` | `tools/ivm-parity/ALL-DIVERGENCES-CATALOG.md`, `tools/ivm-parity/all-divergences.json`, `tools/ivm-parity/regression-runner-last.json` |

Total: 2 commits, 4 files changed, all under `tools/ivm-parity/`.

## Why this ordering matters

The fix lands before its measured artifact. A future bisect can independently revert either:

- Commit 1 (the generator change) without losing the regenerated baseline data, or
- Commit 2 (the catalog regeneration) without losing the arb scoping fix.

## Pre-fix vs post-fix measurement

- **Pre-fix:** 56 unique divergence shapes — most were arb noise (cross-table column references like `event_tags WHERE processedAt IS NULL` where `processedAt` is a column on `events`, not `event_tags`).
- **Post-fix:** 6 unique shapes, all fast-check shrinks of the same B5 family — AND-inside-OR-of-EXISTS (the documented TODO at `ast_to_config.rs:587`).
- **Bucket distribution:** A-nested-OR-with-EXISTS = 5; D-simple-OR-with-EXISTS = 1.
- **Regression baseline:** 0/6 hydrate-only pass — clean, reproducible suite for the upcoming B5 fix.
- **Cache stability:** previously died after ~130 random ASTs; now clean for 2800+.

## OUT-OF-SCOPE files preserved untracked

All 9 unrelated working-tree files remain untracked after both commits:

```
?? tools/ivm-parity/_gen_prod_gap_asts.py
?? tools/ivm-parity/_gen_prod_shape_asts.py
?? tools/ivm-parity/harness-capture-golden.ts
?? tools/ivm-parity/harness-golden-shared.ts
?? tools/ivm-parity/harness-replay-golden.ts
?? tools/ivm-parity/prod_gap_asts.json
?? tools/ivm-parity/prod_shape_asts.json
?? tools/ivm-parity/run-capture-golden.sh
?? tools/ivm-parity/run-replay-golden.sh
```

These belong to a separate golden-replay / prod-AST-capture workstream and are intentionally not part of this commit pair.

## Verification (every gate green)

### Per-commit

- Commit 1 staging exclusivity: `git diff --cached --name-only` → `tools/ivm-parity/arb-ast.ts` only
- Commit 1 diff-tree: `git diff-tree --no-commit-id --name-only -r HEAD~1` → exactly `tools/ivm-parity/arb-ast.ts`
- Commit 2 staging exclusivity: 3-path sort matches expected list
- Commit 2 diff-tree: matches the 3 catalog/baseline files exactly
- Commit 2 JSON probes (using `node -e JSON.parse(...)` only — never `npx tsx -e import(...)` per m6u checkpoint lesson):
  - `all-divergences.json.uniqueShapes === 6` ✓
  - `regression-runner-last.json.total.diverge === 6` ✓

### Phase-level

- Commit ordering: `fix(ivm-parity)` lands at HEAD~1, `docs(ivm-parity)` at HEAD ✓
- `git diff --name-only d6b755158..HEAD | grep -v "^tools/ivm-parity/"` → empty ✓
- `git status --short tools/ivm-parity/ | grep -c "^??"` → 9 ✓
- No `.planning/` files modified by the executor ✓
- No `--no-verify`, no `--amend`, no `git add .` / `-A` / directory globs used ✓

## Constraints honored

- All staging used explicit per-file pathspecs (`git add <path1> <path2>`).
- No git hooks were skipped.
- No commit was amended; both commits are fresh.
- No source modifications were made beyond the staging-and-commit operation (working-tree changes existed at task start).
- All verification gates used `grep`, `test -s`, or `node -e "JSON.parse(...)"` patterns only — no `npx tsx -e "import(...)"` per the m6u checkpoint lesson.

## Deviations from plan

None — plan executed exactly as written.

## Self-Check

- `git rev-parse bf964a585`: present ✓
- `git rev-parse a90cdfcf2`: present ✓
- `tools/ivm-parity/arb-ast.ts` exists in HEAD~1 tree ✓
- `tools/ivm-parity/all-divergences.json` exists in HEAD tree ✓
- `tools/ivm-parity/ALL-DIVERGENCES-CATALOG.md` exists in HEAD tree ✓
- `tools/ivm-parity/regression-runner-last.json` exists in HEAD tree ✓

## Self-Check: PASSED
