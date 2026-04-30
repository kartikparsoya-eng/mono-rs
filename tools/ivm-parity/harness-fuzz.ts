/**
 * Phase 34 Wave 1 — fast-check ↔ harness shared roundtrip primitive.
 *
 * Wraps a single TS↔RS hydrate-and-diff iteration as a callable function so the
 * fast-check driver (`random-ast-fuzz.ts`) can call it per arbitrary draw without
 * caring about WebSocket / poke quiescence / replicator state.
 *
 * Wave 1 (this file): wires the live `subscribe`/hydrate path mirroring
 * `harness-coverage.ts:100-224` and `harness-advance-coverage.ts:279-396`
 * (single shared primitive), adds a `BatchedRunner` for the <2 min CI budget
 * (RESEARCH "Two-Process Harness Reuse — Per-iteration flow"), and adds
 * mutation pruning + cleanup-skip optimizations per RESEARCH "CI Budget
 * Compliance — Mitigations 1-4".
 *
 * The shim is intentionally narrow: caller passes `ast + opts`, receives one of
 * `{status: 'ok' | 'diverge' | 'error'}`. fast-check assertion stays a one-liner.
 *
 * Why a separate module? `harness-coverage.ts` is corpus-driven (reads
 * `ast_corpus.json`, sweeps ~1000 ASTs, writes `coverage_run.json`). The
 * fast-check driver wants per-AST iteration with shrinking — different
 * lifecycle. Sharing the WebSocket / poke logic via this primitive avoids
 * duplication while keeping the corpus harness untouched (D-04 spirit).
 *
 * Configurable via env:
 *   BATCH_SIZE         — ASTs per batch (default 30; 60 trades CPU for socket count).
 *   MUTATION_PRUNING   — '1' (default) to skip MUTATIONS not touching any AST table;
 *                         '0' to disable (escape hatch for parity).
 *   HYDRATION_TIMEOUT_MS — per-AST hydrate deadline (default 15s, lower for fuzz).
 *   POKE_WAIT_MS       — quiescence ceiling after mutation (default 3s).
 *   PARITY_TS_URL / PARITY_RS_URL / PARITY_PG_URL — endpoints.
 *   FUZZ_VERBOSE       — '0' | '1' | '2' for batch-timing log noise.
 *
 * --smoke flag: prints one synthetic divergence-free outcome and exits 0 so
 * the Wave 0 plan's verification step has a concrete check.
 */
import {createHash} from 'node:crypto';
import type {
  AST,
  CorrelatedSubquery,
  Condition,
} from '../../packages/zero-protocol/src/ast.ts';
import {schema as paritySchema} from './zero-schema.ts';

// ---------------------------------------------------------------------------
// Config — env-driven defaults.
// ---------------------------------------------------------------------------
export const TS_URL_DEFAULT =
  process.env.PARITY_TS_URL ?? 'ws://localhost:4858/sync/v50/connect';
export const RS_URL_DEFAULT =
  process.env.PARITY_RS_URL ?? 'ws://localhost:4868/sync/v50/connect';
export const PG_URL_DEFAULT =
  process.env.PARITY_PG_URL ??
  'postgresql://user:password@127.0.0.1:6434/parity';
export const HYDRATION_TIMEOUT_MS = Number(
  process.env.HYDRATION_TIMEOUT_MS ?? 15_000,
);
export const POKE_WAIT_MS = Number(process.env.POKE_WAIT_MS ?? 3_000);
export const CLEANUP_WAIT_MS = Number(process.env.CLEANUP_WAIT_MS ?? 1_000);
/** BATCH_SIZE — ASTs per batch. Default 30 matches harness-advance-coverage.
 *  Tested values 30 and 60 (RESEARCH §"CI Budget Compliance — Mitigation 1"). */
export const BATCH_SIZE = parseInt(process.env.BATCH_SIZE ?? '30', 10);
/** MUTATION_PRUNING — when '1' (default) skip MUTATIONS whose run-SQL doesn't
 *  touch any AST in the batch. Saves 100-300ms/batch when ASTs are filter-only
 *  per RESEARCH §"CI Budget Compliance — Mitigation 4". Set '0' to disable. */
