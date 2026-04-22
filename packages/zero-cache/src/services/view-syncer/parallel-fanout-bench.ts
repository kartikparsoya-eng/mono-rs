import {cpus} from 'os';
import path from 'path';
import {fileURLToPath} from 'url';
/**
 * Parallel Fan-out POC Benchmark
 *
 * Compares sequential (single PipelineDriver with 50 pipelines) vs
 * parallel (N workers, each with 50/N pipelines) for the advance/push hot path.
 *
 * Usage:
 *   node --experimental-strip-types --experimental-transform-types packages/zero-cache/src/services/view-syncer/parallel-fanout-bench.ts
 */
import {Worker} from 'worker_threads';
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
import type {Database as DB} from '../../../../zqlite/src/db.ts';
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

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// ─── Config ──────────────────────────────────────────────────────────────────
const NUM_PIPELINES = 50;
const NUM_ROWS = 500;
const NUM_INSERTS = 100;
const NUM_WORKERS = Math.min(cpus().length, 8);
const ITERATIONS = 5;

// ─── Schema ──────────────────────────────────────────────────────────────────
const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

const issues = table('issues')
  .columns({id: string(), closed: boolean()})
  .primaryKey('id');
const comments = table('comments')
  .columns({id: string(), issueID: string(), upvotes: number()})
  .primaryKey('id');

const clientSchema = createSchema({tables: [issues, comments]});

// ─── Queries (varied to simulate real workload) ──────────────────────────────
const QUERY_TEMPLATES: AST[] = [
  // JOIN
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
  // Filter
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
  // LIMIT
  {
    table: 'issues',
    orderBy: [['id', 'asc']],
    limit: 10,
  },
  // All comments ordered
  {
    table: 'comments',
    orderBy: [['id', 'asc']],
  },
];

function generateQueries(
  count: number,
): Array<{hydrationID: string; queryID: string; ast: AST}> {
  const queries = [];
  for (let i = 0; i < count; i++) {
    queries.push({
      hydrationID: `h${i}`,
      queryID: `q${i}`,
      ast: QUERY_TEMPLATES[i % QUERY_TEMPLATES.length],
    });
  }
  return queries;
}

// ─── DB Setup ────────────────────────────────────────────────────────────────
function setupDb(): {dbFile: DbFile; db: DB} {
  const lc = createSilentLogContext();
  const dbFile = new DbFile('parallel_fanout_bench');
  dbFile.connect(lc).pragma('journal_mode = wal2');
  const db = dbFile.connect(lc);

  initReplicationState(db, ['zero_data'], '01');
  db.exec(/*sql*/ `
    CREATE TABLE "${mutationsTableName}" (
      "clientGroupID" TEXT, "clientID" TEXT, "mutationID" INTEGER, "result" TEXT,
      _0_version TEXT NOT NULL,
      PRIMARY KEY ("clientGroupID", "clientID", "mutationID")
    );
    CREATE TABLE issues (id TEXT PRIMARY KEY, closed BOOL, _0_version TEXT NOT NULL);
    CREATE TABLE comments (id TEXT PRIMARY KEY, issueID TEXT, upvotes INTEGER, _0_version TEXT NOT NULL);
  `);

  const insertIssue = db.prepare(
    `INSERT INTO issues (id, closed, _0_version) VALUES (?, ?, '01')`,
  );
  const insertComment = db.prepare(
    `INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES (?, ?, ?, '01')`,
  );

  db.exec('BEGIN');
  for (let i = 0; i < NUM_ROWS; i++) {
    insertIssue.run(`issue-${i}`, i % 2 === 0 ? 0 : 1);
    insertComment.run(`comment-${i}`, `issue-${i}`, i * 100);
  }
  db.exec('COMMIT');

  populateFromExistingTables(db, listTables(db, false));
  return {dbFile, db};
}

