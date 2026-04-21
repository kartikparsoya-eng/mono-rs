import type {LogContext} from '@rocicorp/logger';
import {afterEach, beforeEach, describe, expect, test} from 'vitest';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
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
import {numberClientSchema} from './pipeline-driver.fixtures.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// --- Filter-only AST queries (all Rust-eligible) ---

const ALL_ITEMS: AST = {table: 'number_items', orderBy: [['id', 'asc']]};
const VAL_GT_20: AST = {
  table: 'number_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '>',
    left: {type: 'column', name: 'val'},
    right: {type: 'literal', value: 20},
  },
};
const VAL_LT_40: AST = {
  table: 'number_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '<',
    left: {type: 'column', name: 'val'},
    right: {type: 'literal', value: 40},
  },
};
const LABEL_EQ_A: AST = {
  table: 'number_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '=',
    left: {type: 'column', name: 'label'},
    right: {type: 'literal', value: 'a'},
  },
};
const LABEL_EQ_B: AST = {
  table: 'number_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '=',
    left: {type: 'column', name: 'label'},
    right: {type: 'literal', value: 'b'},
  },
};
const VAL_GT_0: AST = {
  table: 'number_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '>',
    left: {type: 'column', name: 'val'},
    right: {type: 'literal', value: 0},
  },
};

describe('pipeline-driver concurrency — pipeline mutation between advance calls', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_concurrency_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.concurrency.test.ts',
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

      INSERT INTO number_items (id, val, label, _0_version) VALUES ('n1', 10, 'a', '123');
      INSERT INTO number_items (id, val, label, _0_version) VALUES ('n2', 20, 'b', '123');
      INSERT INTO number_items (id, val, label, _0_version) VALUES ('n3', 30, 'c', '123');
      INSERT INTO number_items (id, val, label, _0_version) VALUES ('n4', 40, 'd', '123');
      INSERT INTO number_items (id, val, label, _0_version) VALUES ('n5', 50, 'e', '123');
    `);

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

  test('add pipeline between advance calls gets correct results', () => {
    pipelines.init(numberClientSchema);
    [...pipelines.addQuery('h1', 'q1', ALL_ITEMS, startTimer())];

    replicator.processTransaction(
      '134',
      messages.insert('number_items', {id: 'n6', val: 60, label: 'f'}),
    );
    changes();
    expect(pipelines.queries().has('q1')).toBe(true);

    // Add a second pipeline between advance calls
    const hydration = [
      ...pipelines.addQuery('h2', 'q2', VAL_GT_20, startTimer()),
    ];
    expect(hydration.length).toBeGreaterThan(0);

    // Both pipelines should be active
    expect(pipelines.queries().has('q1')).toBe(true);
    expect(pipelines.queries().has('q2')).toBe(true);

    replicator.processTransaction(
      '135',
      messages.insert('number_items', {id: 'n7', val: 70, label: 'g'}),
    );
    const r2 = changes();
    // After advance, no removed-query results should appear — both are still active
    expect(r2.every(c => c.queryID === 'q1' || c.queryID === 'q2')).toBe(true);
  });

  test('remove pipeline between advance calls — no stale output', () => {
    pipelines.init(numberClientSchema);
    [...pipelines.addQuery('h1', 'q1', ALL_ITEMS, startTimer())];
    [...pipelines.addQuery('h2', 'q2', VAL_GT_20, startTimer())];

    replicator.processTransaction(
      '134',
      messages.insert('number_items', {id: 'n6', val: 60, label: 'f'}),
    );
    changes();

    // Both pipelines active before removal
    expect(pipelines.queries().has('q1')).toBe(true);
    expect(pipelines.queries().has('q2')).toBe(true);

    pipelines.removeQuery('q1');
    expect(pipelines.queries().has('q1')).toBe(false);

    replicator.processTransaction(
      '135',
      messages.insert('number_items', {id: 'n7', val: 70, label: 'g'}),
    );
    const r2 = changes();
    expect(r2.every(c => c.queryID !== 'q1')).toBe(true);
  });

  test('add and remove in same interval', () => {
    pipelines.init(numberClientSchema);
    [...pipelines.addQuery('h1', 'q1', ALL_ITEMS, startTimer())];
    [...pipelines.addQuery('h2', 'q2', VAL_GT_20, startTimer())];
    [...pipelines.addQuery('h3', 'q3', VAL_LT_40, startTimer())];

    replicator.processTransaction(
      '134',
      messages.insert('number_items', {id: 'n6', val: 60, label: 'f'}),
    );
    changes();

    // Remove q1 and add q4 in the same interval
    pipelines.removeQuery('q1');
    [...pipelines.addQuery('h4', 'q4', LABEL_EQ_A, startTimer())];

    replicator.processTransaction(
      '135',
      messages.insert('number_items', {id: 'n8', val: 80, label: 'h'}),
    );
    const r2 = changes();
    expect(r2.every(c => c.queryID !== 'q1')).toBe(true);
  });

  test('remove all pipelines — empty results, no errors', () => {
    pipelines.init(numberClientSchema);
    [...pipelines.addQuery('h1', 'q1', ALL_ITEMS, startTimer())];

    replicator.processTransaction(
      '134',
      messages.insert('number_items', {id: 'n6', val: 60, label: 'f'}),
    );
    changes();

    expect(pipelines.queries().has('q1')).toBe(true);
    pipelines.removeQuery('q1');
    expect(pipelines.queries().size).toBe(0);

    replicator.processTransaction(
      '135',
      messages.insert('number_items', {id: 'n7', val: 70, label: 'g'}),
    );
    const r2 = changes();
    expect(r2).toHaveLength(0);
  });

  test('rapid pipeline churn — remove 5 add 5', () => {
    pipelines.init(numberClientSchema);

    const queries: AST[] = [
      ALL_ITEMS,
      VAL_GT_20,
      VAL_LT_40,
      LABEL_EQ_A,
      LABEL_EQ_B,
      VAL_GT_0,
      ALL_ITEMS,
      VAL_GT_20,
      VAL_LT_40,
      LABEL_EQ_A,
    ];
    for (let i = 0; i < 10; i++) {
      [...pipelines.addQuery(`h${i}`, `q${i}`, queries[i], startTimer())];
    }

    replicator.processTransaction(
      '134',
      messages.insert('number_items', {id: 'n6', val: 60, label: 'f'}),
    );
    changes();

    // Remove q0-q4, add q10-q14
    for (let i = 0; i < 5; i++) {
      pipelines.removeQuery(`q${i}`);
    }
    const newQueries: AST[] = [
      LABEL_EQ_B,
      VAL_GT_0,
      ALL_ITEMS,
      VAL_GT_20,
      VAL_LT_40,
    ];
    for (let i = 0; i < 5; i++) {
      [
        ...pipelines.addQuery(
          `h${10 + i}`,
          `q${10 + i}`,
          newQueries[i],
          startTimer(),
        ),
      ];
    }

    replicator.processTransaction(
      '135',
      messages.insert('number_items', {id: 'n7', val: 70, label: 'g'}),
    );
    const r2 = changes();
    const removedIds = new Set(['q0', 'q1', 'q2', 'q3', 'q4']);
    expect(r2.every(c => !removedIds.has(c.queryID))).toBe(true);
  });
});