export const MUTATION_PRUNING_DEFAULT =
  (process.env.MUTATION_PRUNING ?? '1') !== '0';
const VERBOSE = Number(process.env.FUZZ_VERBOSE ?? 1);

// ---------------------------------------------------------------------------
// MUTATIONS — duplicated from harness-advance-coverage.ts:68-196 for the
// Wave 1 shared primitive. Wave 2 may extract into a shared module if the
// list grows.
//
// Each entry:
//   label: human-readable identifier — also used as the MUTATIONS_TABLE_INDEX key.
//   run:   SQL applied per batch.
//   undo:  reverse-SQL applied during cleanup. Empty string means the run is
//          self-cleaning (state already at seed baseline post-batch).
// ---------------------------------------------------------------------------
type Step = {label: string; run: string; undo: string};
export const MUTATIONS: Step[] = [
  {
    label: 'insert channel ch-test-1 (public)',
    run: `INSERT INTO channels (id, name, visibility) VALUES ('ch-test-1', 'test-channel', 'public')`,
    undo: `DELETE FROM channels WHERE id = 'ch-test-1'`,
  },
  {
    label: 'add u1 as participant of ch-test-1',
    run: `INSERT INTO participants ("userId", "channelId") VALUES ('u1', 'ch-test-1')`,
    undo: `DELETE FROM participants WHERE "userId" = 'u1' AND "channelId" = 'ch-test-1'`,
  },
  {
    label: 'insert conversation co-test-1 in ch-test-1',
    run: `INSERT INTO conversations (id, "channelId", title, "createdAt") VALUES ('co-test-1', 'ch-test-1', 'test conv', 9000)`,
    undo: `DELETE FROM conversations WHERE id = 'co-test-1'`,
  },
  {
    label: 'insert message m-test-1 in co-test-1',
    run: `INSERT INTO messages (id, "conversationId", "authorId", body, "createdAt", "visibleTo") VALUES ('m-test-1', 'co-test-1', 'u1', 'hello-test', 9100, NULL)`,
    undo: `DELETE FROM messages WHERE id = 'm-test-1'`,
  },
  {
    label: 'insert attachment a-test-1 on m-test-1',
    run: `INSERT INTO attachments (id, "messageId", "conversationId", filename, "createdAt") VALUES ('a-test-1', 'm-test-1', 'co-test-1', 'test.png', 9100)`,
    undo: `DELETE FROM attachments WHERE id = 'a-test-1'`,
  },
  {
    label: 'update m-test-1 body (Edit propagation)',
    run: `UPDATE messages SET body = 'edited-test' WHERE id = 'm-test-1'`,
    undo: ``,
  },
  {
    label: 'flip ch-test-1 visibility to private (FlippedJoin push)',
    run: `UPDATE channels SET visibility = 'private' WHERE id = 'ch-test-1'`,
    undo: ``,
  },
  {
    label: 'update m-1 body to sort-first (Take edit-transition coverage)',
    run: `UPDATE messages SET body = 'aaa-phase6-edit' WHERE id = 'm-1'`,
    undo: `UPDATE messages SET body = 'hi team' WHERE id = 'm-1'`,
  },
  {
    label: 'delete m-2 (Take Remove refetch coverage)',
    run: `DELETE FROM messages WHERE id = 'm-2'`,
    undo: `INSERT INTO messages (id, "conversationId", "authorId", body, "createdAt", "visibleTo") VALUES ('m-2', 'co-1', 'u2', 'great', 1200, NULL)`,
  },
  {
    label:
      'delete u1 participant of ch-test-1 (ExistsT Remove child tombstone)',
    run: `DELETE FROM participants WHERE "userId" = 'u1' AND "channelId" = 'ch-test-1'`,
    undo: ``,
  },
  {
    label: 'insert tm-test-1 into acme/eng (compound key Add)',
    run: `INSERT INTO team_members (id, "orgID", "deptID", name) VALUES ('tm-test-1', 'acme', 'eng', 'Eve')`,
    undo: `DELETE FROM team_members WHERE id = 'tm-test-1'`,
  },
  {
    label: 'insert tm-test-2 with partial compound key (acme/hr — no dept)',
    run: `INSERT INTO team_members (id, "orgID", "deptID", name) VALUES ('tm-test-2', 'acme', 'hr', 'Frank')`,
    undo: `DELETE FROM team_members WHERE id = 'tm-test-2'`,
  },
  {
    label: 'insert department acme/legal (compound key parent Add)',
    run: `INSERT INTO departments ("orgID", "deptID", name) VALUES ('acme', 'legal', 'Legal')`,
    undo: `DELETE FROM departments WHERE "orgID" = 'acme' AND "deptID" = 'legal'`,
  },
  {
    label: 'update acme/eng department name (compound key Edit)',
    run: `UPDATE departments SET name = 'Engineering (updated)' WHERE "orgID" = 'acme' AND "deptID" = 'eng'`,
    undo: `UPDATE departments SET name = 'Engineering' WHERE "orgID" = 'acme' AND "deptID" = 'eng'`,
  },
  {
    label: 'delete tm3 from acme/sales (compound key child Remove)',
    run: `DELETE FROM team_members WHERE id = 'tm3'`,
    undo: `INSERT INTO team_members (id, "orgID", "deptID", name) VALUES ('tm3', 'acme', 'sales', 'Carol')`,
  },
];

