/**
 * Worker thread for parallel pipeline fan-out POC.
 * Each worker owns its own PipelineDriver + read-only DB connection.
 * Receives changes from main thread, pushes through its pipelines, returns results.
 */
import {parentPort, workerData} from 'worker_threads';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import type {Schema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
  CREATE_STORAGE_TABLE,
  DatabaseStorage,
} from '../../../../zqlite/src/database-storage.ts';
import {Database} from '../../../../zqlite/src/db.ts';
import {InspectorDelegate} from '../../server/inspector-delegate.ts';
import type {ShardID} from '../../types/shards.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// Minimal timer that satisfies the Timer interface for hydration
const HYDRATION_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

interface WorkerConfig {
  dbPath: string;
  shardID: ShardID;
  queries: Array<{hydrationID: string; queryID: string; ast: AST}>;
  schema: Schema;
  workerIndex: number;
}

const config = workerData as WorkerConfig;
const lc = createSilentLogContext();

// Each worker gets its own storage and pipeline driver
const storage = new Database(lc, ':memory:');
storage.prepare(CREATE_STORAGE_TABLE).run();

const pipelines = new PipelineDriver(
  lc,
  testLogConfig,
  new Snapshotter(lc, config.dbPath, {appID: config.shardID.appID}),
  config.shardID,
  new DatabaseStorage(storage).createClientGroupStorage(
    `worker-${config.workerIndex}`,
  ),
  `worker-${config.workerIndex}`,
  new InspectorDelegate(undefined),
  () => 200,
);

// Initialize and hydrate
pipelines.init(config.schema);
for (const q of config.queries) {
  [...pipelines.addQuery(q.hydrationID, q.queryID, q.ast, HYDRATION_TIMER)];
}

// Signal ready
parentPort!.postMessage({type: 'ready'});

// Handle advance requests
parentPort!.on('message', (msg: {type: string; version?: string}) => {
  if (msg.type === 'advance') {
    const result = pipelines.advance(NO_TIME_ADVANCEMENT_TIMER);
    const changes = [...result.changes];
    parentPort!.postMessage({type: 'done', numChanges: changes.length});
  }
});
