import {createRequire} from 'node:module';
/**
 * Quick standalone parallel push POC — no vitest, just runs and prints times.
 * Usage: npx tsx packages/zero-cache/src/services/view-syncer/parallel-push-quick.ts
 */
import {cpus} from 'node:os';
import {fileURLToPath} from 'node:url';
import {Worker} from 'node:worker_threads';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import {createSchema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
  boolean,
  number,
  string,
  table,
} from '../../../../zero-schema/src/builder/table-builder.ts';
import {
  CREATE_STORAGE_TABLE,
  DatabaseStorage,
} from '../../../../zqlite/src/database-storage.ts';
import {Database} from '../../../../zqlite/src/db.ts';
import {listTables} from '../../db/lite-tables.ts';
import {InspectorDelegate} from '../../server/inspector-delegate.ts';
import {DbFile} from '../../test/lite.ts';
import {upstreamSchema, type ShardID} from '../../types/shards.ts';
import {populateFromExistingTables} from '../replicator/schema/column-metadata.ts';
import {initReplicationState} from '../replicator/schema/replication-state.ts';
import {fakeReplicator, ReplicationMessages} from '../replicator/test-utils.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NUM_WORKERS = Math.min(cpus().length, 8);
const NUM_PIPELINES = 50;
const NUM_ROWS = 500;
const NUM_INSERTS = 100;

const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

const issuesT = table('issues')
  .columns({id: string(), closed: boolean()})
  .primaryKey('id');
const commentsT = table('comments')
  .columns({id: string(), issueID: string(), upvotes: number()})
  .primaryKey('id');
const issueLabelsT = table('issueLabels')
  .columns({issueID: string(), labelID: string(), legacyID: string()})
  .primaryKey('issueID', 'labelID');
const labelsT = table('labels')
  .columns({id: string(), name: string()})
  .primaryKey('id');

const clientSchema = createSchema({
  tables: [issuesT, commentsT, issueLabelsT, labelsT],
});

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

const QUERY_TEMPLATES: AST[] = [
  {
    table: 'issues',
    orderBy: [['id', 'desc']],
    related: [
      {
        system: 'client',
        correlation: {parentField: ['id'], childField: ['issueID']},
        subquery: {
          table: 'comments',
          alias: 'comments',
          orderBy: [['id', 'desc']],
        },
      },
    ],
  },
  {
    table: 'issues',
    orderBy: [['id', 'asc']],
    where: {
      type: 'simple',
      left: {type: 'column', name: 'closed'},
      op: '=',
      right: {type: 'literal', value: false},
    },
  },
  {
    table: 'issues',
    orderBy: [['id', 'asc']],
    limit: 10,
  },
  {
    table: 'comments',
    orderBy: [['id', 'asc']],
    where: {
      type: 'simple',
      left: {type: 'column', name: 'upvotes'},
      op: '>',
      right: {type: 'literal', value: 50},
    },
  },
];

