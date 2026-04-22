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
  unicodeClientSchema,
  UNICODE_ROWS,
  UNICODE_ITEMS_QUERY,
  UNICODE_FILTER_EMOJI_QUERY,
  UNICODE_LIKE_QUERY,
} from './pipeline-driver.fixtures.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('pipeline-driver Unicode handling', () => {
  const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;
  let dbFile: DbFile;
  let db: DB;
  let lc: LogContext;
  let pipelines: PipelineDriver;
  let replicator: FakeReplicator;

  beforeEach(() => {
    lc = createSilentLogContext();
    dbFile = new DbFile('pipelines_unicode_test');
    dbFile.connect(lc).pragma('journal_mode = wal2');

    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage('foo-client-group'),
      'pipeline-driver.unicode.test.ts',
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
      CREATE TABLE unicode_items (
        id TEXT PRIMARY KEY,
        text TEXT,
        category TEXT,
        _0_version TEXT NOT NULL
      );
    `);

    // Insert UNICODE_ROWS using parameterized approach to avoid SQL escaping issues
    const stmt = db.prepare(
      'INSERT INTO unicode_items (id, text, category, _0_version) VALUES (?, ?, ?, ?)',
    );
    for (const row of UNICODE_ROWS) {
      stmt.run(row.id, row.text, row.category, '123');
    }

    populateFromExistingTables(db, listTables(db, false));
    replicator = fakeReplicator(lc, db);
  });

  afterEach(() => {
    dbFile.delete();
  });

  const messages = new ReplicationMessages({
    unicode_items: 'id',
    [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
  });

  function startTimer() {
    return new TimeSliceTimer(lc).startWithoutYielding();
  }

  function changes(timer: Timer = NO_TIME_ADVANCEMENT_TIMER) {
    return [...pipelines.advance(timer).changes];
  }

  test('Unicode strings hydrate correctly', () => {
    pipelines.init(unicodeClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', UNICODE_ITEMS_QUERY, startTimer()),
    ];
    expect(hydration.length).toBe(7);
    const u1 = hydration.find(r => r.row?.id === 'u1');
    expect(u1?.row?.text).toBe(UNICODE_ROWS[0].text);
    const u2 = hydration.find(r => r.row?.id === 'u2');
    expect(u2?.row?.text).toBe(UNICODE_ROWS[1].text);
  });

  test('emoji filter equality works', () => {
    pipelines.init(unicodeClientSchema);
    const hydration = [
      ...pipelines.addQuery(
        'hash1',
        'q1',
        UNICODE_FILTER_EMOJI_QUERY,
        startTimer(),
      ),
    ];
    expect(hydration.length).toBe(1);
    expect(hydration[0].row?.id).toBe('u1');
  });

  test('LIKE predicate works with Unicode', () => {
    pipelines.init(unicodeClientSchema);
    const hydration = [
      ...pipelines.addQuery('hash1', 'q1', UNICODE_LIKE_QUERY, startTimer()),
    ];
    // Should match u7 ('cafe') and possibly u5 (precomposed e-acute) depending on LIKE semantics
    // At minimum u7 should match since it starts with 'caf'
    const ids = hydration.map(r => r.row?.id);
    expect(ids).toContain('u7');
  });

  test('Unicode insert via replication preserves content', () => {
    pipelines.init(unicodeClientSchema);
    [...pipelines.addQuery('hash1', 'q1', UNICODE_ITEMS_QUERY, startTimer())];

    const zwjEmoji = '\u{1F469}\u200D\u{1F52C}'; // woman scientist
    replicator.processTransaction(
      '134',
      messages.insert('unicode_items', {
        id: 'u8',
        text: zwjEmoji,
        category: 'zwj',
      }),
    );

    const result = changes();
    const added = result.find(c => c.row?.id === 'u8');
    expect(added).toBeDefined();
    expect(added?.row?.text).toBe(zwjEmoji);
  });

  test('Unicode Rust vs TS produces identical results', () => {
    pipelines.init(unicodeClientSchema);
    const rustHydration = [
      ...pipelines.addQuery('hash1', 'q1', UNICODE_ITEMS_QUERY, startTimer()),
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
        'pipeline-driver.unicode.test.ts-ts-path',
        new InspectorDelegate(undefined),
        () => 200,
      );

      pipelines2.init(unicodeClientSchema);
      const tsHydration = [
        ...pipelines2.addQuery(
          'hash1',
          'q1',
          UNICODE_ITEMS_QUERY,
          startTimer(),
        ),
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
