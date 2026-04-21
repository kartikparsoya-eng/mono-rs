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
  nullClientSchema,
  NULL_FILTER_ROWS,
  NULL_ITEMS_QUERY,
  NULL_ITEMS_IS_NULL_QUERY,
  NULL_JOIN_QUERY,
  NULL_JOIN_PARENTS_DATA,
  NULL_JOIN_CHILDREN_DATA,
} from './pipeline-driver.fixtures.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('pipeline-driver NULL handling', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_null_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.null.test.ts',
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
      CREATE TABLE null_items (
        id TEXT PRIMARY KEY,
        val TEXT,
        "sortKey" TEXT,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE null_join_parents (
        id TEXT PRIMARY KEY,
        "joinKey" TEXT,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE null_join_children (
        id TEXT PRIMARY KEY,
        "parentKey" TEXT,
        label TEXT,
        _0_version TEXT NOT NULL
      );
    `);

    // Insert NULL_FILTER_ROWS
    for (const row of NULL_FILTER_ROWS) {
      const val = row.val === null ? 'null' : `'${row.val}'`;
      const sortKey = row.sortKey === null ? 'null' : `'${row.sortKey}'`;
      db.exec(
        `INSERT INTO null_items (id, val, "sortKey", _0_version) VALUES ('${row.id}', ${val}, ${sortKey}, '123');`,
      );
    }

    // Insert NULL_JOIN_PARENTS_DATA
    for (const row of NULL_JOIN_PARENTS_DATA) {
      const joinKey = row.joinKey === null ? 'null' : `'${row.joinKey}'`;
      db.exec(
        `INSERT INTO null_join_parents (id, "joinKey", _0_version) VALUES ('${row.id}', ${joinKey}, '123');`,
      );
    }

    // Insert NULL_JOIN_CHILDREN_DATA
    for (const row of NULL_JOIN_CHILDREN_DATA) {
      const parentKey = row.parentKey === null ? 'null' : `'${row.parentKey}'`;
      db.exec(
        `INSERT INTO null_join_children (id, "parentKey", label, _0_version) VALUES ('${row.id}', ${parentKey}, '${row.label}', '123');`,
      );
    }

    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const messages = new ReplicationMessages({
    null_items: 'id',
    null_join_parents: 'id',
    null_join_children: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER) {
    return [...pipelines.advance(timer).changes];
  }

  test('NULL values appear in unfiltered hydration', () => {
    pipelines.init(nullClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', NULL_ITEMS_QUERY, startTimer()),
    ];
    expect(hydration.length).toBe(5);
  });

  test('IS NULL filter returns only null-val rows', () => {
    pipelines.init(nullClientSchema);
    const hydration = [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        NULL_ITEMS_IS_NULL_QUERY,
        startTimer(),
      ),
    ];
    const ids = hydration.map(r => r.row?.id).sort();
    expect(ids).toEqual(['n2', 'n4']);
  });

  test('NULL sort keys sort consistently', () => {
    pipelines.init(nullClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', NULL_ITEMS_QUERY, startTimer()),
    ];
    const ids = hydration.map(r => r.row?.id);
    // Verify all rows present
    expect(ids.length).toBe(5);
    // Null sortKey rows (n2, n5) should be grouped together (either all first or all last)
    const nullIndices = ids
      .map((id, i) => ({id, i}))
      .filter(x => x.id === 'n2' || x.id === 'n5')
      .map(x => x.i);
    expect(Math.abs(nullIndices[0] - nullIndices[1])).toBeLessThanOrEqual(1);
  });

  test('NULL join keys do not produce spurious matches', () => {
    pipelines.init(nullClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', NULL_JOIN_QUERY, startTimer()),
    ];
    // Parent rows
    const parentRows = hydration.filter(r => r.table === 'null_join_parents');
    expect(parentRows.length).toBe(3);

    // Child rows — np2 (joinKey=null) should NOT match nc2 (parentKey=null)
    const childRows = hydration.filter(r => r.table === 'null_join_children');
    // np1 -> nc1, np3 -> nc3, np2 -> nothing
    // So we should have exactly 2 child rows
    expect(childRows.length).toBe(2);
    const childIds = childRows.map(r => r.row?.id).sort();
    expect(childIds).toEqual(['nc1', 'nc3']);
  });

  test('NULL handling Rust vs TS produces identical results', () => {
    pipelines.init(nullClientSchema);
    const rustHydration = [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        NULL_ITEMS_IS_NULL_QUERY,
        startTimer(),
      ),
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
        'pipeline-driver.null.test.ts-ts-path',
        new InspectorDelegate(undefined),
        () => 200,
      );

      pipelines2.init(nullClientSchema);
      const tsHydration = [
        ...pipelines2.addQuery(
          'hash1',
          'q1',
          NULL_ITEMS_IS_NULL_QUERY,
          startTimer(),
        ),
      ];

      const rustIds = rustHydration.map(r => r.row?.id).sort();
      const tsIds = tsHydration.map(r => r.row?.id).sort();
      expect(rustIds).toEqual(tsIds);
    } finally {
      if (origEnv === undefined) {
        delete process.env.ZERO_DISABLE_RUST_IVM;
      } else {
        process.env.ZERO_DISABLE_RUST_IVM = origEnv;
      }
    }
  });
});