function createDB() {
  const lc = createSilentLogContext();
  const dbFile = new DbFile('parallel_push_bench');
  dbFile.connect(lc).pragma('journal_mode = wal2');
  const db = dbFile.connect(lc);
  initReplicationState(db, ['zero_data'], '01');
  db.exec(/*sql*/ `
    CREATE TABLE "${mutationsTableName}" (
      "clientGroupID" TEXT, "clientID" TEXT, "mutationID" INTEGER, "result" TEXT,
      _0_version TEXT NOT NULL, PRIMARY KEY ("clientGroupID", "clientID", "mutationID")
    );
    CREATE TABLE issues (id TEXT PRIMARY KEY, closed BOOL, _0_version TEXT NOT NULL);
    CREATE TABLE comments (id TEXT PRIMARY KEY, issueID TEXT, upvotes INTEGER, _0_version TEXT NOT NULL);
    CREATE TABLE "issueLabels" (issueID TEXT, labelID TEXT, legacyID TEXT, _0_version TEXT NOT NULL, PRIMARY KEY (issueID, labelID));
    CREATE TABLE "labels" (id TEXT PRIMARY KEY, name TEXT, _0_version TEXT NOT NULL);
  `);

  const insertIssue = db.prepare(
    `INSERT INTO issues (id, closed, _0_version) VALUES (?, ?, '01')`,
  );
  const insertComment = db.prepare(
    `INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES (?, ?, ?, '01')`,
  );
  const insertLabel = db.prepare(
    `INSERT INTO labels (id, name, _0_version) VALUES (?, ?, '01')`,
  );
  const insertIssueLabel = db.prepare(
    `INSERT INTO "issueLabels" (issueID, labelID, legacyID, _0_version) VALUES (?, ?, ?, '01')`,
  );

  db.exec('BEGIN');
  for (let i = 0; i < NUM_ROWS; i++) {
    insertIssue.run(`issue-${i}`, i % 2 === 0 ? 0 : 1);
    insertComment.run(`comment-${i}`, `issue-${i}`, i * 100);
    if (i < 20) {
      insertLabel.run(`label-${i}`, i % 3 === 0 ? 'bug' : 'feature');
      insertIssueLabel.run(`issue-${i}`, `label-${i}`, `${i}-${i}`);
    }
  }
  db.exec('COMMIT');
  populateFromExistingTables(db, listTables(db, false));

  const messages = new ReplicationMessages({
    issues: 'id',
    comments: 'id',
    issueLabels: ['issueID', 'labelID'],
    labels: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });
  const replicator = fakeReplicator(lc, db);
  return {dbFile, db, replicator, messages};
}

function generateQueries(n: number) {
  return Array.from({length: n}, (_, i) => ({
    hash: `h${i}`,
    id: `q${i}`,
    ast: QUERY_TEMPLATES[i % QUERY_TEMPLATES.length],
  }));
}

// ─── Sequential ────────────────────────────────────────────────────────────

function runSequential(): number {
  const {dbFile, replicator, messages} = createDB();
  try {
    const lc = createSilentLogContext();
    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();
    const pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('seq-cg'),
      'seq',
      new InspectorDelegate(undefined),
      () => 200,
    );
    pipelines.init(clientSchema);
    const timer = new TimeSliceTimer(lc).startWithoutYielding();
    for (const q of generateQueries(NUM_PIPELINES)) {
      [...pipelines.addQuery(q.hash, q.id, q.ast, timer)];
    }

    // Apply transaction
    const insertMsgs = [];
    for (let i = 0; i < NUM_INSERTS; i++) {
      insertMsgs.push(
        messages.insert('issues', {id: `new-${i}`, closed: i % 2}),
      );
      insertMsgs.push(
        messages.insert('comments', {
          id: `nc-${i}`,
          issueID: `new-${i}`,
          upvotes: BigInt(i * 10),
        }),
      );
    }
    replicator.processTransaction('02', ...insertMsgs);

    const start = performance.now();
    const result = pipelines.advance(NO_TIME_ADVANCEMENT_TIMER);
    const changes = [...result.changes];
    return performance.now() - start;
  } finally {
    dbFile.delete();
  }
}

// ─── Parallel ──────────────────────────────────────────────────────────────

