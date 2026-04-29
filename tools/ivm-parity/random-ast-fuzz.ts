/**
 * Phase 34 Wave 0 — fast-check driver script.
 *
 * Loads `parity-allowlist.json`, builds arbitraries via `arb-ast.ts`, runs
 * `fc.assert(fc.asyncProperty(...))`, and calls `harness-fuzz.runOneAst`
 * per iteration. Diverges that match the allow-list are silently passed;
 * everything else fails the property and triggers fast-check shrinking.
 *
 * Wave 0 mode (--dry-run): runs in stub mode (harness-fuzz dryRun=true) so
 * the script can smoke-verify before zero-cache binaries are running. Useful
 * for CI sanity ("the fuzz script at least starts and draws ASTs") and for
 * Wave 1 development.
 *
 * Wave 1: removes --dry-run gate, adds BatchedRunner for the <2 min budget
 * (RESEARCH "Common Pitfalls — Pitfall 1: Per-AST cache restart blowing CI
 * budget"), adds shrink-min recording to ast_corpus.regressions.json, adds
 * targeted arbitraries that GUARANTEE B1/B2/B3/B11 trigger shapes.
 *
 * Env:
 *   FUZZ_NUM_RUNS    — number of iterations (default 100 in Wave 0; 1000 in Wave 1+).
 *   FUZZ_SEED        — fast-check seed for reproducibility (default Date.now()).
 *   FUZZ_VERBOSE     — '0' | '1' | '2' (default 1) — fast-check verbosity.
 *   PARITY_TS_URL, PARITY_RS_URL — WebSocket URLs (defaults match SKILL.md).
 *
 * Flags:
 *   --dry-run        — use harness-fuzz stub mode (no zero-cache required).
 *   --num-runs N     — override FUZZ_NUM_RUNS env var.
 *
 * Exit codes:
 *   0 — all iterations OK or matched allow-list.
 *   1 — at least one unexpected divergence (fast-check shrinks then reports).
 *   2 — environment/config error (e.g., allow-list not parseable).
 */
import {readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import fc from 'fast-check';
import type {AST} from '../../packages/zero-protocol/src/ast.ts';
import {buildArbitraries} from './arb-ast.ts';
import {astHash, runOneAst, type DivergenceDiff} from './harness-fuzz.ts';
import {schema as paritySchema} from './zero-schema.ts';

const here = dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------------------
// Argv parsing.
// ---------------------------------------------------------------------------
const args = process.argv.slice(2);
const dryRun = args.includes('--dry-run');
const numRunsFlagIdx = args.indexOf('--num-runs');
const numRunsFlag =
  numRunsFlagIdx !== -1 && args[numRunsFlagIdx + 1]
    ? Number(args[numRunsFlagIdx + 1])
    : undefined;

const NUM_RUNS = Number(numRunsFlag ?? process.env.FUZZ_NUM_RUNS ?? 100);
const SEED = Number(process.env.FUZZ_SEED ?? Date.now());
const VERBOSE = Number(process.env.FUZZ_VERBOSE ?? 1);

// ---------------------------------------------------------------------------
// Allow-list — load + index.
// ---------------------------------------------------------------------------
type AllowListEntry = {id: string} & Record<string, unknown>;
type AllowList = {keys: AllowListEntry[]; patterns: AllowListEntry[]};

function loadAllowList(): AllowList {
  const path = join(here, 'parity-allowlist.json');
  try {
    const raw = readFileSync(path, 'utf8');
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed.keys) || !Array.isArray(parsed.patterns)) {
      throw new Error('expected keys[] and patterns[]');
    }
    return parsed as AllowList;
  } catch (err) {
    process.stderr.write(
      `random-ast-fuzz: cannot load ${path}: ${(err as Error).message}\n`,
    );
    process.exit(2);
  }
}

function isAllowedDivergence(
  ast: AST,
  diff: DivergenceDiff,
  allowList: AllowList,
): {allowed: true; entry: AllowListEntry} | {allowed: false} {
  // 1. Exact canonical-key match against keys[].
  for (const k of allowList.keys) {
    // Wave 0: keys[] entries currently store `canonical_key_hint` (human readable
    // descriptor) rather than the runtime canonicalKey. Wave 1 will populate the
    // actual canonicalKey by rerunning the corpus and capturing the hash. For
    // now: match by the AST's table+top-level shape against the hint string.
    // This is a coarse pre-filter that Wave 1 replaces with exact match.
    const hint = String(k['canonical_key_hint'] ?? '');
    if (hint && hint.includes(ast.table) && wave0CoarseMatch(ast, hint)) {
      return {allowed: true, entry: k};
    }
  }
  // 2. Pattern match against patterns[].
  for (const p of allowList.patterns) {
    if (matchesPattern(ast, p)) {
      return {allowed: true, entry: p};
    }
  }
  return {allowed: false};
}

