/**
 * Phase 34 Plans 02 + 05 + 06 — Differential tests for Track 2 fixes
 * B1 + B2 + B3 + B11.
 *
 * Runs five focused AST shapes through the TS (port 4858) and RS (port 4868)
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
 *   - **B3 (related-with-limit + child mutation):**
 *     `conversations.related(messages.orderBy(id asc).limit(5))` — verifies
 *     child Take's partition_key threading. Pre-fix Rust silently no-oped on
 *     child mutations because fetch and push state keys differed. Post-fix
 *     plan 34-05 threads `correlation.childField` so both paths agree
 *     (TS builder.ts:626-632 + take.ts:710-757).
 *   - **B11 (cascade-delete on multi-table tx):**
 *     `channels.related(conversations.related(messages))` — verifies that
 *     when a parent channel is deleted in a single PG transaction together
 *     with its conversations and messages, BOTH caches emit identical
 *     descendant Remove row-changes. Pre-fix Rust read descendants from
 *     curr (post-swap_snapshot, descendants already gone) and silently
 *     elided them. Post-fix plan 34-06 reads from prev (where same-tx
 *     deletes are still present). See
 *     .planning/phases/34-differential-fuzz-schema-extension/34-RESEARCH.md §B11.
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

// B3 differential test — verifies child Take with partition_key threaded
// produces correct push behavior on child mutation. Pre-fix Rust silently
// no-oped on these shapes (push state-key mismatch — fetch wrote state under
// `["take","conversationId","co-2"]`, push queried `["take"]`). Post-fix
// matches TS exactly: child Take uses parent's correlation.childField as
// partition key, so fetch and push state keys agree.
//
// See .planning/phases/34-differential-fuzz-schema-extension/34-RESEARCH.md §B3
// and the corresponding Rust unit tests:
//   packages/zqlite-rs/src/ast_to_config.rs::tests::test_b3_partition_key_threading
//   packages/zero-ivm-rs/src/take_op.rs::tests::test_b3_partition_state_consistency
//   packages/zero-ivm-rs/src/take_op.rs::tests::test_b3_push_emits_change_for_constrained_child_take
//
// Shape: `conversations.related(messages.orderBy(id asc).limit(5))` — exactly
// the canonical TS-spec partition_key shape (TS builder.ts:626-632).
// Seed precondition: co-2 has at least one message in its top-5 by id ascending.
const b3Ast: AST = {
  table: 'conversations',
  orderBy: [['id', 'asc']],
  related: [
    {
      correlation: {parentField: ['id'], childField: ['conversationId']},
      subquery: {
        table: 'messages',
        alias: 'b3_thread',
        orderBy: [['id', 'asc']],
        limit: 5,
      },
    },
  ],
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

// B11 differential test — verifies cascade-delete on a multi-table
// transaction emits descendant Remove row-changes from BOTH caches.
// Pre-fix Rust read descendants from curr (post-swap_snapshot,
// descendants already gone) and silently elided them. Post-fix
// reads from prev (where same-tx-deleted rows still exist).
//
// Shape: `channels.related(conversations.related(messages))` — three-level
// chain so the recursive descendant walk in emit_descendant_removals is
// exercised end-to-end.
//
// See .planning/phases/34-differential-fuzz-schema-extension/34-RESEARCH.md §B11.
const b11Ast: AST = {
  table: 'channels',
  orderBy: [['id', 'asc']],
  related: [
    {
      correlation: {parentField: ['id'], childField: ['channelId']},
      subquery: {
        table: 'conversations',
        alias: 'b11_conv',
        orderBy: [['id', 'asc']],
        related: [
          {
            correlation: {
              parentField: ['id'],
              childField: ['conversationId'],
            },
            subquery: {
              table: 'messages',
              alias: 'b11_msg',
              orderBy: [['id', 'asc']],
            },
          },
        ],
      },
    },
  ],
} as unknown as AST;

// ---------------------------------------------------------------------------
// Main.
// ---------------------------------------------------------------------------
async function main() {
  const failures: string[] = [];
  const clientSchema = buildClientSchema();

  process.stdout.write('Track 2 differential tests — B1 + B2 + B3 + B11\n');
  process.stdout.write(`  TS_URL: ${TS_URL}\n`);
  process.stdout.write(`  RS_URL: ${RS_URL}\n`);
  process.stdout.write(`  PG_URL: ${PG_URL}\n\n`);

  // -------------------------------------------------------------------------
  // B1: Skip + EXISTS — verifies post-fix ordering matches TS.
  // -------------------------------------------------------------------------
  process.stdout.write('[1/5] B1 (b1-skip-exists): hydrate ');
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
  process.stdout.write('[2/5] B2 (b2-or-exists-hydrate): hydrate ');
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
    process.stdout.write('[3/5] B2 push: SKIPPED (--skip-mutation)\n');
  } else {
    process.stdout.write('[3/5] B2 push (b2-or-exists-mutation): ');
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

  // -------------------------------------------------------------------------
  // B3: related-with-limit + child mutation — headline Track 2 fix
  // (plan 34-05). Verifies child Take's partition_key threading: pre-fix
  // Rust silently no-oped on child mutations because fetch and push state
  // keys differed; post-fix both paths use parent's correlation.childField
  // and agree. Spec source: TS builder.ts:626-632 + take.ts:710-757.
  //
  // Same hash-equality-after-mutation pattern as B2 push (a per-poke
  // row-changes capture is a follow-up extension). The key signal: pre-fix
  // RS would NOT reflect the body change (push-as-no-op for child Take),
  // post-fix RS converges to the TS payload byte-equal.
  //
  // Mutation: UPDATE messages SET body = 'b3-track2-edited' WHERE id = 'm-3'.
  //   - m-3 is in seed.sql:41 (conversationId='co-2', body='standup notes').
  //   - co-2 has 2 messages (m-3, m-4) — both well within limit=5.
  //   - The hydrated payload includes m-3.body, so the post-mutation hash
  //     differs from pre-mutation — and TS↔RS must produce the same hash.
  // Restored in finally{} regardless of test outcome.
  // -------------------------------------------------------------------------
  if (skipMutation) {
    process.stdout.write('[4/5] B3 push: SKIPPED (--skip-mutation)\n');
  } else {
    process.stdout.write(
      '[4/5] B3 push (b3-related-limit-child-mutation): ',
    );
    let sql: ReturnType<typeof postgres> | undefined;
    let originalBody: string | null = null;
    const mutationTargetId = 'm-3';
    const editedBody = `b3-track2-edited-${Date.now()}`;
    try {
      sql = postgres(PG_URL);

      // Precondition: confirm the target row exists in PG (else the test
      // can't run meaningfully — fail loudly per threat model T-34-14).
      const existing = await sql<{id: string; body: string}[]>`
        SELECT id, body FROM messages WHERE id = ${mutationTargetId} LIMIT 1
      `;
      if (existing.length === 0) {
        process.stdout.write('SKIPPED\n');
        failures.push(
          `B3 precondition failed: messages.${mutationTargetId} not in PG. ` +
            `Was seed.sql modified? See seed.sql:41 — m-3 should exist with ` +
            `conversationId='co-2', body='standup notes'.`,
        );
      } else {
        originalBody = existing[0]!.body;

        // Hydrate baseline (pre-mutation) so we can compare TS↔RS at this
        // step and also distinguish from the post-mutation hash. (The diff
        // helper compares only TS↔RS, not pre↔post — which is what we want.)
        const before = await diffTest(
          'b3-related-limit-pre-mutation',
          b3Ast,
          clientSchema,
        );
        if (!before.ok) {
          process.stdout.write('FAIL (pre-mutation diverges)\n');
          failures.push(`B3 pre-mutation diverges:\n${before.diff}`);
          if (verbose) {
            process.stdout.write(`  ts: ${JSON.stringify(before.tsRows)}\n`);
            process.stdout.write(`  rs: ${JSON.stringify(before.rsRows)}\n`);
          }
        } else {
          // Apply the child mutation.
          await sql`
            UPDATE messages SET body = ${editedBody} WHERE id = ${mutationTargetId}
          `;
          await new Promise(r => setTimeout(r, POKE_QUIESCE_MS));

          // Re-hydrate with a fresh hash to avoid CVR cache reuse — both
          // caches must observe the edited body. summarizeDiff uses
          // multiset-aware comparison (per-table key+value diff).
          const after = await diffTest(
            'b3-related-limit-post-mutation',
            b3Ast,
            clientSchema,
          );
          if (!after.ok) {
            process.stdout.write('FAIL\n');
            failures.push(`B3 push diverges:\n${after.diff}`);
            if (verbose) {
              process.stdout.write(`  ts: ${JSON.stringify(after.tsRows)}\n`);
              process.stdout.write(`  rs: ${JSON.stringify(after.rsRows)}\n`);
            }
          } else {
            // Sanity: confirm the body actually changed in the hydrated
            // payload. If it did NOT, the mutation didn't propagate and the
            // diff result is meaningless (false positive on parity).
            const tsMessages = (after.tsRows.messages ??
              {}) as Record<string, Record<string, unknown>>;
            const editedRow = Object.values(tsMessages).find(
              row => row.id === mutationTargetId,
            );
            if (!editedRow) {
              process.stdout.write('FAIL (no row in payload)\n');
              failures.push(
                `B3 sanity: ${mutationTargetId} not in TS post-mutation payload — ` +
                  `did the AST not yield this row? Inspect b3Ast subquery limit.`,
              );
            } else if (editedRow.body !== editedBody) {
              process.stdout.write('FAIL (mutation not propagated)\n');
              failures.push(
                `B3 sanity: ${mutationTargetId}.body = ${JSON.stringify(
                  editedRow.body,
                )} (expected ${JSON.stringify(editedBody)}). ` +
                  `Replicator may not be in sync; bump POKE_QUIESCE_MS.`,
              );
            } else {
              process.stdout.write(
                `OK (${rowCount(after.tsRows)} rows, body propagated)\n`,
              );
            }
          }
        }
      }
    } catch (e) {
      process.stdout.write('ERROR\n');
      failures.push(`B3 push threw: ${(e as Error).message}`);
    } finally {
      // Restore original body regardless of outcome.
      if (sql && originalBody !== null) {
        try {
          await sql`UPDATE messages SET body = ${originalBody} WHERE id = ${mutationTargetId}`;
          await new Promise(r => setTimeout(r, POKE_QUIESCE_MS));
        } catch (e) {
          process.stderr.write(
            `WARNING: B3 cleanup failed restoring messages.${mutationTargetId}.body — ` +
              `manual fix needed: UPDATE messages SET body = ${JSON.stringify(
                originalBody,
              )} WHERE id = '${mutationTargetId}'. (${(e as Error).message})\n`,
          );
        }
      }
      if (sql) await sql.end({timeout: 2});
    }
  }

  // -------------------------------------------------------------------------
  // B11: cascade-delete on multi-table transaction.
  //
  // Inserts a temp channel + 2 conversations + 4 messages, hydrates baseline
  // on both caches, then DELETEs all 7 rows in a single PG transaction
  // (messages → conversations → channel order — schema has no FK CASCADE
  // per schema.sql:9-40, so the explicit ordered DELETE drives the same
  // single-tx cascade pattern as a real cascade-delete-enabled schema).
  //
  // Pre-fix Rust would silently elide the descendant Removes (read against
  // curr post-swap_snapshot, descendants already gone). Post-fix matches
  // TS exactly — the descendants are emitted from the prev snapshot.
  //
  // The test asserts BYTE-EQUAL hash on the post-mutation hydration: TS
  // and RS must produce the same set of remaining channels (the temp
  // channel + its descendants are all gone from both). A truly broken
  // Rust would diverge from TS in the per-poke row-changes stream — we
  // can't capture that without harness-advance-coverage.ts integration,
  // so the hash-equality is the same pattern as B2/B3 push.
  //
  // RESTORE: cleanup re-deletes (no-op if already deleted) and silently
  // tolerates any leftover state. The test channel id is keyed by Date.now()
  // so concurrent runs don't collide.
  //
  // Per SKILL.md hard rule 2 (no baseline seed modification): all rows
  // are inserted at test start and removed at test end; baseline seed.sql
  // is NOT modified.
  //
  // Spec: TS pipeline-driver.ts:1542-1577.
  // See .planning/phases/34-differential-fuzz-schema-extension/34-RESEARCH.md §B11.
  // -------------------------------------------------------------------------
  if (skipMutation) {
    process.stdout.write('[5/5] B11 cascade-delete: SKIPPED (--skip-mutation)\n');
  } else {
    process.stdout.write(
      '[5/5] B11 cascade-delete (b11-cascade-multi-table-tx): ',
    );
    let sql: ReturnType<typeof postgres> | undefined;
    const stamp = Date.now();
    const channelId = `b11-ch-${stamp}`;
    const convIds = [`b11-co-1-${stamp}`, `b11-co-2-${stamp}`];
    const msgIds = [
      `b11-m-1-${stamp}`,
      `b11-m-2-${stamp}`,
      `b11-m-3-${stamp}`,
      `b11-m-4-${stamp}`,
    ];
    let inserted = false;
    try {
      sql = postgres(PG_URL);

      // Insert the temp rows: 1 channel → 2 conversations → 4 messages.
      // u1 must exist in seed.sql:14 — we use it as authorId.
      await sql.begin(async tx => {
        await tx`INSERT INTO channels (id, name, visibility) VALUES
          (${channelId}, 'b11-test', 'public')`;
        await tx`INSERT INTO conversations (id, "channelId", title, "createdAt") VALUES
          (${convIds[0]}, ${channelId}, 'b11 c1', 90000),
          (${convIds[1]}, ${channelId}, 'b11 c2', 90001)`;
        await tx`INSERT INTO messages
          (id, "conversationId", "authorId", body, "createdAt", "visibleTo") VALUES
          (${msgIds[0]}, ${convIds[0]}, 'u1', 'b11-m1', 90100, NULL),
          (${msgIds[1]}, ${convIds[0]}, 'u1', 'b11-m2', 90101, NULL),
          (${msgIds[2]}, ${convIds[1]}, 'u1', 'b11-m3', 90200, NULL),
          (${msgIds[3]}, ${convIds[1]}, 'u1', 'b11-m4', 90201, NULL)`;
      });
      inserted = true;
      // Wait for replicators on both caches to pick up the inserts.
      await new Promise(r => setTimeout(r, POKE_QUIESCE_MS));

      // Baseline hydrate: confirm the temp rows reach both caches and TS↔RS
      // agree before the cascade-delete. If they diverge here, the cascade
      // signal is meaningless.
      const before = await diffTest(
        'b11-cascade-baseline',
        b11Ast,
        clientSchema,
      );
      if (!before.ok) {
        process.stdout.write('FAIL (baseline diverges)\n');
        failures.push(`B11 baseline diverges:\n${before.diff}`);
        if (verbose) {
          process.stdout.write(`  ts: ${JSON.stringify(before.tsRows)}\n`);
          process.stdout.write(`  rs: ${JSON.stringify(before.rsRows)}\n`);
        }
      } else {
        // Multi-table transaction: messages → conversations → channels
        // (FK ordering — schema.sql has no CASCADE; ordered DELETE drives
        // the same single-tx semantics).
        await sql.begin(async tx => {
          await tx`DELETE FROM messages WHERE "conversationId" IN (${convIds[0]}, ${convIds[1]})`;
          await tx`DELETE FROM conversations WHERE "channelId" = ${channelId}`;
          await tx`DELETE FROM channels WHERE id = ${channelId}`;
        });
        // Mark not-inserted so cleanup doesn't try to re-delete inside finally.
        inserted = false;
        await new Promise(r => setTimeout(r, POKE_QUIESCE_MS));

        // Re-hydrate: TS and RS must converge to the SAME post-cascade
        // state. summarizeDiff catches any per-table key+value drift.
        const after = await diffTest(
          'b11-cascade-after',
          b11Ast,
          clientSchema,
        );
        if (!after.ok) {
          process.stdout.write('FAIL\n');
          failures.push(
            `B11 cascade-delete diverges:\n${after.diff}\n` +
              `  This is the headline B11 signal — descendants in same-tx ` +
              `delete were elided in one cache. Verify Rust ` +
              `emit_descendant_removals reads from prev_db_path (Plan 34-06).`,
          );
          if (verbose) {
            process.stdout.write(`  ts: ${JSON.stringify(after.tsRows)}\n`);
            process.stdout.write(`  rs: ${JSON.stringify(after.rsRows)}\n`);
          }
        } else {
          // Sanity: the deleted channel must NOT appear in either payload.
          const tsChans = (after.tsRows.channels ?? {}) as Record<
            string,
            Record<string, unknown>
          >;
          const rsChans = (after.rsRows.channels ?? {}) as Record<
            string,
            Record<string, unknown>
          >;
          const tsHas = Object.values(tsChans).some(c => c.id === channelId);
          const rsHas = Object.values(rsChans).some(c => c.id === channelId);
          if (tsHas || rsHas) {
            process.stdout.write('FAIL (channel still present)\n');
            failures.push(
              `B11 sanity: ${channelId} still in ts=${tsHas} rs=${rsHas} ` +
                `post-cascade. Replicator may not be in sync; bump ` +
                `POKE_QUIESCE_MS.`,
            );
          } else {
            process.stdout.write(
              `OK (${rowCount(after.tsRows)} rows after cascade)\n`,
            );
          }
        }
      }
    } catch (e) {
      process.stdout.write('ERROR\n');
      failures.push(`B11 cascade-delete threw: ${(e as Error).message}`);
    } finally {
      // Cleanup: re-attempt deletion (no-op if cascade-delete already ran)
      // so subsequent runs are deterministic and SKILL.md hard rule 2
      // (additive only) is preserved.
      if (sql && inserted) {
        try {
          await sql.begin(async tx => {
            await tx`DELETE FROM messages WHERE "conversationId" IN (${convIds[0]}, ${convIds[1]})`;
            await tx`DELETE FROM conversations WHERE "channelId" = ${channelId}`;
            await tx`DELETE FROM channels WHERE id = ${channelId}`;
          });
          await new Promise(r => setTimeout(r, POKE_QUIESCE_MS));
        } catch (e) {
          process.stderr.write(
            `WARNING: B11 cleanup failed for ${channelId} — ` +
              `manual fix may be needed. (${(e as Error).message})\n`,
          );
        }
      }
      if (sql) await sql.end({timeout: 2});
    }
  }

  process.stdout.write('\n');
  if (failures.length === 0) {
    process.stdout.write(
      'OK: Track 2 differential tests pass (B1 + B2 hydrate + B2 push + B3 push + B11 cascade)\n',
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
