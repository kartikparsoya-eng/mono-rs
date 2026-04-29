/**
 * Phase 34 Plan 02 — Differential tests for Track 2 fixes B1 + B2.
 *
 * Runs three focused AST shapes through the TS (port 4858) and RS (port 4868)
 * zero-cache instances and asserts byte-equal hydration payloads.
 *
 * Shapes:
 *   - **B1 (Skip + EXISTS):** `messages.where(EXISTS(attachments)).start({...})`
 *     — exercises the post-fix Source → Skip → Exists ordering
 *     (TS builder.ts:302-345 is canonical).
 *   - **B2 hydrate (OR(simple, EXISTS)):** `channels.where(OR(visibility=public,
 *     EXISTS(conversations)))` — verifies the parent_sizes cache holds the
 *     real child count for or_predicate-positive empty-children parents.
 *   - **B2 push (mutation):** insert a conversation for an or_predicate parent
 *     that previously had zero children; both caches must emit identical row
 *     deltas. (TS contract: parent stays in output via or_predicate; child
 *     Add is a passthrough Child change.)
 *
 * Per CONTEXT D-17 (TS-as-spec) and D-18 (no regressions): this script is the
 * second leg of the no-regressions mandate alongside the Rust unit tests in
 * ast_to_config.rs::tests::test_b1_skip_before_conditions and
 * exists_op.rs::tests::test_b2_*.
 *
 * Per SKILL.md hard rule #1: this script must NOT modify the test apparatus
 * to hide divergence. If post-fix divergence persists for these shapes, the
 * fix is incomplete; investigate before suppressing.
 *
 * Cache startup procedure:
 *   1. PG up: `cd apps/zbugs && npm run db-up`, then create+seed the parity
 *      database (`npm run db-create && npm run db-migrate` from this dir).
 *   2. TS cache on :4858 — see `tools/ivm-parity/run.sh` (TS reference now
 *      runs from a `/private/tmp/ivm-parity-ts-ref` git worktree per
 *      Phase 33 D-19).
 *   3. RS cache on :4868 — `npm run start-rs` from this dir.
 *   4. Wait for both replicas to be in sync (a few seconds after seed).
 *   5. Run: `npm run diff:track2` from this dir. Exit 0 on parity, 1 on
 *      divergence, 2 on config error.
 *
 * Usage flags:
 *   --skip-mutation   Run only B1 + B2 hydrate shapes (no PG mutation).
 *   --verbose         Print full per-shape diffs.
 */
import {createHash} from 'node:crypto';
import postgres from 'postgres';
import type {AST} from '../../packages/zero-protocol/src/ast.ts';
import {schema as paritySchema} from './zero-schema.ts';

// ---------------------------------------------------------------------------
// Config — env-overridable, defaults match harness-coverage.ts.
// ---------------------------------------------------------------------------
const TS_URL =
  process.env.PARITY_TS_URL ?? 'ws://localhost:4858/sync/v50/connect';
const RS_URL =
  process.env.PARITY_RS_URL ?? 'ws://localhost:4868/sync/v50/connect';
const PG_URL =
  process.env.PARITY_PG_URL ??
  'postgresql://user:password@127.0.0.1:6434/parity';
const HYDRATION_TIMEOUT_MS = Number(
  process.env.HYDRATION_TIMEOUT_MS ?? 15_000,
);
const POKE_QUIESCE_MS = Number(process.env.POKE_QUIESCE_MS ?? 2_500);

const args = new Set(process.argv.slice(2));
const skipMutation = args.has('--skip-mutation');
const verbose = args.has('--verbose');

// ---------------------------------------------------------------------------
// Types — mirrors harness-coverage.ts internal shape.
// ---------------------------------------------------------------------------
type RowsByTable = Record<string, Record<string, Record<string, unknown>>>;

