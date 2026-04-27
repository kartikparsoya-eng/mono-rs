/**
 * Benchmark: Rust IVM (RustPipeline with per-core Rayon parallelism)
 *            vs TypeScript IVM (PipelineDriver with TS operator trees)
 *
 * Strategy:
 * - ZERO_DISABLE_RUST_IVM=1 is set in vitest.config.bench.ts so PipelineDriver
 *   uses pure TypeScript operators for hydrate AND advance.
 * - RustPipeline NAPI class is used directly for Rust benchmarks — this is the
 *   real per-core Rayon IVM with RwLock<Vec<Mutex<PipelineState>>> and par_iter().
 *
 * The Change format for RustPipeline.advance() matches diff.rs:
 *   { table, prevValues: Row[], nextValue: Row | null, rowKey: JsonValue }
 */

// Must set env BEFORE any pipeline-driver imports (belt + suspenders with config)
process.env['ZERO_DISABLE_RUST_IVM'] = '1';

import {createRequire} from 'module';
import {bench, describe} from 'vitest';
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
import {fakeReplicator, ReplicationMessages} from '../replicator/test-utils.ts';
import {decodeAdvanceResultBuf} from './decode-advance-buf.ts';
import {PipelineDriver, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';

// Load Rust NAPI bindings directly (bypassing PipelineDriver's env check)
const esmRequire = createRequire(import.meta.url);
let RustPipelineClass:
  | (new (
      dbPath: string,
      queriesJson: string,
    ) => {
      hydrate(): Buffer;
      advance(changesJson: string): Buffer;
      swapSnapshot(newDbPath: string): void;
      addQuery(queryJson: string): void;
      removeQuery(queryId: string): void;
      pipelineCount(): number;
    })
  | undefined;

try {
  const bindings = esmRequire('zqlite-rs');
  RustPipelineClass = bindings?.RustPipeline;
} catch {
  // Rust bindings not available — Rust benchmarks will be skipped
}

// ---------- Schema & AST definitions ----------

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

const ISSUES_AND_COMMENTS: AST = {
  table: 'issues',
  orderBy: [['id', 'desc']],
  related: [
    {
      system: 'client',
      correlation: {parentField: ['id'], childField: ['issueID']},
      subquery: {
        table: 'comments',
        alias: 'comments',
        orderBy: [['id', 'desc']],
      },
    },
  ],
};

const SIMPLE_ISSUES: AST = {
  table: 'issues',
  orderBy: [['id', 'asc']],
};

const ISSUES_WITH_EXISTS: AST = {
  table: 'issues',
  orderBy: [['id', 'asc']],
  where: {
    type: 'correlatedSubquery',
    op: 'EXISTS',
    related: {
      system: 'client',
      correlation: {parentField: ['id'], childField: ['issueID']},
      subquery: {
        table: 'issueLabels',
        alias: 'labels',
        orderBy: [
          ['issueID', 'asc'],
          ['labelID', 'asc'],
        ],
      },
    },
  },
};

const messages = new ReplicationMessages({
  issues: 'id',
  comments: 'id',
  issueLabels: ['issueID', 'labelID'],
  [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
});

const NO_TIMER: Timer = {elapsedLap: () => 0, totalElapsed: () => 0};

/** Zero-pad version strings so lexicographic comparison works (e.g. '00200' < '01001'). */
function ver(n: number): string {
  return String(n).padStart(10, '0');
}

// ---------- DB setup helpers ----------

function setupDB(prefix: string): {dbFile: DbFile; db: DB} {
  const lc = createSilentLogContext();
  const dbFile = new DbFile(`bench_${prefix}_${Date.now()}`);
  dbFile.connect(lc).pragma('journal_mode = wal2');
  const db = dbFile.connect(lc);
  // Disable auto-checkpoint so the WAL doesn't invalidate Snapshotter diffs
  // during repeated advance benchmarks.
  db.pragma('wal_autocheckpoint = 0');

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
      _0_version TEXT NOT NULL
    );
    CREATE TABLE "issueLabels" (
      issueID TEXT,
      labelID TEXT,
      legacyID "TEXT|NOT_NULL",
      _0_version TEXT NOT NULL,
      PRIMARY KEY (issueID, labelID)
    );
    CREATE TABLE "labels" (
      id TEXT PRIMARY KEY,
      name TEXT,
      _0_version TEXT NOT NULL
    );
  `);

  db.exec(/*sql*/ `
    INSERT INTO issues (id, closed, ignored, _0_version) VALUES ('1', 0, 1728345600000, '123');
    INSERT INTO issues (id, closed, ignored, _0_version) VALUES ('2', 1, 1722902400000, '123');
    INSERT INTO issues (id, closed, ignored, _0_version) VALUES ('3', 0, null, '123');
    INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES ('10', '1', 0, '123');
    INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES ('20', '2', 1, '123');
    INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES ('21', '2', 10000, '123');
    INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES ('22', '2', 20000, '123');
    INSERT INTO "issueLabels" (issueID, labelID, legacyID, _0_version) VALUES ('1', '1', '1-1', '123');
    INSERT INTO "labels" (id, name, _0_version) VALUES ('1', 'bug', '123');
    INSERT INTO "labels" (id, name, _0_version) VALUES ('2', 'Bug', '123');
    INSERT INTO "labels" (id, name, _0_version) VALUES ('3', 'FEATURE', '123');
    INSERT INTO "labels" (id, name, _0_version) VALUES ('4', 'feature', '123');
  `);

  populateFromExistingTables(db, listTables(db, false));
  return {dbFile, db};
}

function makeBulkDB(
  prefix: string,
  rowCount: number,
): {dbFile: DbFile; db: DB} {
  const {dbFile, db} = setupDB(prefix);

  const insertIssue = db.prepare(
    `INSERT INTO issues (id, closed, ignored, _0_version) VALUES (?, ?, ?, '123')`,
  );
  const insertComment = db.prepare(
    `INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES (?, ?, ?, '123')`,
  );

  db.exec('BEGIN');
  for (let i = 100; i < 100 + rowCount; i++) {
    insertIssue.run(`i${i}`, i % 2, null);
    insertComment.run(`c${i}`, `i${i}`, i * 10);
  }
  db.exec('COMMIT');

  return {dbFile, db};
}

function createTSPipeline(dbFile: DbFile) {
  const lc = createSilentLogContext();
  const storage = new Database(lc, ':memory:');
  storage.prepare(CREATE_STORAGE_TABLE).run();

  const pipelines = new PipelineDriver(
    lc,
    testLogConfig,
    new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
    shardID,
    new DatabaseStorage(storage).createClientGroupStorage('bench-cg'),
    'bench',
    new InspectorDelegate(undefined),
    () => 200,
  );

  return pipelines;
}

// ---------- Rust NAPI: build HydrateQuery matching ast_to_config.rs ----------

const ALL_PRIMARY_KEYS: Record<string, string[]> = {
  issues: ['id'],
  comments: ['id'],
  issueLabels: ['issueID', 'labelID'],
  labels: ['id'],
};

const COLUMN_TYPES: Record<string, Record<string, string>> = {
  issues: {id: 'string', closed: 'boolean', _0_version: 'string'},
  comments: {
    id: 'string',
    issueID: 'string',
    upvotes: 'number',
    _0_version: 'string',
  },
  issueLabels: {
    issueID: 'string',
    labelID: 'string',
    legacyID: 'string',
    _0_version: 'string',
  },
  labels: {id: 'string', name: 'string', _0_version: 'string'},
};

function buildHydrateQuery(queryId: string, ast: AST): unknown {
  return {
    query_id: queryId,
    ast,
    primary_key: ALL_PRIMARY_KEYS[ast.table] ?? ['id'],
    column_types: COLUMN_TYPES,
    all_primary_keys: ALL_PRIMARY_KEYS,
  };
}

/** Build multiple unique queries for multi-pipeline parallelism benchmarks. */
function buildMultiQueries(
  count: number,
  ast: AST,
): {queries: unknown[]; queriesJson: string} {
  const queries = [];
  for (let i = 0; i < count; i++) {
    queries.push(buildHydrateQuery(`q${i}`, ast));
  }
  return {queries, queriesJson: JSON.stringify(queries)};
}

/**
 * Build a Change object matching diff.rs Change struct:
 *   { table, prevValues: Row[], nextValue: Row | null, rowKey: {col: val} }
 */
function makeInsertChange(
  tableName: string,
  rowKey: Record<string, unknown>,
  nextValue: Record<string, unknown>,
): unknown {
  return {
    table: tableName,
    prevValues: [],
    nextValue: {...nextValue, _0_version: '999'},
    rowKey,
  };
}

// ========== HYDRATION BENCHMARKS ==========

describe('hydrate: simple issues (3 rows)', () => {
  const {dbFile} = setupDB('h_simple');

  bench('TypeScript', () => {
    const p = createTSPipeline(dbFile);
    p.init(clientSchema);
    const r = [...p.addQuery('h1', 'q1', SIMPLE_ISSUES, NO_TIMER)];
    if (r.length === 0) throw new Error('no results');
  });

  if (RustPipelineClass) {
    const RPC = RustPipelineClass;
    bench('Rust (RustPipeline)', () => {
      const cfg = buildHydrateQuery('q1', SIMPLE_ISSUES);
      const rp = new RPC(dbFile.path, JSON.stringify([cfg]));
      const buf = rp.hydrate();
      const d = decodeAdvanceResultBuf(buf);
      if (d.changes.length === 0) throw new Error('no results');
    });
  }
});

describe('hydrate: issues + comments join (7 rows)', () => {
  const {dbFile} = setupDB('h_join');

  bench('TypeScript', () => {
    const p = createTSPipeline(dbFile);
    p.init(clientSchema);
    const r = [...p.addQuery('h1', 'q1', ISSUES_AND_COMMENTS, NO_TIMER)];
    if (r.length === 0) throw new Error('no results');
  });

  if (RustPipelineClass) {
    const RPC = RustPipelineClass;
    bench('Rust (RustPipeline)', () => {
      const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
      const rp = new RPC(dbFile.path, JSON.stringify([cfg]));
      const buf = rp.hydrate();
      const d = decodeAdvanceResultBuf(buf);
      if (d.changes.length === 0) throw new Error('no results');
    });
  }
});

describe('hydrate: issues with EXISTS filter', () => {
  const {dbFile} = setupDB('h_exists');

  bench('TypeScript', () => {
    const p = createTSPipeline(dbFile);
    p.init(clientSchema);
    const r = [...p.addQuery('h1', 'q1', ISSUES_WITH_EXISTS, NO_TIMER)];
    if (r.length === 0) throw new Error('no results');
  });

  if (RustPipelineClass) {
    const RPC = RustPipelineClass;
    bench('Rust (RustPipeline)', () => {
      const cfg = buildHydrateQuery('q1', ISSUES_WITH_EXISTS);
      const rp = new RPC(dbFile.path, JSON.stringify([cfg]));
      const buf = rp.hydrate();
      const d = decodeAdvanceResultBuf(buf);
      if (d.changes.length === 0) throw new Error('no results');
    });
  }
});

describe('hydrate: bulk 1000 rows (issues + comments)', () => {
  const {dbFile} = makeBulkDB('h_bulk', 1000);

  bench('TypeScript', () => {
    const p = createTSPipeline(dbFile);
    p.init(clientSchema);
    const r = [...p.addQuery('h1', 'q1', ISSUES_AND_COMMENTS, NO_TIMER)];
    if (r.length < 100) throw new Error('too few results');
  });

  if (RustPipelineClass) {
    const RPC = RustPipelineClass;
    bench('Rust (RustPipeline)', () => {
      const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
      const rp = new RPC(dbFile.path, JSON.stringify([cfg]));
      const buf = rp.hydrate();
      const d = decodeAdvanceResultBuf(buf);
      if (d.changes.length < 100) throw new Error('too few results');
    });
  }
});

// ========== MULTI-PIPELINE HYDRATION (Rayon parallelism) ==========

for (const queryCount of [10, 50]) {
  describe(`hydrate: ${queryCount} pipelines (Rayon par_iter)`, () => {
    const {dbFile} = makeBulkDB(`h_multi_${queryCount}`, 500);

    bench('TypeScript', () => {
      const p = createTSPipeline(dbFile);
      p.init(clientSchema);
      for (let i = 0; i < queryCount; i++) {
        const r = [
          ...p.addQuery(`h${i}`, `q${i}`, ISSUES_AND_COMMENTS, NO_TIMER),
        ];
        if (r.length === 0) throw new Error('no results');
      }
    });

    if (RustPipelineClass) {
      const RPC = RustPipelineClass;
      bench('Rust (RustPipeline)', () => {
        const {queriesJson} = buildMultiQueries(
          queryCount,
          ISSUES_AND_COMMENTS,
        );
        const rp = new RPC(dbFile.path, queriesJson);
        const buf = rp.hydrate();
        const d = decodeAdvanceResultBuf(buf);
        if (d.changes.length === 0) throw new Error('no results');
      });
    }
  });
}

// ========== ADVANCE BENCHMARKS ==========

describe('advance: single insert (small DB)', () => {
  const {dbFile, db} = setupDB('a_small');
  const lc = createSilentLogContext();

  // --- TS: pre-hydrate, then benchmark advance ---
  const tsStorage = new Database(lc, ':memory:');
  tsStorage.prepare(CREATE_STORAGE_TABLE).run();
  const tsPipelines = new PipelineDriver(
    lc,
    testLogConfig,
    new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
    shardID,
    new DatabaseStorage(tsStorage).createClientGroupStorage('bench-adv-cg'),
    'bench-advance',
    new InspectorDelegate(undefined),
    () => 200,
  );
  tsPipelines.init(clientSchema);
  [...tsPipelines.addQuery('h1', 'q1', ISSUES_AND_COMMENTS, NO_TIMER)];
  const tsReplicator = fakeReplicator(lc, db);
  let tsVersion = 200;

  bench('TypeScript', () => {
    const v = ver(tsVersion++);
    tsReplicator.processTransaction(
      v,
      messages.insert('issues', {id: `adv-${v}`}),
    );
    const result = [...tsPipelines.advance(NO_TIMER).changes].filter(
      c => c !== 'yield',
    );
    void result;
  });

  // --- Rust: pre-hydrate via RustPipeline, then benchmark advance ---
  if (RustPipelineClass) {
    const RPC = RustPipelineClass;
    const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
    const rustPipeline = new RPC(dbFile.path, JSON.stringify([cfg]));
    rustPipeline.hydrate();
    let rustVersion = 10000;

    bench('Rust (RustPipeline)', () => {
      const v = ver(rustVersion++);
      const id = `radv-${v}`;
      // Insert into DB so snapshot has it
      tsReplicator.processTransaction(v, messages.insert('issues', {id}));
      rustPipeline.swapSnapshot(dbFile.path);
      const changesJson = JSON.stringify([
        makeInsertChange('issues', {id}, {id, closed: false}),
      ]);
      const buf = rustPipeline.advance(changesJson);
      const d = decodeAdvanceResultBuf(buf);
      if (d.changes.length === 0) throw new Error('no advance results');
    });
  }
});

describe('advance: single insert (1000-row DB)', () => {
  const {dbFile, db} = makeBulkDB('a_bulk', 1000);
  const lc = createSilentLogContext();

  // --- TS ---
  const tsStorage = new Database(lc, ':memory:');
  tsStorage.prepare(CREATE_STORAGE_TABLE).run();
  const tsPipelines = new PipelineDriver(
    lc,
    testLogConfig,
    new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
    shardID,
    new DatabaseStorage(tsStorage).createClientGroupStorage(
      'bench-adv-bulk-cg',
    ),
    'bench-advance-bulk',
    new InspectorDelegate(undefined),
    () => 200,
  );
  tsPipelines.init(clientSchema);
  [...tsPipelines.addQuery('h1', 'q1', ISSUES_AND_COMMENTS, NO_TIMER)];
  const tsReplicator = fakeReplicator(lc, db);
  let tsVersion = 2000;

  bench('TypeScript', () => {
    const v = ver(tsVersion++);
    tsReplicator.processTransaction(
      v,
      messages.insert('issues', {id: `bulk-${v}`}),
    );
    const result = [...tsPipelines.advance(NO_TIMER).changes].filter(
      c => c !== 'yield',
    );
    void result;
  });

  // --- Rust ---
  if (RustPipelineClass) {
    const RPC = RustPipelineClass;
    const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
    const rustPipeline = new RPC(dbFile.path, JSON.stringify([cfg]));
    rustPipeline.hydrate();
    let rustVersion = 20000;

    bench('Rust (RustPipeline)', () => {
      const v = ver(rustVersion++);
      const id = `rbulk-${v}`;
      tsReplicator.processTransaction(v, messages.insert('issues', {id}));
      rustPipeline.swapSnapshot(dbFile.path);
      const changesJson = JSON.stringify([
        makeInsertChange('issues', {id}, {id, closed: false}),
      ]);
      const buf = rustPipeline.advance(changesJson);
      const d = decodeAdvanceResultBuf(buf);
      if (d.changes.length === 0) throw new Error('no advance results');
    });
  }
});

// ========== MULTI-PIPELINE ADVANCE (Rayon parallelism) ==========

for (const queryCount of [10, 50]) {
  describe(`advance: ${queryCount} pipelines, single insert (Rayon par_iter)`, () => {
    const {dbFile, db} = makeBulkDB(`a_multi_${queryCount}`, 500);
    const lc = createSilentLogContext();

    // --- TS: add N queries, then advance ---
    const tsStorage = new Database(lc, ':memory:');
    tsStorage.prepare(CREATE_STORAGE_TABLE).run();
    const tsPipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(tsStorage).createClientGroupStorage(
        `bench-adv-multi-${queryCount}`,
      ),
      `bench-advance-multi-${queryCount}`,
      new InspectorDelegate(undefined),
      () => 200,
    );
    tsPipelines.init(clientSchema);
    for (let i = 0; i < queryCount; i++) {
      [
        ...tsPipelines.addQuery(
          `h${i}`,
          `q${i}`,
          ISSUES_AND_COMMENTS,
          NO_TIMER,
        ),
      ];
    }
    const tsReplicator = fakeReplicator(lc, db);
    let tsVersion = 30000 + queryCount * 1000;

    bench('TypeScript', () => {
      const v = ver(tsVersion++);
      tsReplicator.processTransaction(
        v,
        messages.insert('issues', {id: `multi-${v}`}),
      );
      const result = [...tsPipelines.advance(NO_TIMER).changes].filter(
        c => c !== 'yield',
      );
      void result;
    });

    // --- Rust: N pipelines via RustPipeline, then advance ---
    if (RustPipelineClass) {
      const RPC = RustPipelineClass;
      const {queriesJson} = buildMultiQueries(queryCount, ISSUES_AND_COMMENTS);
      const rustPipeline = new RPC(dbFile.path, queriesJson);
      rustPipeline.hydrate();
      let rustVersion = 50000 + queryCount * 1000;

      bench('Rust (RustPipeline)', () => {
        const v = ver(rustVersion++);
        const id = `rmulti-${v}`;
        tsReplicator.processTransaction(v, messages.insert('issues', {id}));
        rustPipeline.swapSnapshot(dbFile.path);
        const changesJson = JSON.stringify([
          makeInsertChange('issues', {id}, {id, closed: false}),
        ]);
        const buf = rustPipeline.advance(changesJson);
        const d = decodeAdvanceResultBuf(buf);
        if (d.changes.length === 0) throw new Error('no advance results');
      });
    }
  });
}