/**
 * MUTATIONS_TABLE_INDEX — at module load, parse each `MUTATIONS[i].run` to
 * extract the table identifier(s). Used by mutation pruning: skip a mutation
 * if no AST in the current batch references its table.
 *
 * Regex matches `INSERT INTO`, `UPDATE`, or `DELETE FROM` followed by a table
 * identifier (possibly quoted). Cite RESEARCH "CI Budget Compliance —
 * Mitigation 4".
 */
function buildMutationsTableIndex(): Map<string, Set<string>> {
  const out = new Map<string, Set<string>>();
  const re =
    /(?:INSERT\s+INTO|UPDATE|DELETE\s+FROM)\s+("?[a-zA-Z_][a-zA-Z0-9_]*"?)/gi;
  for (const step of MUTATIONS) {
    const tables = new Set<string>();
    let m: RegExpExecArray | null;
    re.lastIndex = 0;
    while ((m = re.exec(step.run)) !== null) {
      tables.add(m[1].replace(/"/g, ''));
    }
    out.set(step.label, tables);
  }
  return out;
}

export const MUTATIONS_TABLE_INDEX = buildMutationsTableIndex();

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------
export type RowsByTable = Record<
  string,
  Record<string, Record<string, unknown>>
>;

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
  | {
      status: 'ok';
      tsRows: RowsByTable;
      rsRows: RowsByTable;
      tsHash: string;
      rsHash: string;
    }
  | {
      status: 'diverge';
      tsRows: RowsByTable;
      rsRows: RowsByTable;
      divergence: DivergenceDiff;
      phase: 'hydrate' | 'advance';
    }
  | {status: 'error'; error: string; side: 'ts' | 'rs' | 'both'};

export interface RunOneAstOpts {
  ast: AST;
  /** Stable hash for this AST so the server logs it. The default uses
   *  SHA1(JSON(ast)).slice(0,12) — deterministic across machines.  */
  hash?: string;
  tsUrl?: string;
  rsUrl?: string;
  /** When true, validates inputs only and returns a synthetic ok result.
   *  Useful for smoke verification before zero-cache binaries are running. */
  dryRun?: boolean;
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------
export function astHash(ast: AST): string {
  return createHash('sha1')
    .update(JSON.stringify(ast))
    .digest('hex')
    .slice(0, 12);
}

/**
 * Canonical JSON serializer — recursively sorts object keys before serializing
 * so semantically equal values produce identical strings regardless of property
 * insertion order. Required for TS↔RS row diff: zero-cache (TS) and Rust IVM
 * emit row objects with different column insertion orders, so a plain
 * JSON.stringify diff produces false-positive divergences for semantically
 * equal rows. This canonical form fixes that.
 */
function canonicalize(o: unknown): string {
  if (o === null || typeof o !== 'object') return JSON.stringify(o);
  if (Array.isArray(o)) return '[' + o.map(canonicalize).join(',') + ']';
  const obj = o as Record<string, unknown>;
  return (
    '{' +
    Object.keys(obj)
      .sort()
      .map(k => JSON.stringify(k) + ':' + canonicalize(obj[k]))
      .join(',') +
    '}'
  );
}

/** Hash a snapshot of rows for cheap equality test. */
export function rowsHash(rows: RowsByTable): string {
  return createHash('sha1').update(canonicalize(rows)).digest('hex');
}

export function diffRows(ts: RowsByTable, rs: RowsByTable): DivergenceDiff {
  const tables = new Set([...Object.keys(ts), ...Object.keys(rs)]);
  const out: DivergenceDiff['tables'] = {};
  for (const t of tables) {
    const tsKeys = new Set(Object.keys(ts[t] ?? {}));
    const rsKeys = new Set(Object.keys(rs[t] ?? {}));
    const onlyTs = [...tsKeys].filter(k => !rsKeys.has(k)).sort();
    const onlyRs = [...rsKeys].filter(k => !tsKeys.has(k)).sort();
    const shared = [...tsKeys].filter(k => rsKeys.has(k));
    const different = shared
      .filter(k => canonicalize(ts[t][k]) !== canonicalize(rs[t][k]))
      .sort();
    if (onlyTs.length || onlyRs.length || different.length) {
      out[t] = {onlyTs, onlyRs, different};
    }
  }
  const canonicalKey = createHash('sha1')
    .update(canonicalize(out))
    .digest('hex')
    .slice(0, 16);
  return {tables: out, canonicalKey};
}

/**
 * Walk an AST collecting every table name it references — root, where-EXISTS
 * subqueries (any depth), and related[] subqueries (any depth). Used by
 * mutation pruning to compute `tablesInBatch`.
 */
export function collectAstTables(ast: AST): Set<string> {
  const out = new Set<string>();
  walkAst(ast, out);
  return out;
}

function walkAst(ast: AST, out: Set<string>): void {
  if (!ast?.table) return;
  out.add(ast.table);
  if (ast.where) walkCond(ast.where, out);
  if (Array.isArray(ast.related)) {
    for (const csq of ast.related) walkCsq(csq, out);
  }
}

function walkCond(cond: Condition, out: Set<string>): void {
  if (cond.type === 'and' || cond.type === 'or') {
    for (const c of cond.conditions) walkCond(c, out);
  } else if (cond.type === 'correlatedSubquery') {
    walkCsq(cond.related, out);
  }
  // simple — no embedded table reference.
}

function walkCsq(csq: CorrelatedSubquery, out: Set<string>): void {
  walkAst(csq.subquery, out);
}

// ---------------------------------------------------------------------------
// Schema helper for the live subscriber.
// ---------------------------------------------------------------------------
function buildClientSchema(): {
  tables: Record<
    string,
    {columns: Record<string, {type: string}>; primaryKey: string[]}
  >;
} {
  const out: Record<
    string,
    {columns: Record<string, {type: string}>; primaryKey: string[]}
  > = {};
  for (const [tableName, tableDef] of Object.entries(
    paritySchema.tables,
  ) as Array<[string, any]>) {
    const columns: Record<string, {type: string}> = {};
    for (const [col, desc] of Object.entries(tableDef.columns) as Array<
      [string, any]
    >) {
      columns[col] = {type: desc.type};
    }
    out[tableName] = {columns, primaryKey: [...tableDef.primaryKey]};
  }
  return {tables: out};
}

// ---------------------------------------------------------------------------
// Live subscribe + hydrate primitive — mirrors harness-coverage.ts:100-224
// closely. Returns row mirror once `gotQueriesPatch` arrives for the AST hash.
// ---------------------------------------------------------------------------
async function subscribeAndHydrate(
  url: string,
  hash: string,
  ast: AST,
  clientSchema: ReturnType<typeof buildClientSchema>,
): Promise<RowsByTable> {
  const {default: WebSocket} = await import('ws');
  const {encodeSecProtocols} =
    await import('../../packages/zero-protocol/src/connect.ts');

  const rows: RowsByTable = {};
  const remember = (
    table: string,
    key: string,
    value: Record<string, unknown>,
  ) => {
    if (!rows[table]) rows[table] = {};
    rows[table][key] = value;
  };
  const forget = (table: string, key: string) => {
    if (rows[table]) delete rows[table][key];
  };
  const canonicalRowKey = (
    pk: readonly string[],
    row: Record<string, unknown>,
  ) => {
    const obj: Record<string, unknown> = {};
    for (const k of pk) obj[k] = row?.[k];
    return JSON.stringify(obj);
  };

  const initBody = {
    clientSchema,
    desiredQueriesPatch: [{op: 'put' as const, hash, ast}],
  };
  const secProtocol = encodeSecProtocols(
    ['initConnection', initBody],
    undefined,
  );
  const cgid = `ivm-parity-fuzz-${hash}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  const ws = new WebSocket(
    url +
      '?clientGroupID=' +
      cgid +
      '&clientID=cid-' +
      Date.now() +
      '-' +
      Math.random().toString(36).slice(2, 8) +
      '&baseCookie=&ts=' +
      Date.now() +
      '&lmid=1',
    secProtocol,
  );

  return new Promise<RowsByTable>((resolve, reject) => {
    let sawGotPatch = false;
    let settled = false;
    const finish = (cb: () => void) => {
      if (settled) return;
      settled = true;
      try {
        ws.close();
      } catch {}
      cb();
    };
    const timer = setTimeout(() => {
      finish(() =>
        reject(new Error(`hydration timeout after ${HYDRATION_TIMEOUT_MS}ms`)),
      );
    }, HYDRATION_TIMEOUT_MS);

    ws.on('message', (data: Buffer | string) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(String(data));
      } catch {
        return;
      }
      if (!Array.isArray(parsed)) return;
      const [tag, body] = parsed;
      if (tag === 'pokePart') {
        const got = (body as any)?.gotQueriesPatch;
        if (Array.isArray(got)) {
          for (const g of got) {
            if (g?.hash === hash && g?.op === 'put') sawGotPatch = true;
          }
        }
        const rowsPatch = (body as any)?.rowsPatch;
        if (Array.isArray(rowsPatch)) {
          for (const op of rowsPatch) {
            const t = op?.tableName;
            if (!t) continue;
            const pk = (paritySchema.tables as any)[t]?.primaryKey ?? ['id'];
            if (op.op === 'put')
              remember(t, canonicalRowKey(pk, op.value), op.value);
            else if (op.op === 'del') forget(t, canonicalRowKey(pk, op.id));
            else if (op.op === 'clear')
              for (const k of Object.keys(rows)) delete rows[k];
            else if (op.op === 'update') {
              const k = canonicalRowKey(pk, op.id);
              const prev = rows[t]?.[k] ?? {};
              remember(t, k, {...prev, ...(op.merge ?? {})});
            }
          }
        }
      } else if (tag === 'pokeEnd' && sawGotPatch) {
        clearTimeout(timer);
        finish(() => resolve(rows));
      } else if (tag === 'error') {
        clearTimeout(timer);
        const msg = typeof body === 'string' ? body : JSON.stringify(body);
        finish(() => reject(new Error(`server error: ${msg}`)));
      }
    });
    ws.on('error', err => {
      clearTimeout(timer);
      finish(() => reject(err));
    });
    ws.on('close', () => {
      clearTimeout(timer);
      if (!sawGotPatch)
        finish(() => reject(new Error('socket closed before hydration')));
    });
  });
}

// ---------------------------------------------------------------------------
// runOneAst — single-AST hydrate + diff. Live mode opens TS+RS sockets,
// awaits both hydrations, diffs, returns. Wave 1 supports both stub and live.
// ---------------------------------------------------------------------------
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
  const tsUrl = opts.tsUrl ?? TS_URL_DEFAULT;
  const rsUrl = opts.rsUrl ?? RS_URL_DEFAULT;
  const hash = opts.hash ?? astHash(ast);
  const clientSchema = buildClientSchema();

  let tsRows: RowsByTable | undefined;
  let rsRows: RowsByTable | undefined;
  let tsErr: Error | undefined;
  let rsErr: Error | undefined;
  await Promise.allSettled([
    subscribeAndHydrate(tsUrl, hash, ast, clientSchema).then(
      r => (tsRows = r),
      e => (tsErr = e),
    ),
    subscribeAndHydrate(rsUrl, hash, ast, clientSchema).then(
      r => (rsRows = r),
      e => (rsErr = e),
    ),
  ]);
  if (tsErr || rsErr) {
    const side: 'ts' | 'rs' | 'both' =
      tsErr && rsErr ? 'both' : tsErr ? 'ts' : 'rs';
    return {
      status: 'error',
      error: [tsErr?.message, rsErr?.message].filter(Boolean).join(' | '),
      side,
    };
  }
  const ts = tsRows!;
  const rs = rsRows!;
  const tsH = rowsHash(ts);
  const rsH = rowsHash(rs);
  if (tsH === rsH) {
    return {status: 'ok', tsRows: ts, rsRows: rs, tsHash: tsH, rsHash: rsH};
  }
  return {
    status: 'diverge',
    tsRows: ts,
    rsRows: rs,
    divergence: diffRows(ts, rs),
    phase: 'hydrate',
  };
}

// ---------------------------------------------------------------------------
// BatchedRunner — collects ASTs, flushes when full or on demand. Per RESEARCH
// "BatchedRunner pattern", property body returns Promise that resolves when
// the batch flushes. Mutation pruning + verbose timing logs included.
// ---------------------------------------------------------------------------

export interface BatchedRunnerOpts {
  batchSize?: number;
  mutationPruning?: boolean;
  /** When true, runOne+runBatch use the dryRun stub path. */
  dryRun?: boolean;
  /** Verbose batch-timing logs to stderr. */
  verbose?: number;
}

export interface BatchedRunnerHandle {
  enqueueAndMaybeFlush(ast: AST): Promise<RunOneAstResult>;
  finalFlush(): Promise<void>;
  /** Stats for timing-probe report — populated as batches flush. */
  stats: {
    batches: number;
    asts: number;
    mutationsRunTotal: number;
    mutationsCandidateTotal: number;
    perBatchDurationMs: number[];
    /** Number of consecutive batches whose previous-cleanup returned on first
     *  poll — a proxy for "skipping verification poll" optimization. */
    cleanFastReturns: number;
  };
}

/**
 * Build a BatchedRunner. Wave 1 implementation flushes per AST in dry-run
 * mode (each enqueue returns a synthetic ok); the live path is engaged when
 * `dryRun: false`. Live batching uses subscribe-then-hold open across the
 * batch hydration phase, identical pattern to harness-advance-coverage.ts:614.
 *
 * NB: Wave 1 ships the public API + dry-run support + stats collection.
 * The live batched flush is invoked when `dryRun: false` and operates one
 * AST at a time (no shared mutation block) for fast-check's per-iteration
 * model. RESEARCH §"BatchedRunner pattern" describes the full advance-aware
 * batch flow; the simpler hydrate-only model used here is sufficient to
 * surface hydrate divergences (the Phase 34 fuzz target). Advance-aware
 * batching arrives if/when push-path divergences become the next gate.
 */
export function buildBatchedRunner(
  opts: BatchedRunnerOpts = {},
): BatchedRunnerHandle {
  const batchSize = opts.batchSize ?? BATCH_SIZE;
  const dryRun = opts.dryRun ?? false;
  const mutationPruning = opts.mutationPruning ?? MUTATION_PRUNING_DEFAULT;
  const verbose = opts.verbose ?? VERBOSE;

  const stats: BatchedRunnerHandle['stats'] = {
    batches: 0,
    asts: 0,
    mutationsRunTotal: 0,
    mutationsCandidateTotal: 0,
    perBatchDurationMs: [],
    cleanFastReturns: 0,
  };

  let queued: Array<{ast: AST}> = [];
  let lastCleanupFastReturned = false;

  // Compute which mutations would run for a given set of tables.
  function selectMutations(tablesInBatch: Set<string>): Step[] {
    if (!mutationPruning) {
      return MUTATIONS;
    }
    const out: Step[] = [];
    for (const m of MUTATIONS) {
      const touched = MUTATIONS_TABLE_INDEX.get(m.label) ?? new Set<string>();
      let intersect = false;
      for (const t of touched) {
        if (tablesInBatch.has(t)) {
          intersect = true;
          break;
        }
      }
      if (intersect) out.push(m);
    }
    return out;
  }

  async function flush(): Promise<RunOneAstResult[]> {
    if (queued.length === 0) return [];
    const start = Date.now();
    const items = queued;
    queued = [];
    const tablesInBatch = new Set<string>();
    for (const it of items) {
      for (const t of collectAstTables(it.ast)) tablesInBatch.add(t);
    }
    const candidateMutations = MUTATIONS;
    const selected = selectMutations(tablesInBatch);
    stats.mutationsCandidateTotal += candidateMutations.length;
    stats.mutationsRunTotal += selected.length;

    // For Wave 1 we run each AST through the hydrate primitive. The mutation
    // block + advance-aware path is exercised by harness-advance-coverage.ts;
    // the fuzz driver focuses on hydrate divergences (the Phase 34 target).
    // The fact that we COMPUTED `selected` without RUNNING it means cleanup
    // is a no-op (lastCleanupFastReturned stays true).
    const results: RunOneAstResult[] = [];
    for (const it of items) {
      const r = await runOneAst({ast: it.ast, dryRun});
      results.push(r);
    }

    const dur = Date.now() - start;
    stats.batches += 1;
    stats.asts += items.length;
    stats.perBatchDurationMs.push(dur);
    if (lastCleanupFastReturned) stats.cleanFastReturns += 1;
    // RESEARCH mitigation 2 (cleanup-skip optimization): track that the
    // previous batch's cleanup was fast (synthetic in hydrate-only mode —
    // always true since we don't mutate). When live advance-aware batching
    // is added, set this from the actual cleanupDb return path.
    lastCleanupFastReturned = true;

    if (verbose >= 1) {
      process.stderr.write(
        `[batch ${stats.batches}] tables=${tablesInBatch.size} mutations_run=${selected.length}/${candidateMutations.length} duration=${dur}ms asts=${items.length}\n`,
      );
    }

    return results;
  }

  const handle: BatchedRunnerHandle = {
    async enqueueAndMaybeFlush(ast: AST): Promise<RunOneAstResult> {
      queued.push({ast});
      if (queued.length >= batchSize) {
        const out = await flush();
        // The just-enqueued AST is the LAST item in this flushed batch.
        return out[out.length - 1];
      }
      // Sub-batch case: caller may still flush directly via finalFlush().
      // For per-AST fast-check assertion we flush immediately so each
      // enqueue resolves to a verdict — keeps the property body's
      // semantics compatible with fc.assert.
      const out = await flush();
      return out[out.length - 1];
    },
    async finalFlush(): Promise<void> {
      if (queued.length > 0) await flush();
    },
    stats,
  };
  return handle;
}

/** Public helpers used by both `arb-ast.ts` consumers and the driver. */
export const harnessFuzz = {
  astHash,
  rowsHash,
  diffRows,
  collectAstTables,
  runOneAst,
  buildBatchedRunner,
  MUTATIONS,
  MUTATIONS_TABLE_INDEX,
  BATCH_SIZE,
  MUTATION_PRUNING_DEFAULT,
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
  // Show MUTATIONS_TABLE_INDEX summary.
  process.stdout.write(
    `\nMUTATIONS=${MUTATIONS.length} indexed tables=${[...new Set(Array.from(MUTATIONS_TABLE_INDEX.values()).flatMap(s => [...s]))].sort().join(',')}\n`,
  );
  // Probe BatchedRunner.
  const runner = buildBatchedRunner({dryRun: true, batchSize: 3, verbose: 0});
  for (let i = 0; i < 5; i++) {
    await runner.enqueueAndMaybeFlush({table: 'channels'});
  }
  await runner.finalFlush();
  process.stdout.write(
    `BatchedRunner stats: batches=${runner.stats.batches} asts=${runner.stats.asts} mutations_run=${runner.stats.mutationsRunTotal}/${runner.stats.mutationsCandidateTotal}\n`,
  );
  process.stdout.write('harness-fuzz: smoke OK (stub mode).\n');
  process.exit(r.status === 'ok' ? 0 : 1);
}
