import type {LogContext} from '@rocicorp/logger';
import {afterEach, beforeEach, describe, expect, test} from 'vitest';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import {createSchema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
  boolean,
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

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('pipeline-driver NOT EXISTS', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_not_exists_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.not-exists.test.ts',
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
      CREATE TABLE parents (
        id TEXT PRIMARY KEY,
        name TEXT,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE children (
        id TEXT PRIMARY KEY,
        "parentID" TEXT,
        active BOOL,
        _0_version TEXT NOT NULL
      );

      INSERT INTO parents (id, name, _0_version) VALUES ('p1', 'Alice', '123');
      INSERT INTO parents (id, name, _0_version) VALUES ('p2', 'Bob', '123');
      INSERT INTO parents (id, name, _0_version) VALUES ('p3', 'Carol', '123');
      INSERT INTO children (id, "parentID", active, _0_version) VALUES ('c1', 'p1', 1, '123');
      INSERT INTO children (id, "parentID", active, _0_version) VALUES ('c2', 'p2', 0, '123');
    `);

    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const parents = table('parents')
    .columns({id: string(), name: string()})
    .primaryKey('id');
  const children = table('children')
    .columns({id: string(), parentID: string(), active: boolean()})
    .primaryKey('id');

  const clientSchema = createSchema({tables: [parents, children]});

  const PARENTS_WITHOUT_CHILDREN: AST = {
    table: 'parents',
    orderBy: [['id', 'asc']],
    where: {
      type: 'correlatedSubquery',
      op: 'NOT EXISTS',
      related: {
        system: 'client',
        correlation: {
          parentField: ['id'],
          childField: ['parentID'],
        },
        subquery: {
          table: 'children',
          alias: 'children',
          orderBy: [['id', 'asc']],
        },
      },
    },
  };

  const PARENTS_WITH_CHILDREN: AST = {
    table: 'parents',
    orderBy: [['id', 'asc']],
    where: {
      type: 'correlatedSubquery',
      op: 'EXISTS',
      related: {
        system: 'client',
        correlation: {
          parentField: ['id'],
          childField: ['parentID'],
        },
        subquery: {
          table: 'children',
          alias: 'children',
          orderBy: [['id', 'asc']],
        },
      },
    },
  };

  const messages = new ReplicationMessages({
    parents: 'id',
    children: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER) {
    return [...pipelines.advance(timer).changes];
  }

  test('NOT EXISTS hydration returns only parents without children', () => {
    pipelines.init(clientSchema);
    const hydration = [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        PARENTS_WITHOUT_CHILDREN,
        startTimer(),
      ),
    ];

    // Only p3 has no children
    const parentRows = hydration.filter(r => r.table === 'parents');
    expect(parentRows).toHaveLength(1);
    expect(parentRows[0].row).toMatchObject({id: 'p3', name: 'Carol'});
  });

  test('NOT EXISTS reacts to child addition', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', PARENTS_WITHOUT_CHILDREN, startTimer())];

    // Add a child for p3 — p3 should be removed from NOT EXISTS results
    replicator.processTransaction(
      '134',
      messages.insert('children', {id: 'c3', parentID: 'p3', active: true}),
    );

    const result = changes();
    const parentChanges = result.filter(
      c => c.table === 'parents' && c.queryID === 'q1',
    );
    // p3 should be removed (type 1 = remove)
    expect(
      parentChanges.some(
        c => (c.row?.id === 'p3' || c.rowKey?.id === 'p3') && c.type === 1,
      ),
    ).toBe(true);
  });

  test('NOT EXISTS reacts to child removal', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', PARENTS_WITHOUT_CHILDREN, startTimer())];

    // Remove p1's only child — p1 should appear in NOT EXISTS results
    replicator.processTransaction('134', messages.delete('children', {id: 'c1'}));

    const result = changes();
    const parentChanges = result.filter(
      c => c.table === 'parents' && c.queryID === 'q1',
    );
    // p1 should be added (type 0 = add)
    expect(parentChanges.some(c => c.row.id === 'p1' && c.type === 0)).toBe(
      true,
    );
  });

  test('EXISTS vs NOT EXISTS produce complementary results', () => {
    pipelines.init(clientSchema);
    const existsHydration = [
      ...pipelines.addQuery('hash-e', 'qExists', PARENTS_WITH_CHILDREN, startTimer()),
    ];
    const notExistsHydration = [
      ...pipelines.addQuery('hash-ne', 'qNotExists', PARENTS_WITHOUT_CHILDREN, startTimer()),
    ];

    const existsParents = existsHydration
      .filter(r => r.table === 'parents')
      .map(r => r.row.id)
      .sort();
    const notExistsParents = notExistsHydration
      .filter(r => r.table === 'parents')
      .map(r => r.row.id)
      .sort();

    // Union should be all parents, no overlap
    const allParents = [...existsParents, ...notExistsParents].sort();
    expect(allParents).toEqual(['p1', 'p2', 'p3']);
    // No overlap
    const overlap = existsParents.filter(id => notExistsParents.includes(id));
    expect(overlap).toHaveLength(0);
  });
});