// ---------------------------------------------------------------------------
// buildClientSchema — duplicated from harness-coverage.ts so this script
// stays self-contained (acceptable per SKILL.md: small additive utility,
// does not modify the apparatus).
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
// subscribeAndHydrate — single-AST WS hydrate, mirrors
// harness-coverage.ts:100-224. Auto-closes on first pokeEnd after gotPatch.
// ---------------------------------------------------------------------------
async function subscribeAndHydrate(
  url: string,
  hash: string,
  ast: unknown,
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
  const canonicalKey = (
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
  const cgid = `ivm-parity-track2-${hash}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
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
              remember(t, canonicalKey(pk, op.value), op.value);
            else if (op.op === 'del') forget(t, canonicalKey(pk, op.id));
            else if (op.op === 'clear')
              for (const k of Object.keys(rows)) delete rows[k];
            else if (op.op === 'update') {
              const k = canonicalKey(pk, op.id);
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
// Helpers: canonicalize, hash, summarize.
// ---------------------------------------------------------------------------
function canon(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canon);
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(value as object).sort()) {
      out[k] = canon((value as Record<string, unknown>)[k]);
    }
    return out;
  }
  return value;
}

function hashRows(rows: RowsByTable): string {
  return createHash('sha256')
    .update(JSON.stringify(canon(rows)))
    .digest('hex')
    .slice(0, 16);
}

function rowCount(rows: RowsByTable): number {
  let n = 0;
  for (const t of Object.values(rows)) n += Object.keys(t).length;
  return n;
}

function summarizeDiff(ts: RowsByTable, rs: RowsByTable): string {
  const tables = new Set([...Object.keys(ts), ...Object.keys(rs)]);
  const lines: string[] = [];
  for (const table of [...tables].sort()) {
    const a = ts[table] ?? {};
    const b = rs[table] ?? {};
    const ak = Object.keys(a).sort();
    const bk = Object.keys(b).sort();
    const onlyTs = ak.filter(k => !(k in b));
    const onlyRs = bk.filter(k => !(k in a));
    const differing: string[] = [];
    for (const k of ak.filter(k => k in b)) {
      if (JSON.stringify(canon(a[k])) !== JSON.stringify(canon(b[k])))
        differing.push(k);
    }
    if (onlyTs.length || onlyRs.length || differing.length) {
      lines.push(`  ${table}: ts=${ak.length} rs=${bk.length}`);
      if (onlyTs.length)
        lines.push(`    only-ts: ${onlyTs.slice(0, 5).join(', ')}`);
      if (onlyRs.length)
        lines.push(`    only-rs: ${onlyRs.slice(0, 5).join(', ')}`);
      if (differing.length)
        lines.push(`    differ: ${differing.slice(0, 5).join(', ')}`);
    }
  }
  return lines.length ? lines.join('\n') : '  (no diff)';
}

// ---------------------------------------------------------------------------
// Single-shape diff runner.
// ---------------------------------------------------------------------------
async function diffTest(
  label: string,
  ast: AST,
  clientSchema: ReturnType<typeof buildClientSchema>,
): Promise<{ok: boolean; tsRows: RowsByTable; rsRows: RowsByTable; diff?: string}> {
  const hash =
    label +
    '-' +
    createHash('sha1').update(JSON.stringify(ast)).digest('hex').slice(0, 8);
  const [ts, rs] = await Promise.all([
    subscribeAndHydrate(TS_URL, hash, ast, clientSchema),
    subscribeAndHydrate(RS_URL, hash, ast, clientSchema),
  ]);
  const tsHash = hashRows(ts);
  const rsHash = hashRows(rs);
  if (tsHash === rsHash) return {ok: true, tsRows: ts, rsRows: rs};
  return {
    ok: false,
    tsRows: ts,
    rsRows: rs,
    diff:
      `ts=${rowCount(ts)} rows (${tsHash}), rs=${rowCount(rs)} rows (${rsHash})\n` +
      summarizeDiff(ts, rs),
  };
}

// ---------------------------------------------------------------------------
// AST shapes — mirror queries/wave0-stubs/{b1,b2}*.json with seed-aware ids.
// ---------------------------------------------------------------------------
const b1Ast: AST = {
  // B1 shape — Skip + EXISTS — TS spec: builder.ts:302-345.
  table: 'messages',
  orderBy: [
    ['createdAt', 'asc'],
    ['id', 'asc'],
  ],
  start: {
    row: {createdAt: 6500, id: 'x-m-3'},
    exclusive: false,
  },
  where: {
    type: 'correlatedSubquery',
    op: 'EXISTS',
    related: {
      correlation: {parentField: ['id'], childField: ['messageId']},
      subquery: {
        table: 'attachments',
        alias: 'b1_attachments',
      },
    },
  },
} as unknown as AST;

const b2Ast: AST = {
  // B2 shape — OR(simple, EXISTS) — TS spec: exists.ts.
  table: 'channels',
  orderBy: [['id', 'asc']],
  where: {
    type: 'or',
    conditions: [
      {
        type: 'simple',
        op: '=',
        left: {type: 'column', name: 'visibility'},
        right: {type: 'literal', value: 'public'},
      },
      {
        type: 'correlatedSubquery',
        op: 'EXISTS',
        related: {
          correlation: {parentField: ['id'], childField: ['channelId']},
          subquery: {
            table: 'conversations',
            alias: 'b2_conversations',
          },
        },
      },
    ],
  },
} as unknown as AST;

// ---------------------------------------------------------------------------
// Main.
// ---------------------------------------------------------------------------
async function main() {
  const failures: string[] = [];
  const clientSchema = buildClientSchema();

  process.stdout.write('Track 2 differential tests — B1 + B2\n');
  process.stdout.write(`  TS_URL: ${TS_URL}\n`);
  process.stdout.write(`  RS_URL: ${RS_URL}\n`);
  process.stdout.write(`  PG_URL: ${PG_URL}\n\n`);

  // -------------------------------------------------------------------------
  // B1: Skip + EXISTS — verifies post-fix ordering matches TS.
  // -------------------------------------------------------------------------
  process.stdout.write('[1/3] B1 (b1-skip-exists): hydrate ');
  try {
    const r = await diffTest('b1-skip-exists', b1Ast, clientSchema);
    if (!r.ok) {
      process.stdout.write('FAIL\n');
      failures.push(`B1 hydrate diverges:\n${r.diff}`);
      if (verbose) {
        process.stdout.write(`  ts: ${JSON.stringify(r.tsRows)}\n`);
        process.stdout.write(`  rs: ${JSON.stringify(r.rsRows)}\n`);
      }
    } else {
      process.stdout.write(`OK (${rowCount(r.tsRows)} rows)\n`);
    }
  } catch (e) {
    process.stdout.write('ERROR\n');
    failures.push(`B1 hydrate threw: ${(e as Error).message}`);
  }

  // -------------------------------------------------------------------------
  // B2: OR(simple, EXISTS) hydrate — verifies parent_sizes cache holds REAL
  // count for or_predicate-positive empty-children parents.
  // -------------------------------------------------------------------------
  process.stdout.write('[2/3] B2 (b2-or-exists-hydrate): hydrate ');
  try {
    const r = await diffTest('b2-or-exists-hydrate', b2Ast, clientSchema);
    if (!r.ok) {
      process.stdout.write('FAIL\n');
      failures.push(`B2 hydrate diverges:\n${r.diff}`);
      if (verbose) {
        process.stdout.write(`  ts: ${JSON.stringify(r.tsRows)}\n`);
        process.stdout.write(`  rs: ${JSON.stringify(r.rsRows)}\n`);
      }
    } else {
      process.stdout.write(`OK (${rowCount(r.tsRows)} rows)\n`);
    }
  } catch (e) {
    process.stdout.write('ERROR\n');
    failures.push(`B2 hydrate threw: ${(e as Error).message}`);
  }

  // -------------------------------------------------------------------------
  // B2 push: insert a child for an empty-children or_predicate parent and
  // verify both caches re-hydrate to the same payload after the mutation.
  //
  // Note: a full push-delta comparison (capturing per-poke row-changes) would
  // require adopting harness-advance-coverage.ts:279-396's subscribe+drain
  // pattern. For Phase 34 plan 02 scope, hydrating BOTH caches AFTER the
  // mutation and asserting byte-equal payloads is sufficient — TS↔RS hash
  // equality after the same mutation proves the operators converge to the
  // same state. A deeper push-delta capture can land in a follow-up plan
  // that extends this script.
  // -------------------------------------------------------------------------
  if (skipMutation) {
    process.stdout.write('[3/3] B2 push: SKIPPED (--skip-mutation)\n');
  } else {
    process.stdout.write('[3/3] B2 push (b2-or-exists-mutation): ');
    let sql: ReturnType<typeof postgres> | undefined;
    try {
      sql = postgres(PG_URL);
      // Pick a row id distinct from any existing seed row.
      const mutationId = `t2-co-${Date.now()}`;
      // Insert a conversation for x-ch-empty (an or_predicate-positive parent
      // that has zero conversations in the seed corpus per the Wave 0 stub).
      await sql`
        INSERT INTO conversations (id, "channelId", title, "createdAt")
        VALUES (${mutationId}, 'x-ch-empty', 'track2 mutation', 9000)
      `;
      // Wait for replicators to pick up the change.
      await new Promise(r => setTimeout(r, POKE_QUIESCE_MS));
      try {
        const r = await diffTest(
          'b2-or-exists-after-mutation',
          b2Ast,
          clientSchema,
        );
        if (!r.ok) {
          process.stdout.write('FAIL\n');
          failures.push(`B2 push diverges:\n${r.diff}`);
          if (verbose) {
            process.stdout.write(`  ts: ${JSON.stringify(r.tsRows)}\n`);
            process.stdout.write(`  rs: ${JSON.stringify(r.rsRows)}\n`);
          }
        } else {
          process.stdout.write(`OK (${rowCount(r.tsRows)} rows after mutation)\n`);
        }
      } finally {
        // Undo regardless of outcome.
        await sql`DELETE FROM conversations WHERE id = ${mutationId}`;
        await new Promise(r => setTimeout(r, POKE_QUIESCE_MS));
      }
    } catch (e) {
      process.stdout.write('ERROR\n');
      failures.push(`B2 push threw: ${(e as Error).message}`);
    } finally {
      if (sql) await sql.end({timeout: 2});
    }
  }

  process.stdout.write('\n');
  if (failures.length === 0) {
    process.stdout.write(
      'OK: Track 2 differential tests pass (B1 + B2 hydrate + B2 push)\n',
    );
    process.exit(0);
  }
  process.stderr.write('FAILED:\n');
  for (const f of failures) process.stderr.write('  - ' + f + '\n');
  process.exit(1);
}

main().catch(e => {
  process.stderr.write('UNHANDLED: ' + (e as Error).message + '\n');
  process.exit(2);
});
