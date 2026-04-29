/**
 * Phase 34 Wave 0 — fast-check ↔ harness shared roundtrip primitive.
 *
 * Wraps a single TS↔RS hydrate-and-diff iteration as a callable function so the
 * fast-check driver (`random-ast-fuzz.ts`) can call it per arbitrary draw without
 * caring about WebSocket / poke quiescence / replicator state. Wave 0 exposes
 * the SHIM only; Wave 1 wires it to the live `subscribeAndHydrate` from
 * `harness-coverage.ts:100-224` and adds the BatchedRunner pattern for the
 * <2 min budget (RESEARCH "Two-Process Harness Reuse — Per-iteration flow").
 *
 * The shim is intentionally narrow: caller passes `ast + hash + schema + urls`,
 * receives `{ tsRows, rsRows, divergence?: Diff, error?: string }`. This keeps
 * the fast-check assertion a one-liner: `result.divergence === undefined`.
 *
 * Why a separate module? `harness-coverage.ts` is corpus-driven; it reads
 * `ast_corpus.json`, runs ~1000 ASTs in parallel, writes `coverage_run.json`.
 * The fast-check driver wants per-AST iteration with shrinking — different
 * lifecycle. Sharing the WebSocket / poke logic via a small primitive avoids
 * duplication while keeping the corpus harness untouched (D-04 spirit).
 *
 * --smoke flag: prints one synthetic divergence-free outcome and exits 0 so
 * the Wave 0 plan's verification step has a concrete check.
 */
import {createHash} from 'node:crypto';
import type {AST} from '../../packages/zero-protocol/src/ast.ts';

export const TS_URL_DEFAULT =
  process.env.PARITY_TS_URL ?? 'ws://localhost:4858/sync/v50/connect';
export const RS_URL_DEFAULT =
  process.env.PARITY_RS_URL ?? 'ws://localhost:4868/sync/v50/connect';
export const HYDRATION_TIMEOUT_MS = Number(
  process.env.HYDRATION_TIMEOUT_MS ?? 15_000,
);

export type RowsByTable = Record<string, Record<string, Record<string, unknown>>>;

export type DivergenceDiff = {
  tables: Record<
    string,
    {
      onlyTs: string[];
      onlyRs: string[];
      different: string[];
    }
  >;
  /** Stable canonical hash of (sorted) onlyTs∪onlyRs∪different keys. Lets
   *  callers dedupe identical divergences across iterations. */
  canonicalKey: string;
};

export type RunOneAstResult =
  | {status: 'ok'; tsRows: RowsByTable; rsRows: RowsByTable; tsHash: string; rsHash: string}
  | {status: 'diverge'; tsRows: RowsByTable; rsRows: RowsByTable; divergence: DivergenceDiff}
  | {status: 'error'; error: string; side: 'ts' | 'rs' | 'both'};

export interface RunOneAstOpts {
  ast: AST;
  /** Stable hash for this AST so the server logs it. The default uses
   *  SHA1(JSON(ast)).slice(0,12) — deterministic across machines.  */
  hash?: string;
  tsUrl?: string;
  rsUrl?: string;
  /** When true, the function only validates inputs and returns a synthetic
   *  ok result. Useful for Wave 0 smoke verification before zero-cache
   *  binaries are running. */
  dryRun?: boolean;
}

export function astHash(ast: AST): string {
  return createHash('sha1')
    .update(JSON.stringify(ast))
    .digest('hex')
    .slice(0, 12);
}

/** Hash a snapshot of rows for cheap equality test. */
function rowsHash(rows: RowsByTable): string {
  // Canonicalize: sort tables and keys-per-table so equal payloads hash equal.
  const tables = Object.keys(rows).sort();
  const obj: Record<string, Record<string, Record<string, unknown>>> = {};
  for (const t of tables) {
    const keys = Object.keys(rows[t]).sort();
    obj[t] = {};
    for (const k of keys) obj[t][k] = rows[t][k];
  }
  return createHash('sha1').update(JSON.stringify(obj)).digest('hex');
}

function diffRows(ts: RowsByTable, rs: RowsByTable): DivergenceDiff {
  const tables = new Set([...Object.keys(ts), ...Object.keys(rs)]);
  const out: DivergenceDiff['tables'] = {};
  for (const t of tables) {
    const tsKeys = new Set(Object.keys(ts[t] ?? {}));
    const rsKeys = new Set(Object.keys(rs[t] ?? {}));
    const onlyTs = [...tsKeys].filter(k => !rsKeys.has(k)).sort();
    const onlyRs = [...rsKeys].filter(k => !tsKeys.has(k)).sort();
    const shared = [...tsKeys].filter(k => rsKeys.has(k));
    const different = shared
      .filter(k => JSON.stringify(ts[t][k]) !== JSON.stringify(rs[t][k]))
      .sort();
    if (onlyTs.length || onlyRs.length || different.length) {
      out[t] = {onlyTs, onlyRs, different};
    }
  }
  const canonicalKey = createHash('sha1')
    .update(JSON.stringify(out))
    .digest('hex')
    .slice(0, 16);
  return {tables: out, canonicalKey};
}

/**
 * Run one AST through both TS and RS zero-cache instances and diff the results.
 *
 * Wave 0: stub. The actual subscribeAndHydrate wiring lives in
 * `harness-coverage.ts:100-224`. Wave 1 will:
 *   - Either import that function (preferred — no duplication), or
 *   - Refactor it into a shared primitive that both this file and
 *     harness-coverage.ts call (preferred for batching).
 *
 * For now, `dryRun: true` is the only supported mode — returns a synthetic
 * ok result so smoke tests pass without zero-cache running. Wave 1 removes
 * the dry-run gate.
 */
export async function runOneAst(opts: RunOneAstOpts): Promise<RunOneAstResult> {
  const {ast, dryRun = false} = opts;
  if (!ast?.table) {
    return {status: 'error', error: 'ast.table missing', side: 'both'};
  }
  if (dryRun) {
    return {
      status: 'ok',
      tsRows: {},
      rsRows: {},
      tsHash: astHash(ast) + ':ts-stub',
      rsHash: astHash(ast) + ':rs-stub',
    };
  }
  // Wave 1: implement live mode by calling subscribeAndHydrate(ts) +
  // subscribeAndHydrate(rs) in parallel, awaiting both, diffing.
  return {
    status: 'error',
    error:
      'harness-fuzz: live mode not yet wired — Wave 1 will integrate ' +
      'subscribeAndHydrate from harness-coverage.ts:100-224. Pass ' +
      '{ dryRun: true } to use the stub.',
    side: 'both',
  };
}

/** Public helpers used by both `arb-ast.ts` consumers and the driver. */
export const harnessFuzz = {
  astHash,
  rowsHash,
  diffRows,
  runOneAst,
} as const;

// ---------------------------------------------------------------------------
// Smoke entry point — when run directly with --smoke flag, prints one
// synthetic divergence-free outcome and exits 0.
// ---------------------------------------------------------------------------
if (import.meta.url === `file://${process.argv[1]}`) {
  const args = process.argv.slice(2);
  const smoke = args.includes('--smoke');
  if (!smoke) {
    process.stderr.write(
      'harness-fuzz.ts: pass --smoke to run a synthetic verification.\n',
    );
    process.exit(0);
  }
  const sampleAst: AST = {table: 'channels'};
  const r = await runOneAst({ast: sampleAst, dryRun: true});
  process.stdout.write(JSON.stringify(r, null, 2) + '\n');
  process.stdout.write('harness-fuzz: smoke OK (stub mode).\n');
  process.exit(r.status === 'ok' ? 0 : 1);
}
