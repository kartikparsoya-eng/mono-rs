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
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('pipeline-driver join topologies', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_join_topology_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.join-topology.test.ts',
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
      CREATE TABLE orgs (
        id TEXT PRIMARY KEY,
        name TEXT,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE teams (
        id TEXT PRIMARY KEY,
        "orgID" TEXT,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE members (
        id TEXT PRIMARY KEY,
        "teamID" TEXT,
        name TEXT,
        _0_version TEXT NOT NULL
      );
      CREATE TABLE projects (
        id TEXT PRIMARY KEY,
        "orgID" TEXT,
        title TEXT,
        _0_version TEXT NOT NULL
      );

      INSERT INTO orgs VALUES ('o1', 'Acme', '123');
      INSERT INTO orgs VALUES ('o2', 'Globex', '123');
      INSERT INTO teams VALUES ('t1', 'o1', '123');
      INSERT INTO teams VALUES ('t2', 'o1', '123');
      INSERT INTO teams VALUES ('t3', 'o2', '123');
      INSERT INTO members VALUES ('m1', 't1', 'Alice', '123');
      INSERT INTO members VALUES ('m2', 't1', 'Bob', '123');
      INSERT INTO members VALUES ('m3', 't2', 'Carol', '123');
      INSERT INTO projects VALUES ('pr1', 'o1', 'Widget', '123');
      INSERT INTO projects VALUES ('pr2', 'o2', 'Gadget', '123');
    `);

    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const orgs = table('orgs')
    .columns({id: string(), name: string()})
    .primaryKey('id');
  const teams = table('teams')
    .columns({id: string(), orgID: string()})
    .primaryKey('id');
  const members = table('members')
    .columns({id: string(), teamID: string(), name: string()})
    .primaryKey('id');
  const projects = table('projects')
    .columns({id: string(), orgID: string(), title: string()})
    .primaryKey('id');

  const clientSchema = createSchema({tables: [orgs, teams, members, projects]});

  const messages = new ReplicationMessages({
    orgs: 'id',
    teams: 'id',
    members: 'id',
    projects: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER) {
    return [...pipelines.advance(timer).changes];
  }

  // Topology 1: Parent -> Child -> Grandchild (3-level join)
  const ORGS_TEAMS_MEMBERS: AST = {
    table: 'orgs',
    orderBy: [['id', 'asc']],
    related: [
      {
        system: 'client',
        correlation: {parentField: ['id'], childField: ['orgID']},
        subquery: {
          table: 'teams',
          alias: 'teams',
          orderBy: [['id', 'asc']],
          related: [
            {
              system: 'client',
              correlation: {parentField: ['id'], childField: ['teamID']},
              subquery: {
                table: 'members',
                alias: 'members',
                orderBy: [['id', 'asc']],
              },
            },
          ],
        },
      },
    ],
  };

  // Topology 2: Sibling joins (two children of same parent)
  const ORGS_WITH_SIBLINGS: AST = {
    table: 'orgs',
    orderBy: [['id', 'asc']],
    related: [
      {
        system: 'client',
        correlation: {parentField: ['id'], childField: ['orgID']},
        subquery: {
          table: 'teams',
          alias: 'teams',
          orderBy: [['id', 'asc']],
        },
      },
      {
        system: 'client',
        correlation: {parentField: ['id'], childField: ['orgID']},
        subquery: {
          table: 'projects',
          alias: 'projects',
          orderBy: [['id', 'asc']],
        },
      },
    ],
  };

  // Topology 3: Parent with EXISTS on child
  const ORGS_WITH_TEAMS_EXISTS: AST = {
    table: 'orgs',
    orderBy: [['id', 'asc']],
    where: {
      type: 'correlatedSubquery',
      op: 'EXISTS',
      related: {
        system: 'client',
        correlation: {parentField: ['id'], childField: ['orgID']},
        subquery: {
          table: 'teams',
          alias: 'teams',
          orderBy: [['id', 'asc']],
        },
      },
    },
  };

  test('3-level join hydration', () => {
    pipelines.init(clientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', ORGS_TEAMS_MEMBERS, startTimer()),
    ];

    const orgRows = hydration.filter(r => r.table === 'orgs');
    const teamRows = hydration.filter(r => r.table === 'teams');
    const memberRows = hydration.filter(r => r.table === 'members');

    expect(orgRows.map(r => r.row.id).sort()).toEqual(['o1', 'o2']);
    expect(teamRows.map(r => r.row.id).sort()).toEqual(['t1', 't2', 't3']);
    expect(memberRows.map(r => r.row.id).sort()).toEqual(['m1', 'm2', 'm3']);
  });

  test('3-level join reacts to grandchild insert', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', ORGS_TEAMS_MEMBERS, startTimer())];

    replicator.processTransaction(
      '134',
      messages.insert('members', {id: 'm4', teamID: 't3', name: 'Dave'}),
    );

    const result = changes();
    const memberChanges = result.filter(c => c.table === 'members');
    expect(memberChanges.some(c => c.row?.id === 'm4' && c.type === 0)).toBe(
      true,
    );
  });

  test('3-level join reacts to middle-level delete', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', ORGS_TEAMS_MEMBERS, startTimer())];

    replicator.processTransaction('134', messages.delete('teams', {id: 't2'}));

    const result = changes();
    // t2 should be removed
    const teamChanges = result.filter(c => c.table === 'teams');
    expect(
      teamChanges.some(
        c => (c.row?.id === 't2' || c.rowKey?.id === 't2') && c.type === 1,
      ),
    ).toBe(true);
    // m3 (child of t2) should also be removed
    const memberChanges = result.filter(c => c.table === 'members');
    expect(
      memberChanges.some(
        c => (c.row?.id === 'm3' || c.rowKey?.id === 'm3') && c.type === 1,
      ),
    ).toBe(true);
  });

  test('sibling join hydration', () => {
    pipelines.init(clientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', ORGS_WITH_SIBLINGS, startTimer()),
    ];

    const orgRows = hydration.filter(r => r.table === 'orgs');
    const teamRows = hydration.filter(r => r.table === 'teams');
    const projectRows = hydration.filter(r => r.table === 'projects');

    expect(orgRows.map(r => r.row.id).sort()).toEqual(['o1', 'o2']);
    expect(teamRows.map(r => r.row.id).sort()).toEqual(['t1', 't2', 't3']);
    expect(projectRows.map(r => r.row.id).sort()).toEqual(['pr1', 'pr2']);
  });

  test('sibling join reacts to one sibling change', () => {
    pipelines.init(clientSchema);
    [...pipelines.addQuery('hash1', 'q1', ORGS_WITH_SIBLINGS, startTimer())];

    replicator.processTransaction(
      '134',
      messages.insert('projects', {id: 'pr3', orgID: 'o1', title: 'Doodad'}),
    );

    const result = changes();
    const projectChanges = result.filter(c => c.table === 'projects');
    expect(projectChanges.some(c => c.row?.id === 'pr3' && c.type === 0)).toBe(
      true,
    );
  });

  test('exists-on-child hydration', () => {
    pipelines.init(clientSchema);
    const hydration = [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        ORGS_WITH_TEAMS_EXISTS,
        startTimer(),
      ),
    ];

    // Both o1 and o2 have teams
    const orgRows = hydration.filter(r => r.table === 'orgs');
    expect(orgRows.map(r => r.row.id).sort()).toEqual(['o1', 'o2']);
  });

  test('exists-on-child reacts to last child removal', () => {
    pipelines.init(clientSchema);
    [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        ORGS_WITH_TEAMS_EXISTS,
        startTimer(),
      ),
    ];

    // Delete t3 (o2's only team)
    replicator.processTransaction('134', messages.delete('teams', {id: 't3'}));

    const result = changes();
    // o2 should be removed from results
    const orgChanges = result.filter(
      c => c.table === 'orgs' && c.queryID === 'q1',
    );
    expect(
      orgChanges.some(
        c => (c.row?.id === 'o2' || c.rowKey?.id === 'o2') && c.type === 1,
      ),
    ).toBe(true);
  });
});
