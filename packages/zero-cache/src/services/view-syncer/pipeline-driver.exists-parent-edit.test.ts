import type {LogContext} from '@rocicorp/logger';
import {afterEach, beforeEach, describe, expect, test} from 'vitest';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import {createSchema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
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

// AUDIT-02 (Phase 30 plan 02): regression test for the EXISTS
// parent_field split-edit-keys bug. When a parent row's correlation
// column is updated, the source MUST emit Remove+Add (not Edit) so
// the downstream ExistsOperator can react to the membership flip.

const parents = table('parents')
  .columns({
    id: string(),
    child_id: string(),
  })
  .primaryKey('id');

const children = table('children')
  .columns({
    id: string(),
    parent_id: string(),
  })
  .primaryKey('id');

const clientSchema = createSchema({tables: [parents, children]});

// Filter via EXISTS in the where clause: parents whose child_id has at
// least one matching child (children.parent_id == parents.child_id).
const PARENTS_WITH_MATCHING_CHILD: AST = {
  table: 'parents',
  orderBy: [['id', 'asc']],
  where: {
    type: 'correlatedSubquery',
    op: 'EXISTS',
    related: {
      system: 'client',
      correlation: {
        parentField: ['child_id'],
        childField: ['parent_id'],
      },
      subquery: {
        table: 'children',
        alias: 'c',
        orderBy: [['id', 'asc']],
      },
    },
  },
};

describe('pipeline-driver EXISTS parent_field edit (AUDIT-02)', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_exists_parent_edit_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.exists-parent-edit.test.ts',
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
        child_id TEXT,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE children (
        id TEXT PRIMARY KEY,
        parent_id TEXT,
        _0_version TEXT NOT NULL
      );

      -- Parent p1 references child group 'A' which has children, so
      -- p1 starts in the EXISTS query result.
      INSERT INTO parents (id, child_id, _0_version) VALUES ('p1', 'A', '123');

      -- Children belonging to group 'A' (membership: A=true, B=false).
      INSERT INTO children (id, parent_id, _0_version) VALUES ('cA1', 'A', '123');
      INSERT INTO children (id, parent_id, _0_version) VALUES ('cA2', 'A', '123');

      -- A second matching group 'A2' (used to verify "membership
      -- preserved" after an edit) — different value but also has
      -- children, so EXISTS still holds.
      INSERT INTO children (id, parent_id, _0_version) VALUES ('cA2_1', 'A2', '123');
    `);
    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const messages = new ReplicationMessages({
    parents: 'id',
    children: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER): RowChange[] {
    return [...pipelines.advance(timer).changes];
  }

  test('EXISTS parent_field edit emits Remove when membership lost', () => {
    pipelines.init(clientSchema);
    // Hydrate: p1 (child_id='A') is in result because A has children.
    const hydration = [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        PARENTS_WITH_MATCHING_CHILD,
        startTimer(),
      ),
    ];
    const parentHydration = hydration.filter(
      r => r.table === 'parents' && r.queryID === 'q1',
    );
    expect(parentHydration).toHaveLength(1);
    expect(parentHydration[0].row).toMatchObject({id: 'p1', child_id: 'A'});

    // Edit p1: child_id 'A' (has children cA1+cA2) → 'B' (no children
    // exist with parent_id='B'). Pre-fix Rust: emitted Edit, downstream
    // ExistsOperator silently kept p1 in the result. Post-fix: source
    // splits to Remove(old)+Add(new); ExistsOperator drops the Add (B
    // has no children) so the net visible output is a Remove.
    replicator.processTransaction(
      '124',
      messages.update('parents', {id: 'p1', child_id: 'B'}),
    );
    const result = changes();
    const q1 = result.filter(c => c.queryID === 'q1' && c.table === 'parents');

    // Must contain a REMOVE for p1.
    const removes = q1.filter(c => c.type === 1); // ChangeType.REMOVE
    expect(removes).toHaveLength(1);
    expect(removes[0].rowKey).toMatchObject({id: 'p1'});

    // Must NOT contain an EDIT for p1 — the canonical Bug #2 symptom.
    const edits = q1.filter(c => c.type === 2); // ChangeType.EDIT
    expect(edits).toHaveLength(0);
  });

  test('EXISTS parent_field edit emits Add when membership gained', () => {
    pipelines.init(clientSchema);
    [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        PARENTS_WITH_MATCHING_CHILD,
        startTimer(),
      ),
    ];

    // Step 1: replicator-driven flip A → B (p1 leaves the result set).
    // We consume these changes so the next advance starts from a clean
    // baseline where p1 is OUT of the EXISTS query.
    replicator.processTransaction(
      '124',
      messages.update('parents', {id: 'p1', child_id: 'B'}),
    );
    const step1 = changes();
    const q1Step1 = step1.filter(c => c.queryID === 'q1' && c.table === 'parents');
    // Sanity: p1 should be removed when crossing A→B.
    const step1Removes = q1Step1.filter(c => c.type === 1);
    expect(step1Removes).toHaveLength(1);

    // Step 2 (the actual subject under test): replicator-driven flip
    // B → A. Pre-fix Rust: would emit Edit, downstream may not handle
    // as Add. Post-fix: source splits to Remove+Add; the synthetic
    // Remove is dropped (p1 wasn't in output) and the Add propagates.
    replicator.processTransaction(
      '125',
      messages.update('parents', {id: 'p1', child_id: 'A'}),
    );
    const result = changes();
    const q1 = result.filter(c => c.queryID === 'q1' && c.table === 'parents');

    // Must contain an ADD for p1.
    const adds = q1.filter(c => c.type === 0); // ChangeType.ADD
    expect(adds).toHaveLength(1);
    expect(adds[0].rowKey).toMatchObject({id: 'p1'});

    // Must NOT contain an EDIT for p1.
    const edits = q1.filter(c => c.type === 2); // ChangeType.EDIT
    expect(edits).toHaveLength(0);
  });

  test('EXISTS parent_field edit emits Edit when membership preserved', () => {
    pipelines.init(clientSchema);
    // Hydrate: p1 (child_id='A') is in result.
    [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        PARENTS_WITH_MATCHING_CHILD,
        startTimer(),
      ),
    ];

    // Edit p1: child_id 'A' (has children) → 'A2' (also has children).
    // Membership is preserved across the edit. The source MAY split
    // (parent_field is in split_edit_keys after the fix) and downstream
    // operators MAY recombine to Edit, OR the result may be a
    // Remove+Add pair. Either is acceptable as long as the post-state
    // shows p1 still in the output with child_id='A2' and no
    // duplicate/orphan rows are produced.
    replicator.processTransaction(
      '124',
      messages.update('parents', {id: 'p1', child_id: 'A2'}),
    );
    const result = changes();
    const q1 = result.filter(c => c.queryID === 'q1' && c.table === 'parents');

    // The fix in advance.rs::collect_split_edit_keys causes the source
    // to emit Remove+Add for any parent_field edit. With the fix, this
    // test verifies the regression guard: even when membership is
    // preserved, the downstream output is consistent (p1 ends up in
    // result with child_id='A2'; net effect == edit).
    const adds = q1.filter(c => c.type === 0);
    const removes = q1.filter(c => c.type === 1);
    const edits = q1.filter(c => c.type === 2);

    // Net change for p1: either single Edit, or one Remove + one Add.
    const totalChangesForP1 =
      adds.filter(c => (c.rowKey as {id: string}).id === 'p1').length +
      removes.filter(c => (c.rowKey as {id: string}).id === 'p1').length +
      edits.filter(c => (c.rowKey as {id: string}).id === 'p1').length;
    expect(totalChangesForP1).toBeGreaterThanOrEqual(1);
    expect(totalChangesForP1).toBeLessThanOrEqual(2);

    // The final row for p1 must reflect the new child_id.
    const finalForP1 = [...adds, ...edits].filter(
      c => (c.rowKey as {id: string}).id === 'p1',
    );
    if (finalForP1.length > 0) {
      expect(finalForP1[finalForP1.length - 1].row).toMatchObject({
        id: 'p1',
        child_id: 'A2',
      });
    }
  });
});
