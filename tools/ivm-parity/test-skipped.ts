import {readFileSync} from 'fs';
import WebSocket from 'ws';
import {encodeSecProtocols} from '../../packages/zero-protocol/src/connect.ts';
import {schema as paritySchema} from './zero-schema.ts';

const corpus = JSON.parse(
  readFileSync(new URL('./ast_corpus.json', import.meta.url), 'utf8'),
);

function buildClientSchema() {
  const out: Record<string, any> = {};
  for (const [tableName, tableDef] of Object.entries(
    paritySchema.tables,
  ) as any) {
    const columns: Record<string, any> = {};
    for (const [col, desc] of Object.entries(tableDef.columns) as any) {
      columns[col] = {type: desc.type};
    }
    out[tableName] = {columns, primaryKey: [...tableDef.primaryKey]};
  }
  return {tables: out};
}

async function testOne(id: string, url: string): Promise<string> {
  const entry = corpus.find((e: any) => e.id === id);
  if (!entry) return `${id}: NOT FOUND`;
  const hash = entry.id;
  const clientSchema = buildClientSchema();
  const initBody = {
    clientSchema,
    desiredQueriesPatch: [{op: 'put' as const, hash, ast: entry.ast}],
  };
  const secProtocol = encodeSecProtocols(
    ['initConnection', initBody],
    undefined,
  );
  const cgid = `test-${id}-${Date.now()}`;
  const ws = new WebSocket(
    `${url}?clientGroupID=${cgid}&clientID=cid-test&baseCookie=&ts=${Date.now()}&lmid=1`,
    secProtocol,
  );
  return new Promise<string>(resolve => {
    const timer = setTimeout(() => {
      ws.close();
      resolve(`${id}: TIMEOUT (10s)`);
    }, 10000);
    let gotPatch = false;
    const messages: string[] = [];
    ws.on('message', (data: any) => {
      const parsed = JSON.parse(String(data));
      if (!Array.isArray(parsed)) return;
      const [tag, body] = parsed;
      messages.push(tag);
      if (tag === 'error') {
        clearTimeout(timer);
        ws.close();
        resolve(`${id}: ERROR: ${JSON.stringify(body).slice(0, 200)}`);
        return;
      }
      if (tag === 'pokePart') {
        const got = body?.gotQueriesPatch;
        if (
          Array.isArray(got) &&
          got.some((g: any) => g.hash === hash && g.op === 'put')
        )
          gotPatch = true;
      }
      if (tag === 'pokeEnd' && gotPatch) {
        clearTimeout(timer);
        ws.close();
        resolve(`${id}: OK (hydrated, ${messages.length} messages)`);
      }
    });
    ws.on('error', (err: any) => {
      clearTimeout(timer);
      resolve(`${id}: WS_ERROR: ${err.message}`);
    });
    ws.on('close', () => {
      clearTimeout(timer);
      if (!gotPatch)
        resolve(`${id}: CLOSED before hydration (msgs: ${messages.join(',')})`);
    });
  });
}

const url = process.argv[2] || 'ws://localhost:4858/sync/v50/connect';
console.log(`Testing against ${url}...`);

const ids = ['fuzz_00079', 'fuzz_00161', 'fuzz_00166'];
const labels = ['correlatedSubquery', 'related+limit', 'nested related'];

for (let i = 0; i < ids.length; i++) {
  console.log(`\n--- ${labels[i]} ---`);
  const result = await testOne(ids[i], url);
  console.log(result);
}

console.log('\nDone');
process.exit(0);
