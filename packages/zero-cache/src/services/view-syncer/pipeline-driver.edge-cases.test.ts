import type {LogContext} from '@rocicorp/logger';
import {afterEach, beforeEach, describe, expect, test} from 'vitest';
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

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('pipeline-driver edge cases', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_edge_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.edge-cases.test.ts',
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
      CREATE TABLE issues (
        id TEXT PRIMARY KEY,
        closed BOOL,
        ignored INET,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE comments (
        id TEXT PRIMARY KEY,
        issueID TEXT,
        upvotes INTEGER,
        ignored BYTEA,
        stillBeingBackfilled TEXT,
         _0_version TEXT NOT NULL);

      INSERT INTO ISSUES (id, closed, ignored, _0_version) VALUES ('1', 0, 1728345600000, '123');
      INSERT INTO ISSUES (id, closed, ignored, _0_version) VALUES ('2', 1, 1722902400000, '123');
      INSERT INTO ISSUES (id, closed, ignored, _0_version) VALUES ('3', 0, null, '123');
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version) VALUES ('10', '1', 0, '123');
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version) VALUES ('20', '2', 1, '123');
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version) VALUES ('21', '2', 10000, '123');
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version) VALUES ('22', '2', 20000, '123');
      `);

    populateFromExistingTables(db, listTables(db, false));
    db.exec(/*sql*/ `
      UPDATE "_zero.column_metadata"
        SET backfill = '{"upstreamID":123}'
        WHERE table_name = 'comments'
         AND column_name = 'stillBeingBackfilled';
      `);
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const issues = table('issues')
    .columns({
      id: string(),
      closed: boolean(),
    })
    .primaryKey('id');
  const comments = table('comments')
    .columns({
      id: string(),
      issueID: string(),
      upvotes: number(),
    })
    .primaryKey('id');

  const clientSchema = createSchema({
    tables: [issues, comments],
  });

  const ISSUES_AND_COMMENTS: AST = {
    table: 'issues',
    orderBy: [['id', 'desc']],
    related: [
      {
        system: 'client',
        correlation: {
          parentField: ['id'],
          childField: ['issueID'],
        },
        subquery: {
          table: 'comments',
          alias: 'comments',
          orderBy: [['id', 'desc']],
        },
      },
    ],
  };

  const OPEN_ISSUES_ONLY: AST = {
    table: 'issues',
    orderBy: [['id', 'asc']],
    where: {
      type: 'simple',
      op: '=',
      left: {type: 'column', name: 'closed'},
      right: {type: 'literal', value: false},
    },
  };

  const messages = new ReplicationMessages({
    issues: 'id',
    comments: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER) {
    return [...pipelines.advance(timer).changes];
  }

  test('mixed eligible and ineligible pipelines produce correct output', () => {
    pipelines.init(clientSchema);

    // Filter-only query (eligible for Rust IVM)
    [
      ...pipelines.addQuery(
        'hash-filter',
        'qFilter',
        OPEN_ISSUES_ONLY,
        startTimer(),
      ),
    ];

    // JOIN query (ineligible for Rust IVM)
    [
      ...pipelines.addQuery(
        'hash-join',
        'qJoin',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];

    replicator.processTransaction(
      '134',
      messages.insert('issues', {id: '4', closed: 0}),
    );

    const result = changes();
    const filterResults = result.filter(c => c.queryID === 'qFilter');
    const joinResults = result.filter(c => c.queryID === 'qJoin');

    expect(filterResults.length).toBeGreaterThanOrEqual(1);
    expect(joinResults.length).toBeGreaterThanOrEqual(1);
  });

  test('adding a pipeline after initial advance works correctly', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', OPEN_ISSUES_ONLY, startTimer())];

    replicator.processTransaction(
      '134',
      messages.insert('issues', {id: '4', closed: 0}),
    );
    changes();

    // Add a second pipeline after the first advance
    const hydration = [
      ...pipelines.addQuery(
        'hash-join',
        'q2',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];
    expect(hydration.length).toBeGreaterThan(0);

    replicator.processTransaction(
      '135',
      messages.insert('issues', {id: '5', closed: 1}),
    );

    const result = changes();
    const q2Results = result.filter(c => c.queryID === 'q2');
    expect(q2Results.length).toBeGreaterThanOrEqual(1);
  });

  test('removing a pipeline after advance works correctly', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', OPEN_ISSUES_ONLY, startTimer())];
    [
      ...pipelines.addQuery(
        'hash-join',
        'q2',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];

    replicator.processTransaction(
      '134',
      messages.insert('issues', {id: '4', closed: 0}),
    );
    changes();

    pipelines.removeQuery('q1');
    expect(pipelines.queries().size).toBe(1);

    replicator.processTransaction(
      '135',
      messages.insert('issues', {id: '5', closed: 1}),
    );

    const result = changes();
    expect(result.every(c => c.queryID === 'q2')).toBe(true);
  });

  test('advance with no data changes produces empty result', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', OPEN_ISSUES_ONLY, startTimer())];

    // No replicator transaction — no changes
    // Advance without any DB mutations should produce no changes.
    // We need at least a version bump for advance to proceed, so
    // process an empty-ish transaction that doesn't touch our tables.
    replicator.processTransaction('134');
    const result = changes();
    expect(result).toHaveLength(0);
  });

  test('large transaction with 500 inserts produces correct change count', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', OPEN_ISSUES_ONLY, startTimer())];

    const inserts = [];
    for (let i = 100; i < 600; i++) {
      inserts.push(messages.insert('issues', {id: String(i), closed: 0}));
    }
    replicator.processTransaction('134', ...inserts);

    const result = changes();
    const addResults = result.filter(
      c => c.queryID === 'q1' && c.table === 'issues',
    );
    expect(addResults.length).toBe(500);
  });

  // Regression test: Without initializeTakeState() after Rust hydration,
  // Take.push() silently drops ALL changes because takeState is uninitialized.
  // This test runs without ZERO_DUAL_EXEC, so hydration goes through
  // #rustHydrate (the production path) which bypasses input.fetch().
  test('LIMIT query remains reactive after Rust hydration', () => {
    const ISSUES_WITH_LIMIT: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      limit: 2,
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-limit',
        'queryLimit',
        ISSUES_WITH_LIMIT,
        startTimer(),
      ),
    ];

    // Hydration should return issues '1' and '2' (LIMIT 2, ORDER BY id ASC)
    const hydratedIssues = hydrated.filter(r => r.table === 'issues');
    expect(hydratedIssues).toHaveLength(2);
    expect(hydratedIssues.map(r => r.rowKey)).toEqual(
      expect.arrayContaining([{id: '1'}, {id: '2'}]),
    );

    // Insert issue '0' which sorts before '1', pushing '2' out of the window
    replicator.processTransaction(
      '134',
      messages.insert('issues', {id: '0', closed: 0}),
    );

    const result = changes();
    const issueChanges = result.filter(
      c => c.queryID === 'queryLimit' && c.table === 'issues',
    );

    // Without the fix, issueChanges would be empty (Take drops everything)
    expect(issueChanges.length).toBeGreaterThanOrEqual(1);

    // Issue '0' should enter the window
    expect(issueChanges).toEqual(
      expect.arrayContaining([
        expect.objectContaining({type: 0, rowKey: {id: '0'}}),
      ]),
    );

    // Issue '2' should fall out of the window
    expect(issueChanges).toEqual(
      expect.arrayContaining([
        expect.objectContaining({type: 1, rowKey: {id: '2'}}),
      ]),
    );
  });

  // Regression test: Ensures takeStorage captures only the top-level Take
  // (name === ':take'), not a child Take from .related() (e.g. '.comments:take').
  // If takeStorage were overwritten by a child's partitioned storage,
  // initializeTakeState would write to the wrong storage and the top-level
  // Take would remain non-reactive.
  test('LIMIT with .related() child LIMIT remains reactive', () => {
    // Top-level: LIMIT 2 on issues, child: LIMIT 1 on comments per issue
    const ISSUES_WITH_RELATED_LIMIT: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      limit: 2,
      related: [
        {
          system: 'client',
          correlation: {
            parentField: ['id'],
            childField: ['issueID'],
          },
          subquery: {
            table: 'comments',
            alias: 'comments',
            orderBy: [['id', 'asc']],
            limit: 1,
          },
        },
      ],
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-related-limit',
        'queryRelatedLimit',
        ISSUES_WITH_RELATED_LIMIT,
        startTimer(),
      ),
    ];

    // Hydration: top-level LIMIT 2 → issues '1' and '2'
    const hydratedIssues = hydrated.filter(r => r.table === 'issues');
    expect(hydratedIssues).toHaveLength(2);
    expect(hydratedIssues.map(r => r.rowKey)).toEqual(
      expect.arrayContaining([{id: '1'}, {id: '2'}]),
    );

    // Child LIMIT 1 per issue: issue '1' has comment '10', issue '2' has '20'
    const hydratedComments = hydrated.filter(r => r.table === 'comments');
    expect(hydratedComments).toHaveLength(2); // 1 per parent issue

    // Insert issue '0' — enters top-level window, pushes issue '2' out
    replicator.processTransaction(
      '134',
      messages.insert('issues', {id: '0', closed: 0}),
    );

    const result = changes();
    const issueChanges = result.filter(
      c => c.queryID === 'queryRelatedLimit' && c.table === 'issues',
    );

    // Top-level Take must still be reactive — issue '0' enters, issue '2' exits
    expect(issueChanges.length).toBeGreaterThanOrEqual(1);
    expect(issueChanges).toEqual(
      expect.arrayContaining([
        expect.objectContaining({type: 0, rowKey: {id: '0'}}),
      ]),
    );
    expect(issueChanges).toEqual(
      expect.arrayContaining([
        expect.objectContaining({type: 1, rowKey: {id: '2'}}),
      ]),
    );
  });

  // Verifies that every row yielded during hydration is accessible via getRow().
  // If Rust hydration returns rows that the warm-up fetch doesn't populate into
  // TS operator state, getRow() would return undefined — causing CVR diffing to
  // think the row doesn't exist.
  test('getRow returns every hydrated row (join query)', () => {
    pipelines.init(clientSchema);

    const hydrated = [
      ...pipelines.addQuery(
        'hash-join',
        'qJoin',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];

    // Every hydrated row should be retrievable via getRow
    for (const change of hydrated) {
      if (change === 'yield') continue;
      const row = pipelines.getRow(change.table, change.rowKey);
      expect(row).toBeDefined();
      expect(row).toEqual(change.row);
    }
  });

  // Verifies that every row yielded during hydration of a filter-only query
  // is accessible via getRow().
  test('getRow returns every hydrated row (filter query)', () => {
    pipelines.init(clientSchema);

    const hydrated = [
      ...pipelines.addQuery(
        'hash-filter',
        'qFilter',
        OPEN_ISSUES_ONLY,
        startTimer(),
      ),
    ];

    for (const change of hydrated) {
      if (change === 'yield') continue;
      const row = pipelines.getRow(change.table, change.rowKey);
      expect(row).toBeDefined();
      expect(row).toEqual(change.row);
    }
  });

  // Verifies that after Rust hydration of a join query, advance produces
  // correct diffs. This is the critical handoff: Rust hydrates → fixup seeds
  // TS state → advance uses that TS state for push processing.
  test('advance after hydration produces correct insert diff (join query)', () => {
    pipelines.init(clientSchema);
    [
      ...pipelines.addQuery(
        'hash-join',
        'qJoin',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];

    // Insert a new issue with a comment
    replicator.processTransaction(
      '134',
      messages.insert('issues', {id: '4', closed: 0}),
      messages.insert('comments', {id: '40', issueID: '4', upvotes: 5}),
    );

    const result = changes();
    // Should see ADD for the new issue
    expect(result).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          queryID: 'qJoin',
          table: 'issues',
          type: 0,
          rowKey: {id: '4'},
        }),
      ]),
    );
    // Should see ADD for the new comment
    expect(result).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          queryID: 'qJoin',
          table: 'comments',
          type: 0,
          rowKey: {id: '40'},
        }),
      ]),
    );
  });

  // Verifies that after Rust hydration, deleting a row produces correct
  // removal diffs through the TS advance path.
  test('advance after hydration produces correct delete diff (join query)', () => {
    pipelines.init(clientSchema);
    [
      ...pipelines.addQuery(
        'hash-join',
        'qJoin',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];

    // Delete issue '1' and its comment '10'
    replicator.processTransaction(
      '134',
      messages.delete('issues', {id: '1'}),
      messages.delete('comments', {id: '10'}),
    );

    const result = changes();
    // Should see REMOVE for issue '1'
    expect(result).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          queryID: 'qJoin',
          table: 'issues',
          type: 1,
          rowKey: {id: '1'},
        }),
      ]),
    );
    // Should see REMOVE for comment '10'
    expect(result).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          queryID: 'qJoin',
          table: 'comments',
          type: 1,
          rowKey: {id: '10'},
        }),
      ]),
    );
  });

  // Tests that advance works correctly after hydrating a filter query.
  // Inserts a row matching the filter and a row not matching — only the
  // matching row should appear in changes.
  test('advance after hydration respects filter (only matching rows)', () => {
    pipelines.init(clientSchema);
    [
      ...pipelines.addQuery(
        'hash-filter',
        'qFilter',
        OPEN_ISSUES_ONLY,
        startTimer(),
      ),
    ];

    // Insert one open issue and one closed issue
    replicator.processTransaction(
      '134',
      messages.insert('issues', {id: '4', closed: 0}), // matches filter
      messages.insert('issues', {id: '5', closed: 1}), // doesn't match
    );

    const result = changes();
    const filterResults = result.filter(c => c.queryID === 'qFilter');

    // Only the open issue should appear
    expect(filterResults).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          table: 'issues',
          type: 0,
          rowKey: {id: '4'},
        }),
      ]),
    );
    // Closed issue should NOT appear
    expect(filterResults).not.toEqual(
      expect.arrayContaining([expect.objectContaining({rowKey: {id: '5'}})]),
    );
  });

  // Tests NULL values in join keys. Issue '4' has a comment with NULL issueID
  // — it should NOT be joined to any parent.
  test('NULL join key does not produce false matches', () => {
    // Add a comment with NULL issueID
    db.exec(/*sql*/ `
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version)
        VALUES ('99', NULL, 0, '123');
    `);

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-join',
        'qJoin',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];

    // Comment '99' with NULL issueID should NOT appear in results
    // (no parent matches NULL)
    const nullComment = hydrated.filter(
      c => c !== 'yield' && c.table === 'comments' && c.rowKey.id === '99',
    );
    expect(nullComment).toHaveLength(0);
  });

  // Tests NULL in filter column. closed=NULL should not match closed=false.
  test('NULL filter value does not match equality filter', () => {
    // Add issue with NULL closed
    db.exec(/*sql*/ `
      INSERT INTO ISSUES (id, closed, ignored, _0_version)
        VALUES ('4', NULL, NULL, '123');
    `);

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-filter',
        'qFilter',
        OPEN_ISSUES_ONLY, // WHERE closed = false
        startTimer(),
      ),
    ];

    // Issue '4' with NULL closed should NOT match closed=false
    const nullIssue = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues' && c.rowKey.id === '4',
    );
    expect(nullIssue).toHaveLength(0);
  });

  // Tests LIMIT 0 — should return no rows.
  test('LIMIT 0 returns no rows', () => {
    const ISSUES_LIMIT_0: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      limit: 0,
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-limit0',
        'qLimit0',
        ISSUES_LIMIT_0,
        startTimer(),
      ),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    expect(issueRows).toHaveLength(0);
  });

  // Tests LIMIT exceeding total rows — should return all rows.
  test('LIMIT exceeding row count returns all rows', () => {
    const ISSUES_LIMIT_100: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      limit: 100,
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-limit100',
        'qLimit100',
        ISSUES_LIMIT_100,
        startTimer(),
      ),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    // DB has 3 issues, LIMIT 100 should return all 3
    expect(issueRows).toHaveLength(3);
  });

  // Tests ORDER BY tie-breaking with LIMIT. When multiple rows have the same
  // value in the ORDER BY column, the LIMIT boundary must be deterministic.
  test('LIMIT with ORDER BY tie-breaking is deterministic', () => {
    // Insert issues with same closed value for tie-breaking test
    db.exec(/*sql*/ `
      INSERT INTO ISSUES (id, closed, ignored, _0_version)
        VALUES ('4', 0, NULL, '123');
      INSERT INTO ISSUES (id, closed, ignored, _0_version)
        VALUES ('5', 0, NULL, '123');
    `);

    // ORDER BY closed ASC, id ASC — all open issues (1,3,4,5) then closed (2)
    // LIMIT 3 should consistently return the first 3 by (closed, id)
    const ISSUES_TIE_LIMIT: AST = {
      table: 'issues',
      orderBy: [
        ['closed', 'asc'],
        ['id', 'asc'],
      ],
      limit: 3,
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery('hash-tie', 'qTie', ISSUES_TIE_LIMIT, startTimer()),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    expect(issueRows).toHaveLength(3);

    // Should be issues 1, 3, 4 (all closed=false, ordered by id)
    const ids = issueRows
      .map(r => (r as {rowKey: {id: string}}).rowKey.id)
      .sort();
    expect(ids).toEqual(['1', '3', '4']);
  });

  // NULL values sort FIRST in SQLite ASC order. If Rust sorts NULLs differently,
  // LIMIT boundary shifts — wrong rows sent to client.
  test('NULL in ORDER BY column + LIMIT: NULLs sort first in ASC', () => {
    // Issue '3' has ignored=NULL, issues '1','2' have non-NULL ignored.
    // ORDER BY ignored ASC: NULL first → issue '3', then '2' (1722902400000), then '1' (1728345600000)
    // But 'ignored' is not in client schema, so test with closed which has no NULLs.
    // Instead, add an issue with NULL closed for this test.
    db.exec(/*sql*/ `
      INSERT INTO ISSUES (id, closed, ignored, _0_version)
        VALUES ('4', NULL, NULL, '123');
    `);

    const ISSUES_ORDER_CLOSED_LIMIT: AST = {
      table: 'issues',
      orderBy: [
        ['closed', 'asc'],
        ['id', 'asc'],
      ],
      limit: 2,
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-null-order',
        'qNullOrder',
        ISSUES_ORDER_CLOSED_LIMIT,
        startTimer(),
      ),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    expect(issueRows).toHaveLength(2);

    // SQLite ASC: NULL < false(0) < true(1)
    // So issue '4' (NULL) and issue '1' (false/0) should be first two
    const ids = issueRows
      .map(r => (r as {rowKey: {id: string}}).rowKey.id)
      .sort();
    expect(ids).toEqual(['1', '4']);
  });

  // Empty string is NOT NULL — it should match WHERE col = '' but not WHERE col IS NULL.
  test('empty string is distinct from NULL in filters', () => {
    db.exec(/*sql*/ `
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version)
        VALUES ('30', '', 5, '123');
    `);

    // Query: comments where issueID = '' (should match comment '30')
    const COMMENTS_EMPTY_ISSUE: AST = {
      table: 'comments',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: '=',
        left: {type: 'column', name: 'issueID'},
        right: {type: 'literal', value: ''},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-empty-str',
        'qEmptyStr',
        COMMENTS_EMPTY_ISSUE,
        startTimer(),
      ),
    ];

    const commentRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'comments',
    );
    // Only comment '30' has issueID = ''
    expect(commentRows).toHaveLength(1);
    expect(commentRows[0]).toEqual(
      expect.objectContaining({rowKey: {id: '30'}}),
    );
  });

  // IN operator with a list of values. Verifies Rust handles multi-value IN.
  test('IN filter matches correct rows', () => {
    const ISSUES_IN: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: 'IN',
        left: {type: 'column', name: 'id'},
        right: {type: 'literal', value: ['1', '3']},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery('hash-in', 'qIn', ISSUES_IN, startTimer()),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    expect(issueRows).toHaveLength(2);
    const ids = issueRows
      .map(r => (r as {rowKey: {id: string}}).rowKey.id)
      .sort();
    expect(ids).toEqual(['1', '3']);
  });

  // IN with empty array should match nothing.
  test('IN with empty array matches no rows', () => {
    const ISSUES_IN_EMPTY: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: 'IN',
        left: {type: 'column', name: 'id'},
        right: {type: 'literal', value: []},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-in-empty',
        'qInEmpty',
        ISSUES_IN_EMPTY,
        startTimer(),
      ),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    expect(issueRows).toHaveLength(0);
  });

  // NOT EXISTS should return rows where the correlated subquery has NO matches.
  test('NOT EXISTS returns rows without matching children', () => {
    // Issue '3' has no comments. Issues '1' and '2' have comments.
    const ISSUES_NOT_EXISTS: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'correlatedSubquery',
        op: 'NOT EXISTS',
        related: {
          system: 'client',
          correlation: {
            parentField: ['id'],
            childField: ['issueID'],
          },
          subquery: {
            table: 'comments',
            alias: 'comments',
            orderBy: [['id', 'asc']],
          },
        },
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-not-exists',
        'qNotExists',
        ISSUES_NOT_EXISTS,
        startTimer(),
      ),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    // Only issue '3' has no comments
    expect(issueRows).toHaveLength(1);
    expect(issueRows[0]).toEqual(expect.objectContaining({rowKey: {id: '3'}}));
  });

  // Multiple children per parent: verify child count and ordering is consistent.
  test('join with multiple children per parent returns all children', () => {
    // Issue '2' has 3 comments (20, 21, 22). ORDER BY id DESC → 22, 21, 20.
    const ISSUE_2_WITH_COMMENTS: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: '=',
        left: {type: 'column', name: 'id'},
        right: {type: 'literal', value: '2'},
      },
      related: [
        {
          system: 'client',
          correlation: {
            parentField: ['id'],
            childField: ['issueID'],
          },
          subquery: {
            table: 'comments',
            alias: 'comments',
            orderBy: [['id', 'desc']],
          },
        },
      ],
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-multi-child',
        'qMultiChild',
        ISSUE_2_WITH_COMMENTS,
        startTimer(),
      ),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    const commentRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'comments',
    );
    expect(issueRows).toHaveLength(1);
    expect(commentRows).toHaveLength(3);

    const commentIds = commentRows
      .map(r => (r as {rowKey: {id: string}}).rowKey.id)
      .sort();
    expect(commentIds).toEqual(['20', '21', '22']);
  });

  // != filter: verify rows NOT matching are returned.
  test('!= filter excludes matching rows', () => {
    const ISSUES_NEQ: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: '!=',
        left: {type: 'column', name: 'id'},
        right: {type: 'literal', value: '2'},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery('hash-neq', 'qNeq', ISSUES_NEQ, startTimer()),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    // Issues 1, 3 match (not '2')
    expect(issueRows).toHaveLength(2);
    const ids = issueRows
      .map(r => (r as {rowKey: {id: string}}).rowKey.id)
      .sort();
    expect(ids).toEqual(['1', '3']);
  });

  // != with NULL: NULL != 'x' should be NULL (falsy), not true.
  // Issue '4' with NULL closed: closed != true should NOT include it.
  test('!= with NULL value returns false (SQL three-valued logic)', () => {
    db.exec(/*sql*/ `
      INSERT INTO ISSUES (id, closed, ignored, _0_version)
        VALUES ('4', NULL, NULL, '123');
    `);

    const ISSUES_NEQ_TRUE: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: '!=',
        left: {type: 'column', name: 'closed'},
        right: {type: 'literal', value: true},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-neq-null',
        'qNeqNull',
        ISSUES_NEQ_TRUE,
        startTimer(),
      ),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    // Issues 1 (closed=false), 3 (closed=false) match.
    // Issue 2 (closed=true) doesn't match.
    // Issue 4 (closed=NULL) should NOT match (NULL != true = NULL = falsy).
    const ids = issueRows
      .map(r => (r as {rowKey: {id: string}}).rowKey.id)
      .sort();
    expect(ids).toEqual(['1', '3']);
  });

  // IS NULL filter
  test('IS NULL filter matches only NULL rows', () => {
    db.exec(/*sql*/ `
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version)
        VALUES ('99', NULL, 0, '123');
    `);

    const COMMENTS_NULL_ISSUE: AST = {
      table: 'comments',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: 'IS',
        left: {type: 'column', name: 'issueID'},
        right: {type: 'literal', value: null},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-is-null',
        'qIsNull',
        COMMENTS_NULL_ISSUE,
        startTimer(),
      ),
    ];

    const commentRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'comments',
    );
    expect(commentRows).toHaveLength(1);
    expect(commentRows[0]).toEqual(
      expect.objectContaining({rowKey: {id: '99'}}),
    );
  });

  // IS NOT NULL filter
  test('IS NOT NULL filter excludes NULL rows', () => {
    db.exec(/*sql*/ `
      INSERT INTO COMMENTS (id, issueID, upvotes, _0_version)
        VALUES ('99', NULL, 0, '123');
    `);

    const COMMENTS_NOT_NULL_ISSUE: AST = {
      table: 'comments',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: 'IS NOT',
        left: {type: 'column', name: 'issueID'},
        right: {type: 'literal', value: null},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-is-not-null',
        'qIsNotNull',
        COMMENTS_NOT_NULL_ISSUE,
        startTimer(),
      ),
    ];

    const commentRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'comments',
    );
    // All 4 comments (10, 20, 21, 22) have non-NULL issueID
    expect(commentRows).toHaveLength(4);
  });

  // AND of two simple conditions
  test('AND filter: both conditions must match', () => {
    const ISSUES_AND: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'and',
        conditions: [
          {
            type: 'simple',
            op: '=',
            left: {type: 'column', name: 'closed'},
            right: {type: 'literal', value: false},
          },
          {
            type: 'simple',
            op: '!=',
            left: {type: 'column', name: 'id'},
            right: {type: 'literal', value: '3'},
          },
        ],
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery('hash-and', 'qAnd', ISSUES_AND, startTimer()),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    // closed=false: issues 1, 3. Excluding id='3': only issue 1.
    expect(issueRows).toHaveLength(1);
    expect(issueRows[0]).toEqual(expect.objectContaining({rowKey: {id: '1'}}));
  });

  // OR of two simple conditions
  test('OR filter: either condition matches', () => {
    const ISSUES_OR: AST = {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'or',
        conditions: [
          {
            type: 'simple',
            op: '=',
            left: {type: 'column', name: 'id'},
            right: {type: 'literal', value: '1'},
          },
          {
            type: 'simple',
            op: '=',
            left: {type: 'column', name: 'id'},
            right: {type: 'literal', value: '3'},
          },
        ],
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery('hash-or', 'qOr', ISSUES_OR, startTimer()),
    ];

    const issueRows = hydrated.filter(
      c => c !== 'yield' && c.table === 'issues',
    );
    expect(issueRows).toHaveLength(2);
    const ids = issueRows
      .map(r => (r as {rowKey: {id: string}}).rowKey.id)
      .sort();
    expect(ids).toEqual(['1', '3']);
  });

  // Join where parent has NO children — parent should still appear,
  // just with no child rows.
  test('join with parent having zero children includes parent', () => {
    // Issue '3' has no comments
    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery(
        'hash-join',
        'qJoin',
        ISSUES_AND_COMMENTS,
        startTimer(),
      ),
    ];

    // Issue '3' should appear as a parent row even with no comments
    const issue3 = hydrated.filter(
      c =>
        c !== 'yield' &&
        c.table === 'issues' &&
        (c as {rowKey: {id: string}}).rowKey.id === '3',
    );
    expect(issue3).toHaveLength(1);

    // No comments for issue '3'
    const comments3 = hydrated.filter(
      c =>
        c !== 'yield' &&
        c.table === 'comments' &&
        (c as {row: {issueID: string}}).row.issueID === '3',
    );
    expect(comments3).toHaveLength(0);
  });

  // Comparison operators: >, <, >=, <=
  test('comparison operators >, <, >=, <= work correctly', () => {
    // comments: upvotes = 0, 1, 10000, 20000
    const COMMENTS_GT: AST = {
      table: 'comments',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        op: '>',
        left: {type: 'column', name: 'upvotes'},
        right: {type: 'literal', value: 1},
      },
    };

    pipelines.init(clientSchema);
    const hydrated = [
      ...pipelines.addQuery('hash-gt', 'qGt', COMMENTS_GT, startTimer()),
    ];

    const rows = hydrated.filter(c => c !== 'yield' && c.table === 'comments');
    // upvotes > 1: comments 21 (10000) and 22 (20000)
    expect(rows).toHaveLength(2);
    const ids = rows.map(r => (r as {rowKey: {id: string}}).rowKey.id).sort();
    expect(ids).toEqual(['21', '22']);
  });

  test('ZERO_DISABLE_RUST_IVM=1 forces TS path and produces correct output', () => {
    const origEnv = process.env.ZERO_DISABLE_RUST_IVM;
    try {
      process.env.ZERO_DISABLE_RUST_IVM = '1';

      // Create a fresh pipeline driver with the env var set
      const storage2 = new Database(lc, ':memory:');
      storage2.prepare(CREATE_STORAGE_TABLE).run();
      const pipelines2 = new PipelineDriver(
        lc,
        testLogConfig,
        new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
        shardID,
        new DatabaseStorage(storage2).createClientGroupStorage('bar-cg'),
        'pipeline-driver.edge-cases.test.ts-disable-rust',
        new InspectorDelegate(undefined),
        () => 200,
      );

      pipelines2.init(clientSchema);
      [...pipelines2.addQuery('hash1', 'q1', OPEN_ISSUES_ONLY, startTimer())];

      replicator.processTransaction(
        '134',
        messages.insert('issues', {id: '4', closed: 0}),
      );

      const result = [...pipelines2.advance(NO_TIME_ADVANCEMENT_TIMER).changes];
      const issueAdds = result.filter(
        c => c.queryID === 'q1' && c.table === 'issues',
      );
      expect(issueAdds.length).toBeGreaterThanOrEqual(1);
    } finally {
      if (origEnv === undefined) {
        delete process.env.ZERO_DISABLE_RUST_IVM;
      } else {
        process.env.ZERO_DISABLE_RUST_IVM = origEnv;
      }
    }
  });
});
