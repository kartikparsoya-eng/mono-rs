#!/usr/bin/env npx tsx
import postgres from 'postgres';
/**
 * Minimal diagnostic: subscribe one simple channels query to both TS and RS,
 * run an INSERT, and log all WS messages to see why RS doesn't advance.
 */
import WebSocket from 'ws';
import {encodeSecProtocols} from '../../packages/zero-protocol/src/connect.ts';

const TS_URL = 'ws://localhost:4858/sync/v50/connect';
const RS_URL = 'ws://localhost:4868/sync/v50/connect';

const sql = postgres('postgresql://user:password@127.0.0.1:6434/parity');

const clientSchema = {
  tables: {
    channels: {
      columns: {
        id: {type: 'string'},
        name: {type: 'string'},
        visibility: {type: 'string'},
      },
      primaryKey: ['id'],
    },
  },
};

const ast = {table: 'channels'};
const hash = 'diag-ch-simple';

function connect(label: string, url: string): Promise<WebSocket> {
  return new Promise((resolve, reject) => {
    const initBody = {
      clientSchema,
      desiredQueriesPatch: [{op: 'put' as const, hash, ast}],
    };
    const secProtocol = encodeSecProtocols(
      ['initConnection', initBody],
      undefined,
    );
    const cgid = `diag-${label}-${Date.now()}`;
    const ws = new WebSocket(
      url +
        '?clientGroupID=' +
        cgid +
        '&clientID=cid-' +
        Date.now() +
        '&baseCookie=&ts=' +
        Date.now() +
        '&lmid=1',
      secProtocol,
    );
    ws.on('open', () => {
      console.log(`[${label}] connected (open event)`);
      resolve(ws);
    });
    ws.on('unexpected-response', (req: any, res: any) => {
      console.log(
        `[${label}] unexpected-response: ${res.statusCode} ${res.statusMessage}`,
      );
      let body = '';
      res.on('data', (d: Buffer) => (body += d.toString()));
      res.on('end', () => {
        console.log(`[${label}] response body: ${body.slice(0, 500)}`);
        reject(new Error('unexpected-response'));
      });
    });
    ws.on('message', (data: Buffer | string) => {
      const msg = String(data);
      const parsed = JSON.parse(msg);
      const tag = parsed[0];
      if (tag === 'pokePart') {
        const body = parsed[1];
        const rowsPatch = body?.rowsPatch;
        const got = body?.gotQueriesPatch;
        console.log(
          `[${label}] pokePart: rowsPatch=${rowsPatch?.length ?? 0} ops, got=${JSON.stringify(got)?.slice(0, 100)}`,
        );
        if (rowsPatch) {
          for (const op of rowsPatch) {
            console.log(
              `  [${label}] row: ${op.op} ${op.tableName} ${JSON.stringify(op.value ?? op.id)}`,
            );
          }
        }
      } else if (tag === 'pokeEnd') {
        console.log(`[${label}] pokeEnd`);
      } else if (tag === 'pokeStart') {
        console.log(`[${label}] pokeStart cookie=${JSON.stringify(parsed[1])}`);
      } else if (tag === 'error') {
        console.log(`[${label}] ERROR: ${JSON.stringify(parsed[1])}`);
      } else {
        console.log(`[${label}] ${tag}: ${msg.slice(0, 200)}`);
      }
    });
    ws.on('error', e => {
      console.log(`[${label}] ws error: ${e}`);
      reject(e);
    });
    ws.on('close', () => console.log(`[${label}] ws closed`));
  });
}

async function sleep(ms: number) {
  return new Promise(r => setTimeout(r, ms));
}

async function main() {
  // Clean up any leftover test data
  await sql`DELETE FROM channels WHERE id = 'ch-diag-1'`.catch(() => {});

  console.log('--- Connecting to TS and RS ---');
  const tsWs = await connect('TS', TS_URL);
  const rsWs = await connect('RS', RS_URL);

  console.log('--- Waiting for hydration (5s) ---');
  await sleep(5000);

  console.log('--- Running INSERT ---');
  await sql`INSERT INTO channels (id, name, visibility) VALUES ('ch-diag-1', 'diag-channel', 'public')`;
  console.log('--- INSERT done, waiting for advance pokes (10s) ---');

  await sleep(10000);

  console.log('--- Cleaning up ---');
  await sql`DELETE FROM channels WHERE id = 'ch-diag-1'`;
  await sleep(2000);

  tsWs.close();
  rsWs.close();
  await sql.end();
  console.log('--- Done ---');
}

main().catch(e => {
  console.error(e);
  process.exit(1);
});