// ─── Sequential Baseline ─────────────────────────────────────────────────────
function runSequential(dbFile: DbFile, db: DB): number {
  const lc = createSilentLogContext();
  const storage = new Database(lc, ':memory:');
  storage.prepare(CREATE_STORAGE_TABLE).run();

  const pipelines = new PipelineDriver(
    lc,
    testLogConfig,
    new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
    shardID,
    new DatabaseStorage(storage).createClientGroupStorage('seq'),
    'seq-bench',
    new InspectorDelegate(undefined),
    () => 200,
  );

  pipelines.init(clientSchema);
  const queries = generateQueries(NUM_PIPELINES);
  for (const q of queries) {
    [
      ...pipelines.addQuery(
        q.hydrationID,
        q.queryID,
        q.ast,
        NO_TIME_ADVANCEMENT_TIMER,
      ),
    ];
  }

  // Push changes
  const messages = new ReplicationMessages({
    issues: 'id',
    comments: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });
  const replicator = fakeReplicator(lc, db);
  const insertMsgs = [];
  for (let i = 0; i < NUM_INSERTS; i++) {
    insertMsgs.push(messages.insert('issues', {id: `new-${i}`, closed: i % 2}));
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
  const elapsed = performance.now() - start;

  console.log(`  Sequential: ${changes.length} row changes produced`);
  return elapsed;
}

// ─── Parallel (Worker Threads) ───────────────────────────────────────────────
async function runParallel(dbFile: DbFile, db: DB): Promise<number> {
  const allQueries = generateQueries(NUM_PIPELINES);
  const queriesPerWorker = Math.ceil(allQueries.length / NUM_WORKERS);

  // Spawn workers FIRST — they hydrate at version '01'
  const workerPath = path.join(__dirname, 'parallel-fanout-worker.ts');
  const workers: Worker[] = [];
  const readyPromises: Promise<void>[] = [];

  for (let i = 0; i < NUM_WORKERS; i++) {
    const workerQueries = allQueries.slice(
      i * queriesPerWorker,
      (i + 1) * queriesPerWorker,
    );
    if (workerQueries.length === 0) continue;

    const worker = new Worker(workerPath, {
      workerData: {
        dbPath: dbFile.path,
        shardID,
        queries: workerQueries,
        schema: clientSchema,
        workerIndex: i,
      },
      execArgv: ['--import', 'tsx'],
    });
    workers.push(worker);
    readyPromises.push(
      new Promise<void>((resolve, reject) => {
        worker.on('message', (msg: {type: string}) => {
          if (msg.type === 'ready') resolve();
        });
        worker.on('error', reject);
      }),
    );
  }

  // Wait for all workers to hydrate at version '01'
  await Promise.all(readyPromises);

  // NOW push changes to DB (version '02') — workers will see the diff on advance
  const lc = createSilentLogContext();
  const messages = new ReplicationMessages({
    issues: 'id',
    comments: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });
  const replicator = fakeReplicator(lc, db);
  const insertMsgs = [];
  for (let i = 0; i < NUM_INSERTS; i++) {
    insertMsgs.push(messages.insert('issues', {id: `new-${i}`, closed: i % 2}));
    insertMsgs.push(
      messages.insert('comments', {
        id: `nc-${i}`,
        issueID: `new-${i}`,
        upvotes: BigInt(i * 10),
      }),
    );
  }
  replicator.processTransaction('02', ...insertMsgs);

  // Now measure: tell all workers to advance simultaneously
  const start = performance.now();
  const donePromises = workers.map(
    w =>
      new Promise<number>((resolve, reject) => {
        w.on('message', (msg: {type: string; numChanges?: number}) => {
          if (msg.type === 'done') resolve(msg.numChanges ?? 0);
        });
        w.on('error', reject);
      }),
  );

  // Fire all at once
  for (const w of workers) {
    w.postMessage({type: 'advance'});
  }

  const results = await Promise.all(donePromises);
  const elapsed = performance.now() - start;

  const totalChanges = results.reduce((a, b) => a + b, 0);
  console.log(
    `  Parallel (${workers.length} workers): ${totalChanges} row changes produced`,
  );

  // Cleanup
  for (const w of workers) {
    await w.terminate();
  }

  return elapsed;
}

// ─── Main ────────────────────────────────────────────────────────────────────
async function main() {
  console.log(`\nParallel Fan-out POC Benchmark`);
  console.log(
    `  ${NUM_PIPELINES} pipelines, ${NUM_ROWS} seeded rows, ${NUM_INSERTS} inserts`,
  );
  console.log(
    `  ${NUM_WORKERS} worker threads (${cpus().length} CPUs available)`,
  );
  console.log(`  ${ITERATIONS} iterations\n`);

  const seqTimes: number[] = [];
  const parTimes: number[] = [];

  for (let i = 0; i < ITERATIONS; i++) {
    console.log(`Iteration ${i + 1}/${ITERATIONS}:`);

    // Sequential
    const {dbFile: dbSeq, db: seqDb} = setupDb();
    const seqTime = runSequential(dbSeq, seqDb);
    seqTimes.push(seqTime);
    console.log(`  Sequential: ${seqTime.toFixed(1)}ms`);
    dbSeq.delete();

    // Parallel
    const {dbFile: dbPar, db: parDb} = setupDb();
    const parTime = await runParallel(dbPar, parDb);
    parTimes.push(parTime);
    console.log(`  Parallel:   ${parTime.toFixed(1)}ms`);
    dbPar.delete();

    console.log(`  Speedup:    ${(seqTime / parTime).toFixed(2)}x\n`);
  }

  // Summary
  const avgSeq = seqTimes.reduce((a, b) => a + b) / seqTimes.length;
  const avgPar = parTimes.reduce((a, b) => a + b) / parTimes.length;
  console.log(`\n─── Summary ───`);
  console.log(`  Avg Sequential: ${avgSeq.toFixed(1)}ms`);
  console.log(`  Avg Parallel:   ${avgPar.toFixed(1)}ms`);
  console.log(`  Avg Speedup:    ${(avgSeq / avgPar).toFixed(2)}x`);
}

main().catch(e => {
  console.error(e);
  process.exit(1);
});
