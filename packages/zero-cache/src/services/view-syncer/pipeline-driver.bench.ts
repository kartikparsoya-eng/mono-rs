/**
 * Benchmark: Rust IVM (RustPipelineManager — production NAPI surface)
 *            vs TypeScript IVM (PipelineDriver with TS operator trees)
 *
 * Strategy:
 * - ZERO_DISABLE_RUST_IVM=1 is set in vitest.config.bench.ts so PipelineDriver
 *   uses pure TypeScript operators for hydrate AND advance.
 * - RustPipelineManager NAPI class is used directly for Rust benchmarks —
 *   the same class production view-syncer uses via PipelineDriver. Bench
 *   numbers therefore reflect production behaviour (sequential operator
 *   chain via build_operator_chain), not the deprecated RustPipeline path.
 *
 * The Change format for RustPipelineManager.advance() matches diff.rs:
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

// Load Rust NAPI bindings directly (bypassing PipelineDriver's env check).
// Mirrors production pipeline-driver.ts:637-639 which does:
//   manager = new RustPipelineManagerClass();
//   manager.createInstance(instanceId, dbPath);
// Per index.d.ts:223 createInstance is an INSTANCE method (id first, dbPath
// second) — verified against the generated .d.ts before coding.
const esmRequire = createRequire(import.meta.url);
let RustPipelineManagerClass:
  | (new () => {
      createInstance(id: string, dbPath: string): void;
      removeInstance(id: string): void;
      addQuery(id: string, queryJson: string): void;
      removeQuery(id: string, queryId: string): void;
      hydrate(id: string): Buffer;
      advance(id: string, changesJson: string): Buffer;
      swapSnapshot(id: string, newDbPath: string): void;
      setPrevSnapshot(id: string, prevDbPath: string): void;
      setPermissionTables(id: string, tablesJson: string): void;
      pipelineCount(id: string): number;
    })
  | undefined;

try {
  const bindings = esmRequire('zqlite-rs');
  RustPipelineManagerClass = bindings?.RustPipelineManager;
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

  if (RustPipelineManagerClass) {
    const Mgr = RustPipelineManagerClass;
    let instanceCounter = 0;
    bench('Rust (Manager)', () => {
      const instanceId = `bench-h-simple-${Date.now()}-${instanceCounter++}`;
      const mgr = new Mgr();
      mgr.createInstance(instanceId, dbFile.path);
      const cfg = buildHydrateQuery('q1', SIMPLE_ISSUES);
      mgr.addQuery(instanceId, JSON.stringify(cfg));
      const buf = mgr.hydrate(instanceId);
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

  if (RustPipelineManagerClass) {
    const Mgr = RustPipelineManagerClass;
    let instanceCounter = 0;
    bench('Rust (Manager)', () => {
      const instanceId = `bench-h-join-${Date.now()}-${instanceCounter++}`;
      const mgr = new Mgr();
      mgr.createInstance(instanceId, dbFile.path);
      const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
      mgr.addQuery(instanceId, JSON.stringify(cfg));
      const buf = mgr.hydrate(instanceId);
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

  if (RustPipelineManagerClass) {
    const Mgr = RustPipelineManagerClass;
    let instanceCounter = 0;
    bench('Rust (Manager)', () => {
      const instanceId = `bench-h-exists-${Date.now()}-${instanceCounter++}`;
      const mgr = new Mgr();
      mgr.createInstance(instanceId, dbFile.path);
      const cfg = buildHydrateQuery('q1', ISSUES_WITH_EXISTS);
      mgr.addQuery(instanceId, JSON.stringify(cfg));
      const buf = mgr.hydrate(instanceId);
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

  if (RustPipelineManagerClass) {
    const Mgr = RustPipelineManagerClass;
    let instanceCounter = 0;
    bench('Rust (Manager)', () => {
      const instanceId = `bench-h-bulk-${Date.now()}-${instanceCounter++}`;
      const mgr = new Mgr();
      mgr.createInstance(instanceId, dbFile.path);
      const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
      mgr.addQuery(instanceId, JSON.stringify(cfg));
      const buf = mgr.hydrate(instanceId);
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

    if (RustPipelineManagerClass) {
      const Mgr = RustPipelineManagerClass;
      let instanceCounter = 0;
      bench('Rust (Manager)', () => {
        const instanceId = `bench-h-multi-${queryCount}-${Date.now()}-${instanceCounter++}`;
        const mgr = new Mgr();
        mgr.createInstance(instanceId, dbFile.path);
        // Production multi-pipeline shape: one Manager instance, N
        // addQuery() calls — same as PipelineDriver.addQueriesAsync's
        // Phase 2 loop (pipeline-driver.ts:1451-1456 in production).
        const {queries} = buildMultiQueries(queryCount, ISSUES_AND_COMMENTS);
        for (const q of queries) {
          mgr.addQuery(instanceId, JSON.stringify(q));
        }
        const buf = mgr.hydrate(instanceId);
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

  // --- Rust: pre-hydrate via RustPipelineManager, then benchmark advance ---
  // Simplification: bench has only one DB file, so prev and curr snapshots
  // share the same path. Production view-syncer.ts:2317-2318 calls
  // setPrevSnapshot(prev.db.db.name) + swapSnapshot(curr.db.db.name) with
  // distinct paths via the snapshotter; the bench uses one path because
  // we're driving Rust directly without the snapshotter. Effect on numbers
  // is conservative (skips an extra connection-pool open).
  if (RustPipelineManagerClass) {
    const Mgr = RustPipelineManagerClass;
    const instanceId = `bench-adv-small-${Date.now()}`;
    const mgr = new Mgr();
    mgr.createInstance(instanceId, dbFile.path);
    const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
    mgr.addQuery(instanceId, JSON.stringify(cfg));
    mgr.hydrate(instanceId);
    let rustVersion = 10000;

    bench('Rust (Manager)', () => {
      const v = ver(rustVersion++);
      const id = `radv-${v}`;
      // Insert into DB so snapshot has it
      tsReplicator.processTransaction(v, messages.insert('issues', {id}));
      mgr.setPrevSnapshot(instanceId, dbFile.path);
      mgr.swapSnapshot(instanceId, dbFile.path);
      const changesJson = JSON.stringify([
        makeInsertChange('issues', {id}, {id, closed: false}),
      ]);
      const buf = mgr.advance(instanceId, changesJson);
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
  if (RustPipelineManagerClass) {
    const Mgr = RustPipelineManagerClass;
    const instanceId = `bench-adv-bulk-${Date.now()}`;
    const mgr = new Mgr();
    mgr.createInstance(instanceId, dbFile.path);
    const cfg = buildHydrateQuery('q1', ISSUES_AND_COMMENTS);
    mgr.addQuery(instanceId, JSON.stringify(cfg));
    mgr.hydrate(instanceId);
    let rustVersion = 20000;

    bench('Rust (Manager)', () => {
      const v = ver(rustVersion++);
      const id = `rbulk-${v}`;
      tsReplicator.processTransaction(v, messages.insert('issues', {id}));
      mgr.setPrevSnapshot(instanceId, dbFile.path);
      mgr.swapSnapshot(instanceId, dbFile.path);
      const changesJson = JSON.stringify([
        makeInsertChange('issues', {id}, {id, closed: false}),
      ]);
      const buf = mgr.advance(instanceId, changesJson);
      const d = decodeAdvanceResultBuf(buf);
      if (d.changes.length === 0) throw new Error('no advance results');
    });
  }
});

// ========== MULTI-PIPELINE ADVANCE (Rayon parallelism) ==========

// ========== PROFILING: Where is time spent in Rust advance? ==========

if (RustPipelineManagerClass) {
  for (const queryCount of [1, 10, 50]) {
    describe(`PROFILE advance breakdown: ${queryCount} pipelines`, () => {
      const {dbFile, db} = makeBulkDB(`prof_${queryCount}`, 500);
      const lc = createSilentLogContext();
      const tsReplicator = fakeReplicator(lc, db);

      const Mgr = RustPipelineManagerClass!;
      const instanceId = `bench-prof-${queryCount}-${Date.now()}`;
      const mgr = new Mgr();
      mgr.createInstance(instanceId, dbFile.path);
      const {queries} = buildMultiQueries(queryCount, ISSUES_AND_COMMENTS);
      for (const q of queries) {
        mgr.addQuery(instanceId, JSON.stringify(q));
      }
      mgr.hydrate(instanceId);
      let profVersion = 80000 + queryCount * 1000;

      // Accumulators for timing (in ms)
      const timings = {
        processTransaction: 0,
        swapSnapshot: 0,
        jsonStringify: 0,
        rustAdvance: 0,
        decodeBuf: 0,
        iterations: 0,
      };

      bench(
        `${queryCount} pipelines (profiled)`,
        () => {
          const v = ver(profVersion++);
          const id = `prof-${v}`;

          let t0 = performance.now();
          tsReplicator.processTransaction(v, messages.insert('issues', {id}));
          let t1 = performance.now();
          timings.processTransaction += t1 - t0;

          t0 = performance.now();
          // Mirror production: setPrevSnapshot before swapSnapshot per
          // pipeline-driver.ts:2317-2318.
          mgr.setPrevSnapshot(instanceId, dbFile.path);
          mgr.swapSnapshot(instanceId, dbFile.path);
          t1 = performance.now();
          timings.swapSnapshot += t1 - t0;

          t0 = performance.now();
          const changesJson = JSON.stringify([
            makeInsertChange('issues', {id}, {id, closed: false}),
          ]);
          t1 = performance.now();
          timings.jsonStringify += t1 - t0;

          t0 = performance.now();
          const buf = mgr.advance(instanceId, changesJson);
          t1 = performance.now();
          timings.rustAdvance += t1 - t0;

          t0 = performance.now();
          const d = decodeAdvanceResultBuf(buf);
          t1 = performance.now();
          timings.decodeBuf += t1 - t0;

          timings.iterations++;
          if (d.changes.length === 0) throw new Error('no advance results');
        },
        {
          teardown: () => {
            if (timings.iterations > 0) {
              const n = timings.iterations;
              console.log(
                `\n--- PROFILE: ${queryCount} pipelines (${n} iterations) ---`,
              );
              console.log(
                `  processTransaction: ${(timings.processTransaction / n).toFixed(3)} ms/iter`,
              );
              console.log(
                `  swapSnapshot:       ${(timings.swapSnapshot / n).toFixed(3)} ms/iter`,
              );
              console.log(
                `  JSON.stringify:     ${(timings.jsonStringify / n).toFixed(3)} ms/iter`,
              );
              console.log(
                `  rustAdvance (NAPI): ${(timings.rustAdvance / n).toFixed(3)} ms/iter`,
              );
              console.log(
                `  decodeBuf:          ${(timings.decodeBuf / n).toFixed(3)} ms/iter`,
              );
              console.log(
                `  TOTAL:              ${((timings.processTransaction + timings.swapSnapshot + timings.jsonStringify + timings.rustAdvance + timings.decodeBuf) / n).toFixed(3)} ms/iter`,
              );
              // Reset for next warmup/measurement cycle
              timings.processTransaction = 0;
              timings.swapSnapshot = 0;
              timings.jsonStringify = 0;
              timings.rustAdvance = 0;
              timings.decodeBuf = 0;
              timings.iterations = 0;
            }
          },
        },
      );
    });
  }
}

// ========== MULTI-PIPELINE ADVANCE (Rayon parallelism) ==========

for (const queryCount of [2, 10, 50]) {
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

    // --- Rust: N pipelines via RustPipelineManager, then advance ---
    if (RustPipelineManagerClass) {
      const Mgr = RustPipelineManagerClass;
      const instanceId = `bench-adv-multi-${queryCount}-${Date.now()}`;
      const mgr = new Mgr();
      mgr.createInstance(instanceId, dbFile.path);
      const {queries} = buildMultiQueries(queryCount, ISSUES_AND_COMMENTS);
      for (const q of queries) {
        mgr.addQuery(instanceId, JSON.stringify(q));
      }
      mgr.hydrate(instanceId);
      let rustVersion = 50000 + queryCount * 1000;

      bench('Rust (Manager)', () => {
        const v = ver(rustVersion++);
        const id = `rmulti-${v}`;
        tsReplicator.processTransaction(v, messages.insert('issues', {id}));
        mgr.setPrevSnapshot(instanceId, dbFile.path);
        mgr.swapSnapshot(instanceId, dbFile.path);
        const changesJson = JSON.stringify([
          makeInsertChange('issues', {id}, {id, closed: false}),
        ]);
        const buf = mgr.advance(instanceId, changesJson);
        const d = decodeAdvanceResultBuf(buf);
        if (d.changes.length === 0) throw new Error('no advance results');
      });
    }
  });
}
