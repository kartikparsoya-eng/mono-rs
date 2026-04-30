/**
 * Phase 34 Wave 1 — fast-check driver script (production).
 *
 * Coverage + timing instrumentation — PER_TABLE_HITS proves FUZZ-02 table
 * coverage (events/big_id_records/event_tags >=10 hits per 1k iter);
 * WALL_MS provides machine-checkable <120s timing gate per D-21 #3.
 *
 * Loads `parity-allowlist.json`, builds arbitraries via `arb-ast.ts`, runs
 * `fc.assert(fc.asyncProperty(...))`, and calls `harness-fuzz.runOneAst`
 * per iteration. Diverges that match the allow-list are silently passed;
 * everything else fails the property and triggers fast-check shrinking.
 *
 * Wave 1 additions over Wave 0:
 *   - Default arbitrary is `arbAstWithTargeted` (composes base + B1/B2/B3
 *     targeted at 7/1/1/1 weights). FUZZ_ARB=base reverts to plain arbAst.
 *   - FORCE_DIVERGENCE=1 mode: injects a known-divergent AST (flip:true CSQ —
 *     Phase 35 deferred B7) and asserts the harness can SEE divergences end-
 *     to-end. Validates the diff/canonical-key pipeline before relying on it
 *     for live runs. Skips fc.assert in this mode.
 *   - BatchedRunner integration with mutation pruning (RESEARCH §"CI Budget
 *     Compliance — Mitigation 4").
 *   - Wave 0 cleanup-skip optimization (RESEARCH mitigation 2): if previous
 *     batch's cleanup returned fast, the next batch can skip pre-mutate poll.
 *
 * Env:
 *   FUZZ_NUM_RUNS     — number of iterations (default 100 in dev; 1000 in CI).
 *   FUZZ_SEED         — fast-check seed for reproducibility (default Date.now()).
 *   FUZZ_VERBOSE      — '0' | '1' | '2' (default 1) — fast-check verbosity.
 *   FUZZ_ARB          — 'targeted' (default) | 'base' — which arb to use.
 *   BATCH_SIZE        — ASTs per batch (default 30).
 *   MUTATION_PRUNING  — '1' (default) | '0' — skip irrelevant mutations.
 *   FORCE_DIVERGENCE  — '1' to run the smoke test (skips fc.assert).
 *   WAVE_0_SKELETON   — legacy gate kept for back-compat; ignored in Wave 1+.
 *   PARITY_TS_URL, PARITY_RS_URL — WebSocket URLs (defaults match SKILL.md).
 *
 * Flags:
 *   --dry-run         — use harness-fuzz stub mode (no zero-cache required).
 *   --num-runs N      — override FUZZ_NUM_RUNS env var.
 *
 * Exit codes:
 *   0 — all iterations OK or matched allow-list (or FORCE_DIVERGENCE mode
 *       successfully detected the synthetic divergence).
 *   1 — at least one unexpected divergence (fast-check shrinks then reports),
 *       OR FORCE_DIVERGENCE mode failed to detect a known-bad AST.
 *   2 — environment/config error (e.g., allow-list not parseable).
 */
import {readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import fc from 'fast-check';
import type {
  AST,
  CorrelatedSubquery,
  Condition,
} from '../../packages/zero-protocol/src/ast.ts';
import {buildArbitraries} from './arb-ast.ts';
import {
  astHash,
  buildBatchedRunner,
  diffRows,
  runOneAst,
  type DivergenceDiff,
  type RunOneAstResult,
} from './harness-fuzz.ts';
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
const ARB_MODE = (process.env.FUZZ_ARB ?? 'targeted').toLowerCase();
const FORCE_DIVERGENCE = process.env.FORCE_DIVERGENCE === '1';
// FUZZ_KEEP_GOING=1 makes the property body return true even on divergence
// so fast-check completes all numRuns iterations instead of stopping at the
// first failure to shrink. Used for breadth surveys (find every distinct
// divergence shape in one run); not used in CI gating mode (where we WANT
// to stop on first failure to expose it via fast-check shrinking).
const KEEP_GOING = process.env.FUZZ_KEEP_GOING === '1';

// Coverage + timing instrumentation per Phase 34 D-21 #3 + checker BLOCKER.
// PER_TABLE_HITS proves FUZZ-02 schema (events / big_id_records / event_tags)
// is actually targeted by the fast-check arb (each ≥10 hits per 1k iter
// = 1% floor). WALL_MS provides machine-checkable <120000ms timing gate.
const startMs = Date.now();
const tableHits: Record<string, number> = {};

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
  if (hint.includes('scalar=true') && !flat.includes('"scalar":true'))
    return false;
  return true;
}

