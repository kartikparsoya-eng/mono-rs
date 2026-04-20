/**
 * Parallel Push POC Benchmark
 *
 * Compares:
 * - Sequential: 50 pipelines in 1 PipelineDriver on main thread
 * - Parallel: 50 pipelines spread across N worker threads (each with own PipelineDriver)
 *
 * Both read from the same SQLite DB (WAL mode allows concurrent readers).
 *
 * Usage: npx vitest bench packages/zero-cache/src/services/view-syncer/parallel-push.bench.ts
 */

import {cpus} from 'node:os';
import {fileURLToPath} from 'node:url';
import {Worker} from 'node:worker_threads';
import {bench, describe} from 'vitest';
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
import {
  fakeReplicator,
  ReplicationMessages,
  type FakeReplicator,
} from '../replicator/test-utils.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NUM_WORKERS = Math.min(cpus().length, 8);
const NUM_PIPELINES = 50;
const NUM_ROWS = 500;
const NUM_INSERTS = 100;

const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

const issues = table('issues')
  .columns({id: string(), closed: boolean()})
  .primaryKey('id');
const comments = table('comments')
  .columns({id: string(), issueID: string(), upvotes: number()})
  .primaryKey('id');
const issueLabels = table('issueLabels')
  .columns({issueID: string(), labelID: string(), legacyID: string()})
  .primaryKey('issueID', 'labelID');
const labels = table('labels')
  .columns({id: string(), name: string()})
  .primaryKey('id');

const clientSchema = createSchema({
  tables: [issues, comments, issueLabels, labels],
});

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// Mix of query types for realistic workload
const QUERY_TEMPLATES: AST[] = [
  // JOIN query
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
  // Filter query
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
  // Limit query
  {
    table: 'issues',
    orderBy: [['id', 'asc']],
    limit: 10,
  },
  // Comments filter
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

function createDB(): {dbFile: DbFile; db: DB; replicator: FakeReplicator; messages: ReplicationMessages<Record<string, string | string[]>>} {
  const lc = createSilentLogContext();
  const dbFile = new DbFile('parallel_push_bench');
  dbFile.connect(lc).pragma('journal_mode = wal2');

  const db = dbFile.connect(lc);
  initReplicationState(db, ['zero_data'], '01');
  db.exec(/*sql*/ `
    CREATE TABLE "${mutationsTableName}" (
      "clientGroupID"  TEXT,
      "clientID"       TEXT,
      "mutationID"     INTEGER,
      "result"         TEXT,
      _0_version       TEXT NOT NULL,
      PRIMARY KEY ("clientGroupID", "clientID", "mutationID")
    );
    CREATE TABLE issues (
      id TEXT PRIMARY KEY,
      closed BOOL,
      _0_version TEXT NOT NULL
    );
    CREATE TABLE comments (
      id TEXT PRIMARY KEY,
      issueID TEXT,
      upvotes INTEGER,
      _0_version TEXT NOT NULL
    );
    CREATE TABLE "issueLabels" (
      issueID TEXT,
      labelID TEXT,
      legacyID TEXT,
      _0_version TEXT NOT NULL,
      PRIMARY KEY (issueID, labelID)
    );
    CREATE TABLE "labels" (
      id TEXT PRIMARY KEY,
      name TEXT,
      _0_version TEXT NOT NULL
    );
  `);

  // Seed data
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

function generateQueries(n: number): Array<{hash: string; id: string; ast: AST}> {
  const queries = [];
  for (let i = 0; i < n; i++) {
    const ast = QUERY_TEMPLATES[i % QUERY_TEMPLATES.length];
    queries.push({hash: `h${i}`, id: `q${i}`, ast});
  }
  return queries;
}

function applyTransaction(
  replicator: FakeReplicator,
  messages: ReplicationMessages<Record<string, string | string[]>>,
  version: string,
) {
  const insertMsgs = [];
  for (let i = 0; i < NUM_INSERTS; i++) {
    insertMsgs.push(
      messages.insert('issues', {id: `new-${version}-${i}`, closed: i % 2}),
    );
    insertMsgs.push(
      messages.insert('comments', {
        id: `nc-${version}-${i}`,
        issueID: `new-${version}-${i}`,
        upvotes: BigInt(i * 10),
      }),
    );
  }
  replicator.processTransaction(version, ...insertMsgs);
}

// ─── Sequential benchmark ──────────────────────────────────────────────────

function runSequential(): number {
  const {dbFile, db, replicator, messages} = createDB();
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
      'sequential-bench',
      new InspectorDelegate(undefined),
      () => 200,
    );

    pipelines.init(clientSchema);
    const timer = new TimeSliceTimer(lc).startWithoutYielding();

    // Add N pipelines
    const queries = generateQueries(NUM_PIPELINES);
    for (const q of queries) {
      [...pipelines.addQuery(q.hash, q.id, q.ast, timer)];
    }

    // Apply transaction and advance
    applyTransaction(replicator, messages, '02');

    const start = performance.now();
    const result = pipelines.advance(NO_TIME_ADVANCEMENT_TIMER);
    const changes = [...result.changes];
    const elapsed = performance.now() - start;

    return elapsed;
  } finally {
    dbFile.delete();
  }
}

