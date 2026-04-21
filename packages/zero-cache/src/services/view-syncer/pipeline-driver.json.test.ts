import type {LogContext} from '@rocicorp/logger';
import {afterEach, beforeEach, describe, expect, test} from 'vitest';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
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
import {
  jsonClientSchema,
  JSON_ROWS,
  JSON_ITEMS_QUERY,
  JSON_ITEMS_FILTER_QUERY,
} from './pipeline-driver.fixtures.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('pipeline-driver JSON data types', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_json_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.json.test.ts',
      new InspectorDelegate(undefined),
      () => 200,
    );

    db = dbFile.connect(lc);
    initReplicationState(db, ['zero_data'], '123');
    db.exec(/*sql*/ `
      CREATE TABLE "${mutationsTableName}" (
        "clientGroupID"  TEXT,
        "clientID"       TEXT,
        "mutationID"     INTEGER,
        "result"         TEXT,
        _0_version       TEXT NOT NULL,
        PRIMARY KEY ("clientGroupID", "clientID", "mutationID")
      );
      CREATE TABLE json_items (
        id TEXT PRIMARY KEY,
        payload TEXT,
        _0_version TEXT NOT NULL
      );

      INSERT INTO json_items (id, payload, _0_version) VALUES ('j1', '${JSON_ROWS.nestedObject.payload.replace(/'/g, "''")}', '123');
      INSERT INTO json_items (id, payload, _0_version) VALUES ('j2', '${JSON_ROWS.arrayTop.payload.replace(/'/g, "''")}', '123');
      INSERT INTO json_items (id, payload, _0_version) VALUES ('j3', '${JSON_ROWS.emptyObject.payload.replace(/'/g, "''")}', '123');
      INSERT INTO json_items (id, payload, _0_version) VALUES ('j4', '${JSON_ROWS.emptyArray.payload.replace(/'/g, "''")}', '123');
      INSERT INTO json_items (id, payload, _0_version) VALUES ('j5', '${JSON_ROWS.stringEscaped.payload.replace(/'/g, "''")}', '123');
    `);

    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const messages = new ReplicationMessages({
    json_items: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER) {
    return [...pipelines.advance(timer).changes];
  }

  test('JSON objects hydrate correctly', () => {
    pipelines.init(jsonClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', JSON_ITEMS_QUERY, startTimer()),
    ];
    expect(hydration.length).toBe(5);
    const j1 = hydration.find(r => r.row?.id === 'j1');
    expect(j1?.row?.payload).toBe(JSON_ROWS.nestedObject.payload);
  });

  test('JSON filter matches exact nested object', () => {
    pipelines.init(jsonClientSchema);
    const hydration = [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        JSON_ITEMS_FILTER_QUERY,
        startTimer(),
      ),
    ];
    expect(hydration.length).toBe(1);
    expect(hydration[0].row?.id).toBe('j1');
  });

  test('JSON values survive insert via replication', () => {
    pipelines.init(jsonClientSchema);
    [...pipelines.addQuery('hash1', 'q1', JSON_ITEMS_QUERY, startTimer())];

    const newPayload = JSON.stringify({new: true, nested: {deep: [1]}});
    replicator.processTransaction(
      '134',
      messages.insert('json_items', {id: 'j6', payload: newPayload}),
    );

    const result = changes();
    const added = result.find(c => c.row?.id === 'j6');
    expect(added).toBeDefined();
    expect(added?.row?.payload).toBe(newPayload);
  });

  test('JSON round-trip Rust vs TS produces identical results', () => {
    pipelines.init(jsonClientSchema);
    const rustHydration = [
      ...pipelines.addQuery('hash1', 'q1', JSON_ITEMS_QUERY, startTimer()),
    ];

    const origEnv = process.env.ZERO_DISABLE_RUST_IVM;
    try {
      process.env.ZERO_DISABLE_RUST_IVM = '1';

      const storage2 = new Database(lc, ':memory:');
      storage2.prepare(CREATE_STORAGE_TABLE).run();
      const pipelines2 = new PipelineDriver(
        lc,
        testLogConfig,
        new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
        shardID,
        new DatabaseStorage(storage2).createClientGroupStorage('bar-cg'),
        'pipeline-driver.json.test.ts-ts-path',
        new InspectorDelegate(undefined),
        () => 200,
      );

      pipelines2.init(jsonClientSchema);
      const tsHydration = [
        ...pipelines2.addQuery('hash1', 'q1', JSON_ITEMS_QUERY, startTimer()),
      ];

      const rustRows = rustHydration
        .map(r => r.row)
        .sort((a, b) => (a?.id ?? '').localeCompare(b?.id ?? ''));
      const tsRows = tsHydration
        .map(r => r.row)
        .sort((a, b) => (a?.id ?? '').localeCompare(b?.id ?? ''));

      expect(rustRows).toEqual(tsRows);
    } finally {
      if (origEnv === undefined) {
        delete process.env.ZERO_DISABLE_RUST_IVM;
      } else {
        process.env.ZERO_DISABLE_RUST_IVM = origEnv;
      }
    }
  });
});