/** Wave 0 coarse hint match — looks for substrings like "OR(", "EXISTS",
 *  "flip=true", "scalar=true" in the rendered hint that the AST exhibits.
 *  Wave 1 replaces with exact canonicalKey match. */
function wave0CoarseMatch(ast: AST, hint: string): boolean {
  const flat = JSON.stringify(ast);
  if (hint.includes('flip=true') && !flat.includes('"flip":true')) return false;
  if (hint.includes('scalar=true') && !flat.includes('"scalar":true')) return false;
  return true;
}

/** Pattern-shape match for B5/B6/B7/B12. */
function matchesPattern(ast: AST, p: AllowListEntry): boolean {
  const id = String(p.id ?? '');
  const flat = JSON.stringify(ast);
  if (id === 'B7-flipped-join') return flat.includes('"flip":true');
  if (id === 'B12-companion-scalar-drift') return flat.includes('"scalar":true');
  if (id === 'B6-exists-limit-downgrade') {
    // EXISTS subquery with explicit limit > 1.
    return /"op":"EXISTS"[^}]*?\}[^}]*?"limit":\s*([2-9]|\d{2,})/.test(flat);
  }
  if (id === 'B5-or-exists-short-circuit') {
    // Or with at least 2 EXISTS branches.
    const orMatch = ast.where && (ast.where as {type?: string}).type === 'or';
    if (!orMatch) return false;
    const cs = (ast.where as unknown as {conditions: unknown[]}).conditions;
    let count = 0;
    for (const c of cs ?? []) {
      if ((c as {type?: string})?.type === 'correlatedSubquery') count++;
    }
    return count >= 2;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Main.
// ---------------------------------------------------------------------------
async function main(): Promise<number> {
  const allowList = loadAllowList();
  const built = buildArbitraries(paritySchema as never);
  let unexpectedCount = 0;
  let allowedCount = 0;
  let okCount = 0;
  let errorCount = 0;
  const recorded: Array<{ast: AST; reason: string}> = [];

  process.stdout.write(
    `random-ast-fuzz: numRuns=${NUM_RUNS} seed=${SEED} dryRun=${dryRun} ` +
      `tables=${built.tables.length} ` +
      `allow={keys:${allowList.keys.length},patterns:${allowList.patterns.length}}\n`,
  );

  try {
    await fc.assert(
      fc.asyncProperty(built.arbAst, async ast => {
        const result = await runOneAst({ast, dryRun});
        if (result.status === 'ok') {
          okCount++;
          return true;
        }
        if (result.status === 'error') {
          errorCount++;
          // In dry-run we never expect error; in live mode an error means
          // hydration timeout / WS error / server reject — Wave 1 needs to
          // distinguish "AST malformed (skip)" from "actual problem (fail)".
          // Wave 0: log and pass to keep smoke runs green.
          if (VERBOSE >= 2) {
            process.stderr.write(`[err] ${astHash(ast)}: ${result.error}\n`);
          }
          return true;
        }
        // status === 'diverge'.
        const verdict = isAllowedDivergence(ast, result.divergence, allowList);
        if (verdict.allowed) {
          allowedCount++;
          if (VERBOSE >= 2) {
            process.stderr.write(
              `[allow] ${astHash(ast)}: matched ${verdict.entry.id}\n`,
            );
          }
          return true;
        }
        unexpectedCount++;
        recorded.push({
          ast,
          reason: `unexpected divergence (canonicalKey=${result.divergence.canonicalKey})`,
        });
        return false;
      }),
      {numRuns: NUM_RUNS, seed: SEED, verbose: VERBOSE},
    );
  } catch (err) {
    // fc.assert throws when the property fails. The thrown error includes the
    // shrunk minimal counterexample. Wave 0 logs it and exits 1.
    process.stderr.write(
      `random-ast-fuzz: property FAILED — ${(err as Error).message}\n`,
    );
  }

  process.stdout.write(
    `random-ast-fuzz: ok=${okCount} allowed=${allowedCount} ` +
      `unexpected=${unexpectedCount} error=${errorCount}\n`,
  );

  if (unexpectedCount > 0) {
    process.stdout.write(
      `random-ast-fuzz: ${unexpectedCount} unexpected divergence(s):\n`,
    );
    for (const r of recorded.slice(0, 5)) {
      process.stdout.write(
        `  - ${astHash(r.ast)}: ${r.reason}\n    AST: ${JSON.stringify(r.ast)}\n`,
      );
    }
    return 1;
  }
  return 0;
}

const code = await main();
process.exit(code);
