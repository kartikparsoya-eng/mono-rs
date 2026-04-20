/**
 * IVM Push Path Profiler
 *
 * Measures where time is spent during pipeline advancement (the push hot path).
 * Instruments key stages:
 *   1. Diff iteration + deepEqual (PipelineDriver.#advance overhead)
 *   2. TableSource.genPush — checkExists (SQLite read) + fan-out to operators
 *   3. Operator chain (Filter, Take, Join, Exists) — including Storage I/O
 *   4. TableSource.#writeChange — SQLite writes (INSERT/DELETE/UPDATE)
 *   5. Streamer/accumulator overhead
 *
 * Usage: npx vitest bench packages/zero-cache/src/services/view-syncer/pipeline-push-profile.bench.ts
 */

import {describe, bench} from 'vitest';
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

// ─── Profiling instrumentation ───────────────────────────────────────────────

type Profile = {
  totalAdvanceMs: number;
  numChangesProcessed: number;
  iterations: number;
};

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// ─── Test setup (mirrors pipeline-driver.test.ts) ────────────────────────────

const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

const issues = table('issues')
  .columns({id: string(), closed: boolean()})
  .primaryKey('id');
const comments = table('comments')
  .columns({id: string(), issueID: string(), upvotes: number()})
  .primaryKey('id');
const issueLabels = table('issueLabels')
  .columns({issueID: string(), labelID: string(), legacyID: string()})
  .primaryKey('issueID', 'labelID');
const labels = table('labels')
  .columns({id: string(), name: string()})
  .primaryKey('id');

const clientSchema = createSchema({
  tables: [issues, comments, issueLabels, labels],
});

// Query with JOIN (issues + related comments)
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

// Query with EXISTS (issues WHERE EXISTS issueLabels WHERE EXISTS labels)
const ISSUES_WITH_EXISTS: AST = {
  table: 'issues',
  orderBy: [['id', 'asc']],
  where: {
    type: 'correlatedSubquery',
    op: 'EXISTS',
    related: {
      system: 'client',
      correlation: {
        parentField: ['id'],
        childField: ['issueID'],
      },
      subquery: {
        table: 'issueLabels',
        alias: 'labels',
        orderBy: [
          ['issueID', 'asc'],
          ['labelID', 'asc'],
        ],
        where: {
          type: 'correlatedSubquery',
          op: 'EXISTS',
          related: {
            system: 'client',
            correlation: {
              parentField: ['labelID'],
              childField: ['id'],
            },
            subquery: {
              table: 'labels',
              alias: 'labels',
              orderBy: [['id', 'asc']],
              where: {
                type: 'simple',
                left: {type: 'column', name: 'name'},
                op: '=',
                right: {type: 'literal', value: 'bug'},
              },
            },
          },
        },
      },
    },
  },
};

// Simple filter query (issues WHERE closed = false)
const ISSUES_FILTERED: AST = {
  table: 'issues',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    left: {type: 'column', name: 'closed'},
    op: '=',
    right: {type: 'literal', value: false},
  },
};

// Query with LIMIT (Take operator)
const ISSUES_WITH_LIMIT: AST = {
  table: 'issues',
  orderBy: [['id', 'asc']],
  limit: 2,
};

// ─── Benchmark harness ───────────────────────────────────────────────────────

