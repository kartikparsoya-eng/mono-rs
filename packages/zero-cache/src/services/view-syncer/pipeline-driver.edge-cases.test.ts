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