async function runParallel(): Promise<{
  wall: number;
  maxWorker: number;
  totalChanges: number;
}> {
  const {dbFile, replicator, messages} = createDB();
  try {
    const queries = generateQueries(NUM_PIPELINES);
    const workerQueries: Array<typeof queries> = Array.from(
      {length: NUM_WORKERS},
      () => [],
    );
    for (let i = 0; i < queries.length; i++) {
      workerQueries[i % NUM_WORKERS].push(queries[i]);
    }

    const esmRequire = createRequire(import.meta.url);
    const workerPath = fileURLToPath(
      new URL('./parallel-push-worker.ts', import.meta.url),
    );
    const workers: Worker[] = [];
    const readyPromises: Promise<void>[] = [];

    for (let i = 0; i < NUM_WORKERS; i++) {
      const worker = new Worker(workerPath, {
        workerData: {dbPath: dbFile.path, queries: workerQueries[i], shardID},
        execArgv: [
          '--require',
          esmRequire.resolve('tsx/preflight'),
          '--import',
          'tsx/esm',
          '--no-warnings',
        ],
      });
      workers.push(worker);
      readyPromises.push(
        new Promise<void>(resolve => {
          worker.on('message', (msg: {type: string}) => {
            if (msg.type === 'ready') resolve();
          });
        }),
      );
    }

    await Promise.all(readyPromises);

    // Apply transaction
    const insertMsgs = [];
    for (let i = 0; i < NUM_INSERTS; i++) {
      insertMsgs.push(
        messages.insert('issues', {id: `new-${i}`, closed: i % 2}),
      );
      insertMsgs.push(
        messages.insert('comments', {
          id: `nc-${i}`,
          issueID: `new-${i}`,
          upvotes: BigInt(i * 10),
        }),
      );
    }
    replicator.processTransaction('02', ...insertMsgs);

    const start = performance.now();
    const advancePromises = workers.map(
      worker =>
        new Promise<{elapsed: number; numChanges: number}>(resolve => {
          worker.on(
            'message',
            (msg: {type: string; elapsed: number; numChanges: number}) => {
              if (msg.type === 'advance-done')
                resolve({elapsed: msg.elapsed, numChanges: msg.numChanges});
            },
          );
          worker.postMessage({type: 'advance'});
        }),
    );
    const results = await Promise.all(advancePromises);
    const wall = performance.now() - start;

    for (const worker of workers) {
      worker.postMessage({type: 'shutdown'});
    }

    return {
      wall,
      maxWorker: Math.max(...results.map(r => r.elapsed)),
      totalChanges: results.reduce((s, r) => s + r.numChanges, 0),
    };
  } finally {
    dbFile.delete();
  }
}

// ─── Main ──────────────────────────────────────────────────────────────────

async function main() {
  console.log(`\n=== Parallel Push POC ===`);
  console.log(
    `Pipelines: ${NUM_PIPELINES} | Rows: ${NUM_ROWS} | Inserts: ${NUM_INSERTS} | Workers: ${NUM_WORKERS}\n`,
  );

  // Warmup
  console.log('Warming up...');
  runSequential();

  // Sequential runs
  const seqTimes: number[] = [];
  for (let i = 0; i < 5; i++) {
    seqTimes.push(runSequential());
  }
  const seqMean = seqTimes.reduce((a, b) => a + b) / seqTimes.length;
  console.log(`Sequential: ${seqTimes.map(t => t.toFixed(1)).join(', ')} ms`);
  console.log(`  Mean: ${seqMean.toFixed(1)} ms\n`);

  // Parallel runs
  console.log('Running parallel...');
  const parTimes: number[] = [];
  for (let i = 0; i < 5; i++) {
    const {wall, maxWorker, totalChanges} = await runParallel();
    parTimes.push(wall);
    console.log(
      `  Run ${i + 1}: wall=${wall.toFixed(1)}ms, maxWorker=${maxWorker.toFixed(1)}ms, changes=${totalChanges}`,
    );
  }
  const parMean = parTimes.reduce((a, b) => a + b) / parTimes.length;
  console.log(`  Mean: ${parMean.toFixed(1)} ms\n`);

  console.log(`=== Result ===`);
  console.log(`Sequential mean: ${seqMean.toFixed(1)} ms`);
  console.log(`Parallel mean:   ${parMean.toFixed(1)} ms`);
  console.log(`Speedup:         ${(seqMean / parMean).toFixed(2)}x`);
}

main().catch(e => {
  console.error(e);
  process.exit(1);
});
