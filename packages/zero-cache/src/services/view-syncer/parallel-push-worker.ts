/**
 * Worker thread for parallel pipeline push POC.
 *
 * Each worker:
 * 1. Receives init data (dbPath, queries to hydrate)
 * 2. Sets up its own PipelineDriver + Snapshotter (read-only)
 * 3. Waits for "advance" messages
 * 4. Runs advance on its subset of pipelines
 * 5. Posts results back
 */
import {parentPort, workerData} from 'node:worker_threads';
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
import {InspectorDelegate} from '../../server/inspector-delegate.ts';
import type {ShardID} from '../../types/shards.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

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

interface WorkerInitData {
  dbPath: string;
  queries: Array<{hash: string; id: string; ast: AST}>;
  shardID: ShardID;
}

const {dbPath, queries, shardID} = workerData as WorkerInitData;

// Setup own PipelineDriver
const lc = createSilentLogContext();
const storage = new Database(lc, ':memory:');
storage.prepare(CREATE_STORAGE_TABLE).run();

const pipelines = new PipelineDriver(
  lc,
  testLogConfig,
  new Snapshotter(lc, dbPath, {appID: shardID.appID}),
  shardID,
  new DatabaseStorage(storage).createClientGroupStorage('worker-cg'),
  'parallel-worker',
  new InspectorDelegate(undefined),
  () => 200,
);

pipelines.init(clientSchema);

// Hydrate assigned queries
const timer = new TimeSliceTimer(lc).startWithoutYielding();
for (const q of queries) {
  [...pipelines.addQuery(q.hash, q.id, q.ast, timer)];
}

// Signal ready
parentPort!.postMessage({type: 'ready'});

// Listen for advance commands
parentPort!.on('message', (msg: {type: string}) => {
  if (msg.type === 'advance') {
    const start = performance.now();
    const result = pipelines.advance(NO_TIME_ADVANCEMENT_TIMER);
    const changes = [...result.changes];
    const elapsed = performance.now() - start;
    parentPort!.postMessage({
      type: 'advance-done',
      elapsed,
      numChanges: changes.length,
    });
  } else if (msg.type === 'shutdown') {
    process.exit(0);
  }
});