// ─── Parallel benchmark ────────────────────────────────────────────────────

async function runParallel(): Promise<number> {
  const {dbFile, db, replicator, messages} = createDB();
  try {
    const queries = generateQueries(NUM_PIPELINES);

    // Distribute queries across workers (round-robin)
    const workerQueries: Array<Array<{hash: string; id: string; ast: AST}>> =
      Array.from({length: NUM_WORKERS}, () => []);
    for (let i = 0; i < queries.length; i++) {
      workerQueries[i % NUM_WORKERS].push(queries[i]);
    }

    // Spawn workers
    const workerPath = fileURLToPath(
      new URL('./parallel-push-worker.ts', import.meta.url),
    );

    const workers: Worker[] = [];
    const readyPromises: Promise<void>[] = [];

    for (let i = 0; i < NUM_WORKERS; i++) {
      const worker = new Worker(workerPath, {
        workerData: {
          dbPath: dbFile.path,
          queries: workerQueries[i],
          shardID,
        },
        execArgv: ['--experimental-strip-types', '--no-warnings'],
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

    // Wait for all workers to be ready (pipelines hydrated)
    await Promise.all(readyPromises);

    // Apply transaction (writes to shared DB — workers will see it via WAL)
    applyTransaction(replicator, messages, '02');

    // Tell all workers to advance and measure total time
    const start = performance.now();

    const advancePromises = workers.map(
      worker =>
        new Promise<{elapsed: number; numChanges: number}>(resolve => {
          worker.on('message', (msg: {type: string; elapsed: number; numChanges: number}) => {
            if (msg.type === 'advance-done') {
              resolve({elapsed: msg.elapsed, numChanges: msg.numChanges});
            }
          });
          worker.postMessage({type: 'advance'});
        }),
    );

    const results = await Promise.all(advancePromises);
    const elapsed = performance.now() - start;

    // Cleanup
    for (const worker of workers) {
      worker.postMessage({type: 'shutdown'});
    }

    const totalChanges = results.reduce((s, r) => s + r.numChanges, 0);
    const maxWorkerTime = Math.max(...results.map(r => r.elapsed));

    console.log(
      `  Parallel: ${elapsed.toFixed(1)}ms wall, max worker: ${maxWorkerTime.toFixed(1)}ms, ` +
        `total changes: ${totalChanges}, workers: ${NUM_WORKERS}`,
    );

    return elapsed;
  } finally {
    dbFile.delete();
  }
}

// ─── Benchmark suite ─────────────────────────────────────────────────────────

describe(`Parallel Push POC (${NUM_PIPELINES} pipelines, ${NUM_INSERTS} inserts, ${NUM_WORKERS} workers)`, () => {
  bench(
    `sequential (${NUM_PIPELINES} pipelines, 1 thread)`,
    () => {
      const elapsed = runSequential();
      console.log(`  Sequential: ${elapsed.toFixed(1)}ms`);
    },
    {iterations: 5, warmupIterations: 1},
  );

  bench(
    `parallel (${NUM_PIPELINES} pipelines, ${NUM_WORKERS} workers)`,
    async () => {
      await runParallel();
    },
    {iterations: 5, warmupIterations: 1},
  );
});