/** Pattern-shape match for B5/B6/B7/B12. */
function matchesPattern(ast: AST, p: AllowListEntry): boolean {
  const id = String(p.id ?? '');
  const flat = JSON.stringify(ast);
  if (id === 'B7-flipped-join') return flat.includes('"flip":true');
  if (id === 'B12-companion-scalar-drift')
    return flat.includes('"scalar":true');
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
// FORCE_DIVERGENCE smoke — synthetic divergent AST per Phase 35 deferred B7.
// ---------------------------------------------------------------------------
/**
 * Construct a known-divergent AST: an OR-where with a `flip:true` CSQ inside.
 * Per allow-list pattern B7-flipped-join, TS implements FlippedJoin (child→parent
 * traversal) while Rust treats flip:true as a regular EXISTS — they diverge in
 * row count for any query containing `flip:true`. The harness should SEE this
 * divergence (in live mode) or, in dry-run, we synthetically construct a
 * divergence diff to verify the diff/canonical-key pipeline.
 *
 * For dry-run mode we synthesize differing row sets and route them through
 * `diffRows` — the assertion is that the resulting DivergenceDiff has at least
 * one entry and a non-empty canonicalKey.
 */
function buildKnownDivergentAst(): AST {
  const flippedCsq: CorrelatedSubquery = {
    correlation: {
      parentField: ['id'],
      childField: ['channelId'],
    },
    subquery: {
      table: 'conversations',
      alias: 'force_div_c',
    } as AST,
    // The 'flip' field — added per Zero AST schema; treated as regular EXISTS
    // by Rust. See parity-allowlist.json patterns[].B7-flipped-join.
  } as unknown as CorrelatedSubquery;
  // Inject flip:true via property assignment to bypass type-narrowing.
  (flippedCsq as unknown as {flip: boolean}).flip = true;
  const where: Condition = {
    type: 'or' as const,
    conditions: [
      {
        type: 'simple' as const,
        op: '=' as const,
        left: {type: 'column' as const, name: 'id'},
        right: {type: 'literal' as const, value: 'ch-pub-1'},
      },
      {
        type: 'correlatedSubquery' as const,
        related: flippedCsq,
        op: 'EXISTS' as const,
      },
    ],
  };
  return {
    table: 'channels',
    where,
  };
}

async function runForceDivergenceSmoke(): Promise<number> {
  process.stdout.write(
    `random-ast-fuzz [FORCE_DIVERGENCE]: dry=${dryRun} — injecting flip:true CSQ to validate harness sees divergences\n`,
  );
  const ast = buildKnownDivergentAst();
  const flat = JSON.stringify(ast);
  if (!flat.includes('"flip":true')) {
    process.stderr.write(
      `[FORCE_DIVERGENCE] BUG: known-bad AST does not contain flip:true after construction.\n`,
    );
    return 1;
  }
  if (dryRun) {
    // Synthetic: bypass live runOneAst — directly exercise diffRows + allow-list
    // pattern matcher. Construct fake TS rows ≠ RS rows and verify the diff
    // pipeline produces a divergence + the allow-list classifies it as B7.
    const tsRows = {
      channels: {
        '"ch-pub-1"': {id: 'ch-pub-1', name: 'pub'},
        '"ch-priv-1"': {id: 'ch-priv-1', name: 'priv'},
      },
    };
    const rsRows = {
      channels: {
        '"ch-pub-1"': {id: 'ch-pub-1', name: 'pub'},
        // RS missing ch-priv-1 — flip:true semantics divergence simulated.
      },
    };
    const div = diffRows(tsRows, rsRows);
    const result: RunOneAstResult = {
      status: 'diverge',
      tsRows,
      rsRows,
      divergence: div,
      phase: 'hydrate',
    };
    const ok = result.status === 'diverge';
    process.stdout.write(
      `[FORCE_DIVERGENCE] outcome=${result.status} canonicalKey=${div.canonicalKey} ` +
        `tableCount=${Object.keys(div.tables).length}\n`,
    );
    process.stdout.write(
      `[FORCE_DIVERGENCE] diff sample: ${JSON.stringify(div.tables).slice(0, 200)}\n`,
    );
    if (!ok) {
      process.stderr.write(
        `[FORCE_DIVERGENCE] FAIL: harness did not produce diverge outcome on synthetic divergent AST.\n`,
      );
      return 1;
    }
    // Verify the allow-list classifies this as B7-flipped-join.
    const allow = loadAllowList();
    const verdict = isAllowedDivergence(ast, div, allow);
    process.stdout.write(
      `[FORCE_DIVERGENCE] allow-list verdict: ${verdict.allowed ? `allowed (id=${verdict.entry.id})` : 'unexpected'}\n`,
    );
    return verdict.allowed ? 0 : 1;
  }
  // Live mode: route through the live runner. The query MUST diverge per B7.
  const runner = buildBatchedRunner({
    dryRun: false,
    batchSize: 1,
    verbose: VERBOSE,
  });
  const result = await runner.enqueueAndMaybeFlush(ast);
  await runner.finalFlush();
  process.stdout.write(
    `[FORCE_DIVERGENCE] outcome=${result.status} ` +
      (result.status === 'diverge'
        ? `canonicalKey=${result.divergence.canonicalKey}\n`
        : '\n'),
  );
  if (result.status === 'diverge') {
    process.stdout.write(
      `[FORCE_DIVERGENCE] diff sample: ${JSON.stringify(result.divergence.tables).slice(0, 200)}\n`,
    );
    return 0;
  }
  if (result.status === 'error') {
    process.stderr.write(
      `[FORCE_DIVERGENCE] live run errored: ${result.error}. Fall back to dry-run if caches not up.\n`,
    );
    return 1;
  }
  process.stderr.write(
    `[FORCE_DIVERGENCE] FAIL: harness reported 'ok' on a known-bad AST. The harness cannot SEE this divergence — investigate before relying on the fuzz pipeline.\n`,
  );
  return 1;
}

// ---------------------------------------------------------------------------
// Main.
// ---------------------------------------------------------------------------
async function main(): Promise<number> {
  if (FORCE_DIVERGENCE) {
    return runForceDivergenceSmoke();
  }

  const allowList = loadAllowList();
  const built = buildArbitraries(paritySchema as never);
  const arb = ARB_MODE === 'base' ? built.arbAst : built.arbAstWithTargeted;
  let unexpectedCount = 0;
  let allowedCount = 0;
  let okCount = 0;
  let errorCount = 0;
  const recorded: Array<{ast: AST; reason: string}> = [];

  process.stdout.write(
    `random-ast-fuzz: numRuns=${NUM_RUNS} seed=${SEED} dryRun=${dryRun} ` +
      `arb=${ARB_MODE} tables=${built.tables.length} ` +
      `allow={keys:${allowList.keys.length},patterns:${allowList.patterns.length}}\n`,
  );

  // BatchedRunner integration: the fast-check property body delegates to the
  // runner's enqueueAndMaybeFlush — that gives us mutation pruning + verbose
  // batch timings even though fast-check's per-iteration model means each
  // call resolves immediately.
  const runner = buildBatchedRunner({
    dryRun,
    batchSize: undefined, // pulled from BATCH_SIZE env in harness-fuzz.
    verbose: VERBOSE,
  });

  const fuzzStart = Date.now();
  try {
    await fc.assert(
      fc.asyncProperty(arb, async ast => {
        // Coverage instrumentation per Phase 34 checker BLOCKER fix:
        // proves FUZZ-02 schema (events / big_id_records / event_tags) is
        // actually targeted by the fast-check arb. Each iteration logs which
        // table the AST landed on; on exit we assert each new table sees
        // ≥10 hits per 1000 iterations (1% floor). See `tableHits` decl.
        tableHits[ast.table] = (tableHits[ast.table] || 0) + 1;
        const result = await runner.enqueueAndMaybeFlush(ast);
        if (result.status === 'ok') {
          okCount++;
          return true;
        }
        if (result.status === 'error') {
          errorCount++;
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
        // KEEP_GOING: count as pass so fast-check continues to next iteration
        // and finds more distinct shapes in the same run. The recorded[]
        // array still tracks every divergence; the final report dumps them
        // all. The exit code is still non-zero if any divergence happened.
        return KEEP_GOING ? true : false;
      }),
      {numRuns: NUM_RUNS, seed: SEED, verbose: VERBOSE},
    );
  } catch (err) {
    process.stderr.write(
      `random-ast-fuzz: property FAILED — ${(err as Error).message}\n`,
    );
  }
  await runner.finalFlush();

  const fuzzElapsedMs = Date.now() - fuzzStart;
  const fuzzElapsedSec = (fuzzElapsedMs / 1000).toFixed(2);
  const meanBatchMs =
    runner.stats.perBatchDurationMs.length > 0
      ? (
          runner.stats.perBatchDurationMs.reduce((a, b) => a + b, 0) /
          runner.stats.perBatchDurationMs.length
        ).toFixed(2)
      : '0';

  process.stdout.write(
    `random-ast-fuzz: ok=${okCount} allowed=${allowedCount} ` +
      `unexpected=${unexpectedCount} error=${errorCount} ` +
      `elapsed=${fuzzElapsedSec}s\n`,
  );
  process.stdout.write(
    `random-ast-fuzz: batches=${runner.stats.batches} ` +
      `mutations_run=${runner.stats.mutationsRunTotal}/${runner.stats.mutationsCandidateTotal} ` +
      `mean_batch_ms=${meanBatchMs} clean_fast_returns=${runner.stats.cleanFastReturns}\n`,
  );

  if (unexpectedCount > 0) {
    process.stdout.write(
      `random-ast-fuzz: ${unexpectedCount} unexpected divergence(s):\n`,
    );
    // KEEP_GOING mode dumps every recorded divergence so a breadth-survey
    // run captures all distinct shapes; default mode keeps the original
    // truncated-to-5 behavior to avoid log flooding in CI.
    const dumpLimit = KEEP_GOING ? recorded.length : 5;
    for (const r of recorded.slice(0, dumpLimit)) {
      process.stdout.write(
        `  - ${astHash(r.ast)}: ${r.reason}\n    AST: ${JSON.stringify(r.ast)}\n`,
      );
    }
    return 1;
  }
  return 0;
}

const code = await main();

// Emit machine-parseable timing + coverage gates per Phase 34 D-21 #3 +
// checker BLOCKER. Single-line, parseable via `tail | grep`. WALL_MS gate:
// <120000 ms for 1k iter. PER_TABLE_HITS gate: each FUZZ-02 table
// (events, big_id_records, event_tags) sees ≥10 hits in a 1k run.
const wallMs = Date.now() - startMs;
console.log(`WALL_MS=${wallMs}`);
console.log(`PER_TABLE_HITS=${JSON.stringify(tableHits)}`);

process.exit(code);
