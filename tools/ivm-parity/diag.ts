import WebSocket from 'ws';
import {encodeSecProtocols} from '../../packages/zero-protocol/src/connect.ts';

const ast = {table: 'channels'};
const hash = 'test123';
const clientSchema = {
  tables: {
    channels: {
      columns: {
        id: {type: 'string'},
        name: {type: 'string'},
        description: {type: 'string'},
        created_at: {type: 'number'},
      },
      primaryKey: ['id'],
    },
  },
};

const initBody = {
  clientSchema,
  desiredQueriesPatch: [{op: 'put' as const, hash, ast}],
};
const secProtocol = encodeSecProtocols(['initConnection', initBody], undefined);

for (const [label, port] of [
  ['TS', 4858],
  ['RS', 4868],
] as const) {
  const cgid = 'diag-' + Date.now() + '-' + label;
  const ws = new WebSocket(
    `ws://localhost:${port}/sync/v50/connect?clientGroupID=${cgid}&clientID=cid1&baseCookie=&ts=${Date.now()}&lmid=1`,
    secProtocol,
  );
  let count = 0;
  ws.on('open', () => console.log(label + ': connected'));
  ws.on('unexpected-response', (req: any, res: any) => {
    let body = '';
    res.on('data', (chunk: any) => (body += chunk));
    res.on('end', () =>
      console.log(label + ' unexpected-response:', res.statusCode, body),
    );
  });
  ws.on('message', (data: Buffer | string) => {
    count++;
    const msg = JSON.parse(String(data));
    console.log(
      label + ' msg#' + count + ':',
      JSON.stringify(msg).slice(0, 300),
    );
    if (count >= 10) ws.close();
  });
  ws.on('error', (e: Error) => console.log(label + ' error:', e.message));
  ws.on('close', () => console.log(label + ': closed'));
}
setTimeout(() => {
  console.log('timeout - exiting');
  process.exit(0);
}, 10000);
