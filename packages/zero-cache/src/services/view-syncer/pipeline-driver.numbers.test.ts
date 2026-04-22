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
import {
  numberClientSchema,
  NUMBER_ROWS,
  NUMBER_ITEMS_QUERY,
  NUMBER_FILTER_QUERY,
  NUMBER_GT_QUERY,
} from './pipeline-driver.fixtures.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('pipeline-driver boundary numbers', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_numbers_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.numbers.test.ts',
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
      CREATE TABLE number_items (
        id TEXT PRIMARY KEY,
        val REAL,
        label TEXT,
        _0_version TEXT NOT NULL
      );
    `);

    // Insert NUMBER_ROWS using parameterized queries
    const stmt = db.prepare(
      'INSERT INTO number_items (id, val, label, _0_version) VALUES (?, ?, ?, ?)',
    );
    for (const row of NUMBER_ROWS) {
      // -0 is stored as 0 in SQLite
      stmt.run(row.id, row.val, row.label, '123');
    }

    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const messages = new ReplicationMessages({
    number_items: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER) {
    return [...pipelines.advance(timer).changes];
  }

  test('boundary numbers hydrate correctly', () => {
    pipelines.init(numberClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', NUMBER_ITEMS_QUERY, startTimer()),
    ];
    expect(hydration.length).toBe(7);
    const num1 = hydration.find(r => r.row?.id === 'num1');
    expect(num1?.row?.val).toBe(Number.MAX_SAFE_INTEGER);
  });

  test('MAX_SAFE_INTEGER filter equality works', () => {
    pipelines.init(numberClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', NUMBER_FILTER_QUERY, startTimer()),
    ];
    expect(hydration.length).toBe(1);
    expect(hydration[0].row?.id).toBe('num1');
  });

  test('greater-than filter with boundary numbers', () => {
    pipelines.init(numberClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', NUMBER_GT_QUERY, startTimer()),
    ];
    const ids = hydration.map(r => r.row?.id).sort();
    // val > 0: num1 (MAX_SAFE_INTEGER), num3 (0.3), num5 (1e308), num6 (5e-324), num7 (42)
    // NOT: num2 (negative), num4 (-0 stored as 0, 0 > 0 is false)
    expect(ids).toContain('num1');
    expect(ids).toContain('num7');
    expect(ids).not.toContain('num2');
  });

  test('number insert via replication preserves precision', () => {
    pipelines.init(numberClientSchema);
    [...pipelines.addQuery('hash1', 'q1', NUMBER_ITEMS_QUERY, startTimer())];

    const preciseVal = Number.MAX_SAFE_INTEGER - 1;
    replicator.processTransaction(
      '134',
      messages.insert('number_items', {
        id: 'num8',
        val: preciseVal,
        label: 'precise',
      }),
    );

    const result = changes();
    const added = result.find(c => c.row?.id === 'num8');
    expect(added).toBeDefined();
    expect(added?.row?.val).toBe(preciseVal);
  });

  test('boundary numbers Rust vs TS produces identical results', () => {
    pipelines.init(numberClientSchema);
    const rustHydration = [
      ...pipelines.addQuery('hash1', 'q1', NUMBER_ITEMS_QUERY, startTimer()),
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
        'pipeline-driver.numbers.test.ts-ts-path',
        new InspectorDelegate(undefined),
        () => 200,
      );

      pipelines2.init(numberClientSchema);
      const tsHydration = [
        ...pipelines2.addQuery('hash1', 'q1', NUMBER_ITEMS_QUERY, startTimer()),
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
