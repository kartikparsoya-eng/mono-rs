import type {LogContext} from '@rocicorp/logger';
import {afterEach, beforeEach, describe, expect, test} from 'vitest';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import {createSchema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
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
import {PipelineDriver, type RowChange, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// --- Schema: edit_items table with active column for filter boundary tests ---

const editItems = table('edit_items')
  .columns({
    id: string(),
    val: number(),
    active: string(),
  })
  .primaryKey('id');

const editClientSchema = createSchema({tables: [editItems]});

describe('pipeline-driver edit semantics (Phase 19)', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_edit_semantics_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.edit-semantics.test.ts',
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
      CREATE TABLE edit_items (
        id TEXT PRIMARY KEY,
        val INTEGER,
        active TEXT,
        _0_version TEXT NOT NULL
      );

      INSERT INTO edit_items (id, val, active, _0_version) VALUES ('e1', 10, 'yes', '123');
      INSERT INTO edit_items (id, val, active, _0_version) VALUES ('e2', 20, 'no', '123');
      INSERT INTO edit_items (id, val, active, _0_version) VALUES ('e3', 30, 'yes', '123');
    `);
    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const messages = new ReplicationMessages({
    edit_items: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER): RowChange[] {
    return [...pipelines.advance(timer).changes];
  }

  // Filter-only ASTs (Rust-eligible)
  const ALL_ITEMS: AST = {table: 'edit_items', orderBy: [['id', 'asc']]};
  const ACTIVE_YES: AST = {
    table: 'edit_items',
    orderBy: [['id', 'asc']],
    where: {
      type: 'simple',
      op: '=',
      left: {type: 'column', name: 'active'},
      right: {type: 'literal', value: 'yes'},
    },
  };

  // --- EDI-01: Edit split / filter boundary tests (D-90, D-91) ---

  test('edit crossing filter boundary (visible→hidden) emits remove', () => {
    pipelines.init(editClientSchema);
    // e1 starts with active='yes' (pre-populated), so it's in the ACTIVE_YES query
    [...pipelines.addQuery('h1', 'q1', ACTIVE_YES, startTimer())];

    // Edit e1: active 'yes' → 'no' (crosses filter boundary)
    replicator.processTransaction(
      '124',
      messages.update('edit_items', {id: 'e1', val: 10, active: 'no'}),
    );
    const result = changes();
    const q1 = result.filter(c => c.queryID === 'q1');
    // D-90: strict type match — must be REMOVE (1), not EDIT (2)
    expect(q1.length).toBe(1);
    expect(q1[0].type).toBe(1); // ChangeType.REMOVE
  });

  test('edit making row visible (hidden→visible) emits add or edit', () => {
    pipelines.init(editClientSchema);
    // e2 starts with active='no' (pre-populated), so it's NOT in the ACTIVE_YES query
    [...pipelines.addQuery('h1', 'q1', ACTIVE_YES, startTimer())];

    // Edit e2: active 'no' → 'yes' (becomes visible)
    replicator.processTransaction(
      '124',
      messages.update('edit_items', {id: 'e2', val: 20, active: 'yes'}),
    );
    const result = changes();
    const q1 = result.filter(c => c.queryID === 'q1');
    // Row becomes visible — Rust advance emits "edit" for PK-matched edits
    // where new passes filter. The downstream TS operator may split to "add".
    expect(q1.length).toBe(1);
    // Accept either ADD (0) or EDIT (2) — depends on whether TS operator splits
    expect([0, 2]).toContain(q1[0].type);
  });

  // --- EDI-02: Edit type transitions across pushes ---

  test('insert then update: new row insert produces add, then update produces edit', () => {
    pipelines.init(editClientSchema);
    [...pipelines.addQuery('h1', 'q1', ALL_ITEMS, startTimer())];

    // Push 1: Insert a NEW row (not pre-populated)
    replicator.processTransaction(
      '124',
      messages.insert('edit_items', {id: 'e10', val: 1, active: 'yes'}),
    );
    const push1 = changes();
    const p1 = push1.filter(c => c.queryID === 'q1');
    expect(p1.length).toBe(1);
    expect(p1[0].type).toBe(0); // ADD

    // Push 2: Update the same row
    replicator.processTransaction(
      '125',
      messages.update('edit_items', {id: 'e10', val: 2, active: 'yes'}),
    );
    const push2 = changes();
    const p2 = push2.filter(c => c.queryID === 'q1');
    expect(p2.length).toBe(1);
    expect(p2[0].type).toBe(2); // EDIT
  });

  test('update then delete produces remove', () => {
    pipelines.init(editClientSchema);
    // e1 is pre-populated
    [...pipelines.addQuery('h1', 'q1', ALL_ITEMS, startTimer())];

    // Push 1: Update existing row e1
    replicator.processTransaction(
      '124',
      messages.update('edit_items', {id: 'e1', val: 2, active: 'yes'}),
    );
    const editResult = changes();
    const edits = editResult.filter(c => c.queryID === 'q1');
    expect(edits.length).toBe(1);
    expect(edits[0].type).toBe(2); // EDIT

    // Push 2: Delete e1
    replicator.processTransaction(
      '125',
      messages.delete('edit_items', {id: 'e1'}),
    );
    const deleteResult = changes();
    const deletes = deleteResult.filter(c => c.queryID === 'q1');
    expect(deletes.length).toBe(1);
    expect(deletes[0].type).toBe(1); // REMOVE
  });
});
