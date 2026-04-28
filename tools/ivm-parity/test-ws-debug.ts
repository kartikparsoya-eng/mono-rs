/**
 * Minimal WebSocket debug script.
 * Connects to a zero-cache server and logs every message to understand
 * why the harness sees 0 rows.
 */
import WebSocket from 'ws';
import {encodeSecProtocols} from '../../packages/zero-protocol/src/connect.ts';
import {schema as paritySchema} from './zero-schema.ts';

const PORT = process.env.PORT ?? '4858';
const URL = `ws://localhost:${PORT}/sync/v50/connect`;

function buildClientSchema() {
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

const clientSchema = buildClientSchema();
console.log('clientSchema:', JSON.stringify(clientSchema, null, 2));

const ast = {table: 'channels'};
const hash = 'test-hash-001';

const initBody = {
  clientSchema,
  desiredQueriesPatch: [{op: 'put' as const, hash, ast}],
};

const secProtocol = encodeSecProtocols(['initConnection', initBody], undefined);

const cgid = `debug-${Date.now()}`;
const wsUrl =
  URL +
  `?clientGroupID=${cgid}&clientID=cid-debug&baseCookie=&ts=${Date.now()}&lmid=1`;
console.log('Connecting to:', wsUrl);

const ws = new WebSocket(wsUrl, secProtocol);

let msgCount = 0;
let pokeEndCount = 0;
let gotRows = false;
ws.on('open', () => console.log('WS open'));
ws.on('message', (data: Buffer | string) => {
  msgCount++;
  const str = String(data);
  let parsed: unknown;
  try {
    parsed = JSON.parse(str);
  } catch {
    parsed = str;
  }

  if (Array.isArray(parsed)) {
    const [tag, body] = parsed;
    console.log(`\n--- MSG #${msgCount}: tag=${tag} ---`);
    if (tag === 'pokePart') {
      const gqp = (body as any)?.gotQueriesPatch;
      const rp = (body as any)?.rowsPatch;
      console.log('  gotQueriesPatch:', JSON.stringify(gqp));
      console.log(
        '  rowsPatch length:',
        Array.isArray(rp) ? rp.length : 'not array',
      );
      if (Array.isArray(rp) && rp.length > 0) {
        gotRows = true;
        console.log('  rowsPatch[0]:', JSON.stringify(rp[0]));
        if (rp.length > 1)
          console.log('  rowsPatch[1]:', JSON.stringify(rp[1]));
      }
      console.log('  body keys:', Object.keys(body as any));
    } else if (tag === 'pokeEnd') {
      console.log('  pokeEnd body:', JSON.stringify(body));
    } else {
      console.log('  body:', JSON.stringify(body).slice(0, 500));
    }
  } else {
    console.log(`\n--- MSG #${msgCount} (non-array): ${str.slice(0, 500)}`);
  }

  if (Array.isArray(parsed) && parsed[0] === 'pokeEnd') {
    pokeEndCount++;
    if (gotRows) {
      setTimeout(() => {
        console.log(`\nTotal messages: ${msgCount}`);
        ws.close();
        process.exit(0);
      }, 1000);
    } else {
      console.log(`  (pokeEnd #${pokeEndCount}, no rows yet - waiting...)`);
    }
  }
});

ws.on('error', (err: Error) => {
  console.error('WS error:', err);
  process.exit(1);
});
ws.on('close', (code: number, reason: Buffer) => {
  console.log(`WS closed: code=${code} reason=${reason}`);
});

setTimeout(() => {
  console.log(
    `Timeout - ${msgCount} msgs, ${pokeEndCount} pokeEnds, gotRows=${gotRows}`,
  );
  ws.close();
  process.exit(1);
}, 30000);
