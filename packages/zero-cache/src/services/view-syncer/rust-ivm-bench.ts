import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
/**
 * Rust IVM Benchmark Suite
 *
 * Compares Rust IVM advance() vs TypeScript-only advance() across
 * varying pipeline counts and diff sizes, with a correctness gate.
 *
 * Usage:
 *   node --experimental-strip-types --experimental-transform-types packages/zero-cache/src/services/view-syncer/rust-ivm-bench.ts
 */
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
import {PipelineDriver, type RowChange, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';

// ─── Config ──────────────────────────────────────────────────────────────────
const PIPELINE_COUNTS = [1, 2, 4, 8, 16, 32, 64];
const DIFF_SIZES = [1, 10, 100, 500, 1000];
const ITERATIONS = 5;
const REALISTIC_PIPELINES = 50;
const REALISTIC_DIFF = 100;
const MIN_SPEEDUP = 4.0;
const TARGET_SPEEDUP = 6.0;

const NO_TIME_ADVANCEMENT_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// ─── Schema ──────────────────────────────────────────────────────────────────
const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

const issues = table('issues')
  .columns({id: string(), closed: boolean()})
  .primaryKey('id');
const comments = table('comments')
  .columns({id: string(), issueID: string(), upvotes: number()})
  .primaryKey('id');

const clientSchema = createSchema({tables: [issues, comments]});

// ─── Queries ─────────────────────────────────────────────────────────────────
// Filter-only queries (no orderBy, limit, or related) so Rust advance
// actually engages rust_fan_out / rust_dispatch_poke instead of falling
// back to the TS IVM tree.
const QUERY_TEMPLATES: AST[] = [
  {
    table: 'issues',
    where: {
      type: 'simple',
      left: {type: 'column', name: 'closed'},
      op: '=',
      right: {type: 'literal', value: false},
    },
  },
  {
    table: 'issues',
    where: {
      type: 'simple',
      left: {type: 'column', name: 'closed'},
      op: '=',
      right: {type: 'literal', value: true},
    },
  },
  {
    table: 'comments',
  },
  {
    table: 'comments',
    where: {
      type: 'simple',
      left: {type: 'column', name: 'upvotes'},
      op: '>',
      right: {type: 'literal', value: 500},
    },
  },
];

function generateQueries(
  count: number,
): Array<{hydrationID: string; queryID: string; ast: AST}> {
  const queries = [];
  for (let i = 0; i < count; i++) {
    queries.push({
      hydrationID: `h${i}`,
      queryID: `q${i}`,
      ast: QUERY_TEMPLATES[i % QUERY_TEMPLATES.length],
    });
  }
  return queries;
}

// ─── DB Setup ────────────────────────────────────────────────────────────────
const NUM_SEED_ROWS = 500;

function setupDb(tag: string): {dbFile: DbFile; db: DB} {
  const lc = createSilentLogContext();
  const dbFile = new DbFile(`rust_ivm_bench_${tag}`);
  dbFile.connect(lc).pragma('journal_mode = wal2');
  const db = dbFile.connect(lc);

  initReplicationState(db, ['zero_data'], '01');
  db.exec(/*sql*/ `
    CREATE TABLE "${mutationsTableName}" (
      "clientGroupID" TEXT, "clientID" TEXT, "mutationID" INTEGER, "result" TEXT,
      _0_version TEXT NOT NULL,
      PRIMARY KEY ("clientGroupID", "clientID", "mutationID")
    );
    CREATE TABLE issues (id TEXT PRIMARY KEY, closed BOOL, _0_version TEXT NOT NULL);
    CREATE TABLE comments (id TEXT PRIMARY KEY, issueID TEXT, upvotes INTEGER, _0_version TEXT NOT NULL);
  `);

  const insertIssue = db.prepare(
    `INSERT INTO issues (id, closed, _0_version) VALUES (?, ?, '01')`,
  );
  const insertComment = db.prepare(
    `INSERT INTO comments (id, issueID, upvotes, _0_version) VALUES (?, ?, ?, '01')`,
  );

  db.exec('BEGIN');
  for (let i = 0; i < NUM_SEED_ROWS; i++) {
    insertIssue.run(`issue-${i}`, i % 2 === 0 ? 0 : 1);
    insertComment.run(`comment-${i}`, `issue-${i}`, i * 100);
  }
  db.exec('COMMIT');

  populateFromExistingTables(db, listTables(db, false));
  return {dbFile, db};
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

function runAdvance(
  dbFile: DbFile,
  db: DB,
  numPipelines: number,
  numInserts: number,
  disableRust: boolean,
): {elapsed: number; changes: RowChange[]; changeCount: number} {
  const prevEnv = process.env.ZERO_DISABLE_RUST_IVM;
  process.env.ZERO_DISABLE_RUST_IVM = disableRust ? '1' : '0';

  try {
    const lc = createSilentLogContext();
    const storage = new Database(lc, ':memory:');
    storage.prepare(CREATE_STORAGE_TABLE).run();

    const pipelines = new PipelineDriver(
      lc,
      testLogConfig,
      new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
      shardID,
      new DatabaseStorage(storage).createClientGroupStorage(
        disableRust ? 'ts' : 'rs',
      ),
      disableRust ? 'ts-bench' : 'rs-bench',
      new InspectorDelegate(undefined),
      () => 200,
    );

    pipelines.init(clientSchema);
    const queries = generateQueries(numPipelines);
    for (const q of queries) {
      [
        ...pipelines.addQuery(
          q.hydrationID,
          q.queryID,
          q.ast,
          NO_TIME_ADVANCEMENT_TIMER,
        ),
      ];
    }

    // Push changes
    const messages = new ReplicationMessages({
      issues: 'id',
      comments: 'id',
      [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
    });
    const replicator = fakeReplicator(lc, db);
    const insertMsgs = [];
    for (let i = 0; i < numInserts; i++) {
      insertMsgs.push(
        messages.insert('issues', {id: `new-${i}`, closed: i % 2}),
      );
      insertMsgs.push(
        messages.insert('comments', {
          id: `nc-${i}`,
          issueID: `new-${i}`,
          upvotes: BigInt(i * 10),
        }),
      );
    }
    replicator.processTransaction('02', ...insertMsgs);

    const start = performance.now();
    const result = pipelines.advance(NO_TIME_ADVANCEMENT_TIMER);
    const changes: RowChange[] = [];
    for (const c of result.changes) {
      if (c !== 'yield') {
        changes.push(c);
      }
    }
    const elapsed = performance.now() - start;

    return {elapsed, changes, changeCount: changes.length};
  } finally {
    if (prevEnv === undefined) {
      delete process.env.ZERO_DISABLE_RUST_IVM;
    } else {
      process.env.ZERO_DISABLE_RUST_IVM = prevEnv;
    }
  }
}

function serializeChanges(changes: RowChange[]): string {
  const sorted = [...changes].sort((a, b) => {
    const cmp =
      a.queryID.localeCompare(b.queryID) ||
      a.table.localeCompare(b.table) ||
      JSON.stringify(a.rowKey).localeCompare(JSON.stringify(b.rowKey)) ||
      a.type - b.type;
    return cmp;
  });
  return JSON.stringify(sorted, Object.keys(sorted[0] ?? {}).sort(), 2);
}

function median(arr: number[]): number {
  const s = [...arr].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 !== 0 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

// ─── Benchmark Phases ────────────────────────────────────────────────────────

type PhaseResult = {
  label: string;
  pipelines: number;
  diffSize: number;
  tsMs: number;
  rustMs: number;
  speedup: number;
  correctnessPass: boolean;
};

function runSweep(
  label: string,
  pipelines: number,
  diffSize: number,
): PhaseResult {
  const tsTimes: number[] = [];
  const rustTimes: number[] = [];
  let correctnessPass = true;

  for (let i = 0; i < ITERATIONS; i++) {
    const tag = `${label}_${pipelines}_${diffSize}_${i}`;
    const {dbFile: dbTs, db: tsDb} = setupDb(`ts_${tag}`);
    const ts = runAdvance(dbTs, tsDb, pipelines, diffSize, true);
    tsTimes.push(ts.elapsed);
    dbTs.delete();

    const {dbFile: dbRs, db: rsDb} = setupDb(`rs_${tag}`);
    const rs = runAdvance(dbRs, rsDb, pipelines, diffSize, false);
    rustTimes.push(rs.elapsed);
    dbRs.delete();

    if (serializeChanges(ts.changes) !== serializeChanges(rs.changes)) {
      console.error(
        `  CORRECTNESS MISMATCH at ${label} p=${pipelines} d=${diffSize} iter=${i}`,
      );
      correctnessPass = false;
    }
  }

  const tsMs = median(tsTimes);
  const rustMs = median(rustTimes);
  const speedup = tsMs / rustMs;

  return {label, pipelines, diffSize, tsMs, rustMs, speedup, correctnessPass};
}

// ─── Main ────────────────────────────────────────────────────────────────────
function main() {
  console.log('\nRust IVM Benchmark Suite');
  console.log(`  Seed rows: ${NUM_SEED_ROWS}, Iterations: ${ITERATIONS}\n`);

  const allResults: PhaseResult[] = [];

  // Phase A: Pipeline count sweep (fixed diff = 100)
  console.log('Phase A: Pipeline count sweep (diff=100)');
  for (const p of PIPELINE_COUNTS) {
    const r = runSweep('A', p, 100);
    allResults.push(r);
    console.log(
      `  pipelines=${String(p).padStart(3)} | TS ${r.tsMs.toFixed(1)}ms | Rust ${r.rustMs.toFixed(1)}ms | ${r.speedup.toFixed(2)}x | ${r.correctnessPass ? 'OK' : 'MISMATCH'}`,
    );
  }

  // Phase B: Diff size sweep (fixed pipelines = 50)
  console.log('\nPhase B: Diff size sweep (pipelines=50)');
  for (const d of DIFF_SIZES) {
    const r = runSweep('B', 50, d);
    allResults.push(r);
    console.log(
      `  diff=${String(d).padStart(5)} | TS ${r.tsMs.toFixed(1)}ms | Rust ${r.rustMs.toFixed(1)}ms | ${r.speedup.toFixed(2)}x | ${r.correctnessPass ? 'OK' : 'MISMATCH'}`,
    );
  }

  // Phase C: Realistic workload
  console.log('\nPhase C: Realistic workload (50 pipelines, 100 inserts)');
  const realistic = runSweep('C', REALISTIC_PIPELINES, REALISTIC_DIFF);
  allResults.push(realistic);

  const passMin = realistic.speedup >= MIN_SPEEDUP;
  const passTarget = realistic.speedup >= TARGET_SPEEDUP;
  const verdict = !realistic.correctnessPass
    ? 'FAIL (correctness)'
    : passTarget
      ? 'PASS (target)'
      : passMin
        ? 'PASS (minimum)'
        : 'FAIL (too slow)';

  console.log(
    `  TS ${realistic.tsMs.toFixed(1)}ms | Rust ${realistic.rustMs.toFixed(1)}ms | ${realistic.speedup.toFixed(2)}x | ${verdict}`,
  );
  console.log(`  Min speedup: ${MIN_SPEEDUP}x | Target: ${TARGET_SPEEDUP}x`);

  // Summary
  const summary = {
    phaseA: allResults.filter(r => r.label === 'A'),
    phaseB: allResults.filter(r => r.label === 'B'),
    phaseC: realistic,
    verdict,
    correctnessPass: allResults.every(r => r.correctnessPass),
  };

  console.log('\n--- BENCHMARK RESULTS ---');
  console.log(JSON.stringify(summary, null, 2));
}

main();