describe('IVM Push Path Profiling', () => {
  let dbFile: DbFile;
  let db: DB;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;
  let messages: ReplicationMessages<Record<string, string | string[]>>;

  function setup(numRows: number) {
    const lc = createSilentLogContext();
    dbFile = new DbFile('push_profile_bench');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('bench-cg'),
      'push-profile-bench',
      new InspectorDelegate(undefined),
      () => 200,
    );

    db = dbFile.connect(lc);
    initReplicationState(db, ['zero_data'], '01');
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
        _0_version TEXT NOT NULL
      );
      CREATE TABLE comments (
        id TEXT PRIMARY KEY,
        issueID TEXT,
        upvotes INTEGER,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE "issueLabels" (
        issueID TEXT,
        labelID TEXT,
        legacyID TEXT,
        _0_version TEXT NOT NULL,
        PRIMARY KEY (issueID, labelID)
      );
      CREATE TABLE "labels" (
        id TEXT PRIMARY KEY,
        name TEXT,
        _0_version TEXT NOT NULL
      );
    `);

    // Seed data
    const insertIssue = db.prepare(
      `INSERT INTO issues (id, closed, _0_version) VALUES (?, ?, '01')`,
    );
    const insertComment = db.prepare(
      `INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES (?, ?, ?, '01')`,
    );
    const insertLabel = db.prepare(
      `INSERT INTO labels (id, name, _0_version) VALUES (?, ?, '01')`,
    );
    const insertIssueLabel = db.prepare(
      `INSERT INTO "issueLabels" (issueID, labelID, legacyID, _0_version) VALUES (?, ?, ?, '01')`,
    );

    db.exec('BEGIN');
    for (let i = 0; i < numRows; i++) {
      insertIssue.run(`issue-${i}`, i % 2 === 0 ? 0 : 1);
      insertComment.run(`comment-${i}`, `issue-${i}`, i * 100);
      if (i < 20) {
        insertLabel.run(`label-${i}`, i % 3 === 0 ? 'bug' : 'feature');
        insertIssueLabel.run(`issue-${i}`, `label-${i}`, `${i}-${i}`);
      }
    }
    db.exec('COMMIT');

    populateFromExistingTables(db, listTables(db, false));

    messages = new ReplicationMessages({
      issues: 'id',
      comments: 'id',
      issueLabels: ['issueID', 'labelID'],
      labels: 'id',
      [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
    });
    replicator = fakeReplicator(lc, db);
  }

  function teardown() {
    dbFile.delete();
  }

  // ─── Profiling runs ──────────────────────────────────────────────────────

  function profileAdvance(
    queryName: string,
    ast: AST,
    numRows: number,
    numInserts: number,
  ): Profile {
    setup(numRows);
    try {
      pipelines.init(clientSchema);
      const lc = createSilentLogContext();
      const timer = new TimeSliceTimer(lc).startWithoutYielding();
      [...pipelines.addQuery('h1', 'q1', ast, timer)];

      // Now push `numInserts` changes via replicator and measure advance time
      const insertMsgs = [];
      for (let i = 0; i < numInserts; i++) {
        insertMsgs.push(
          messages.insert('issues', {
            id: `new-issue-${i}`,
            closed: i % 2,
          }),
        );
        insertMsgs.push(
          messages.insert('comments', {
            id: `new-comment-${i}`,
            issueID: `new-issue-${i}`,
            upvotes: BigInt(i * 10),
          }),
        );
      }
      replicator.processTransaction('02', ...insertMsgs);

      const start = performance.now();
      const result = pipelines.advance(NO_TIME_ADVANCEMENT_TIMER);
      const changes = [...result.changes];
      const elapsed = performance.now() - start;

      return {
        totalAdvanceMs: elapsed,
        numChangesProcessed: changes.length,
        iterations: 1,
      };
    } finally {
      teardown();
    }
  }

  // ─── Detailed timing breakdown ─────────────────────────────────────────

  function runDetailedProfile(numRows: number, numInserts: number) {
    setup(numRows);
    try {
      pipelines.init(clientSchema);
      const lc = createSilentLogContext();
      const timer = new TimeSliceTimer(lc).startWithoutYielding();

      // Hydrate all query types
      [...pipelines.addQuery('h1', 'q1', ISSUES_AND_COMMENTS, timer)];
      [...pipelines.addQuery('h2', 'q2', ISSUES_WITH_EXISTS, timer)];
      [...pipelines.addQuery('h3', 'q3', ISSUES_FILTERED, timer)];
      [...pipelines.addQuery('h4', 'q4', ISSUES_WITH_LIMIT, timer)];

      // Push changes
      const insertMsgs = [];
      for (let i = 0; i < numInserts; i++) {
        insertMsgs.push(
          messages.insert('issues', {id: `new-${i}`, closed: i % 2}),
        );
        insertMsgs.push(
          messages.insert('comments', {
            id: `nc-${i}`,
            issueID: `new-${i}`,
            upvotes: BigInt(i),
          }),
        );
      }
      replicator.processTransaction('02', ...insertMsgs);

      const start = performance.now();
      const result = pipelines.advance(NO_TIME_ADVANCEMENT_TIMER);
      const changes = [...result.changes];
      const elapsed = performance.now() - start;

      return {totalAdvanceMs: elapsed, numChanges: changes.length};
    } finally {
      teardown();
    }
  }

  // ─── Console report ────────────────────────────────────────────────────

  describe('profile report', () => {
    const ROWS = 500;
    const INSERTS = 100;

    bench(
      'issues+comments (JOIN)',
      () => {
        profileAdvance('issues+comments', ISSUES_AND_COMMENTS, ROWS, INSERTS);
      },
      {iterations: 20, warmupIterations: 3},
    );

    bench(
      'issues with EXISTS',
      () => {
        profileAdvance('issues+EXISTS', ISSUES_WITH_EXISTS, ROWS, INSERTS);
      },
      {iterations: 20, warmupIterations: 3},
    );

    bench(
      'issues filtered (WHERE closed=false)',
      () => {
        profileAdvance('issues filtered', ISSUES_FILTERED, ROWS, INSERTS);
      },
      {iterations: 20, warmupIterations: 3},
    );

    bench(
      'issues with LIMIT (Take operator)',
      () => {
        profileAdvance('issues+LIMIT', ISSUES_WITH_LIMIT, ROWS, INSERTS);
      },
      {iterations: 20, warmupIterations: 3},
    );

    bench(
      'all queries combined (4 pipelines)',
      () => {
        runDetailedProfile(ROWS, INSERTS);
      },
      {iterations: 20, warmupIterations: 3},
    );
  });
});
