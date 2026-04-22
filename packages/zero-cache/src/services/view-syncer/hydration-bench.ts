/**
 * Rust vs TS Hydration Benchmark
 *
 * Uses subprocess isolation so env vars are set BEFORE module-level
 * constants (USE_RUST_IVM, USE_RUST_HYDRATION) are evaluated.
 *
 * Usage:
 *   node --experimental-strip-types --experimental-transform-types \
 *     packages/zero-cache/src/services/view-syncer/hydration-bench.ts
 *
 * Or run a single mode directly (used by the subprocess):
 *   BENCH_MODE=ts node ... hydration-bench.ts
 *   BENCH_MODE=rust node ... hydration-bench.ts
 */

import {execSync} from 'child_process';

const BENCH_MODE = process.env.BENCH_MODE as 'ts' | 'rust' | undefined;

if (!BENCH_MODE) {
  // ─── Orchestrator: fork subprocesses for each mode ─────────────────────
  const ITERATIONS = 3;
  const SEED_COUNTS = [100, 500, 2000];
  const PIPELINE_COUNTS = [1, 4, 16, 32];

  const scriptPath = import.meta.url.replace('file://', '');
  const nodeArgs =
    '--experimental-strip-types --experimental-transform-types --no-warnings';

  type SubResult = {
    seedRows: number;
    pipelines: number;
    elapsedMs: number;
    rowCount: number;
  };

  function runMode(
    mode: 'ts' | 'rust',
    seedRows: number,
    pipelines: number,
  ): SubResult {
    const env = {
      ...process.env,
      BENCH_MODE: mode,
      BENCH_SEED_ROWS: String(seedRows),
      BENCH_PIPELINES: String(pipelines),
      ZERO_DISABLE_RUST_IVM: mode === 'ts' ? '1' : '0',
      ZERO_DISABLE_RUST_HYDRATION: mode === 'ts' ? '1' : '0',
    };
    const out = execSync(`node ${nodeArgs} "${scriptPath}"`, {
      env,
      encoding: 'utf-8',
      timeout: 60_000,
    });
    return JSON.parse(out.trim().split('\n').pop()!);
  }

  function median(arr: number[]): number {
    const s = [...arr].sort((a, b) => a - b);
    const mid = Math.floor(s.length / 2);
    return s.length % 2 !== 0 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
  }

  console.log('Rust vs TS Hydration Benchmark');
  console.log(`  Iterations: ${ITERATIONS}\n`);

  // Phase A: Seed row sweep (fixed pipelines = 16)
  console.log('Phase A: Data size sweep (pipelines=16)');
  console.log('  seed rows |    TS (ms) |  Rust (ms) | speedup | rows');
  console.log('  ----------|------------|------------|---------|-----');
  for (const seed of SEED_COUNTS) {
    const tsTimes: number[] = [];
    const rustTimes: number[] = [];
    let rowCount = 0;
    for (let i = 0; i < ITERATIONS; i++) {
      const ts = runMode('ts', seed, 16);
      tsTimes.push(ts.elapsedMs);
      const rs = runMode('rust', seed, 16);
      rustTimes.push(rs.elapsedMs);
      rowCount = rs.rowCount;
    }
    const tsMs = median(tsTimes);
    const rustMs = median(rustTimes);
    const speedup = tsMs / rustMs;
    console.log(
      `  ${String(seed).padStart(9)} | ${tsMs.toFixed(1).padStart(10)} | ${rustMs.toFixed(1).padStart(10)} | ${speedup.toFixed(2).padStart(7)}x | ${rowCount}`,
    );
  }

  // Phase B: Pipeline count sweep (fixed seed = 2000)
  console.log('\nPhase B: Pipeline count sweep (seed=2000)');
  console.log('  pipelines |    TS (ms) |  Rust (ms) | speedup | rows');
  console.log('  ----------|------------|------------|---------|-----');
  for (const p of PIPELINE_COUNTS) {
    const tsTimes: number[] = [];
    const rustTimes: number[] = [];
    let rowCount = 0;
    for (let i = 0; i < ITERATIONS; i++) {
      const ts = runMode('ts', 2000, p);
      tsTimes.push(ts.elapsedMs);
      const rs = runMode('rust', 2000, p);
      rustTimes.push(rs.elapsedMs);
      rowCount = rs.rowCount;
    }
    const tsMs = median(tsTimes);
    const rustMs = median(rustTimes);
    const speedup = tsMs / rustMs;
    console.log(
      `  ${String(p).padStart(9)} | ${tsMs.toFixed(1).padStart(10)} | ${rustMs.toFixed(1).padStart(10)} | ${speedup.toFixed(2).padStart(7)}x | ${rowCount}`,
    );
  }

  // Phase C: Queries with LIMIT (Take state initialization)
  console.log('\nPhase C: Queries with LIMIT (seed=2000, pipelines=16)');
  {
    const tsTimes: number[] = [];
    const rustTimes: number[] = [];
    let rowCount = 0;
    for (let i = 0; i < ITERATIONS; i++) {
      const ts = runMode('ts', 2000, 16);
      tsTimes.push(ts.elapsedMs);
      const rs = runMode('rust', 2000, 16);
      rustTimes.push(rs.elapsedMs);
      rowCount = rs.rowCount;
    }
    const tsMs = median(tsTimes);
    const rustMs = median(rustTimes);
    console.log(
      `  TS: ${tsMs.toFixed(1)}ms | Rust: ${rustMs.toFixed(1)}ms | speedup: ${(tsMs / rustMs).toFixed(2)}x | rows: ${rowCount}`,
    );
  }

  console.log('\nDone.');
} else {
  // ─── Worker: run hydration benchmark in isolated process ────────────────
  // ENV vars are set BEFORE this import evaluates module-level constants.

  const {testLogConfig: otelLogConfig} =
    await import('../../../../otel/src/test-log-config.ts');
  const {createSilentLogContext} =
    await import('../../../../shared/src/logging-test-utils.ts');
  const {createSchema} =
    await import('../../../../zero-schema/src/builder/schema-builder.ts');
  const {boolean, number, string, table} =
    await import('../../../../zero-schema/src/builder/table-builder.ts');
  const {CREATE_STORAGE_TABLE, DatabaseStorage} =
    await import('../../../../zqlite/src/database-storage.ts');
  const {Database} = await import('../../../../zqlite/src/db.ts');
  const {listTables} = await import('../../db/lite-tables.ts');
  const {InspectorDelegate} =
    await import('../../server/inspector-delegate.ts');
  const {DbFile} = await import('../../test/lite.ts');
  const {upstreamSchema} = await import('../../types/shards.ts');
  const {populateFromExistingTables} =
    await import('../replicator/schema/column-metadata.ts');
  const {initReplicationState} =
    await import('../replicator/schema/replication-state.ts');
  const {PipelineDriver} = await import('./pipeline-driver.ts');
  const {Snapshotter} = await import('./snapshotter.ts');

  const seedRows = parseInt(process.env.BENCH_SEED_ROWS || '500', 10);
  const numPipelines = parseInt(process.env.BENCH_PIPELINES || '16', 10);

  const shardID = {appID: 'zeroz', shardNum: 1};
  const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

  const issues = table('issues')
    .columns({id: string(), closed: boolean()})
    .primaryKey('id');
  const comments = table('comments')
    .columns({id: string(), issueID: string(), upvotes: number()})
    .primaryKey('id');
  const clientSchema = createSchema({tables: [issues, comments]});

  type AST = Parameters<InstanceType<typeof PipelineDriver>['addQuery']>[2];

  const QUERY_TEMPLATES: AST[] = [
    // Filter-only
    {
      table: 'issues',
      orderBy: [['id', 'asc']],
      where: {
        type: 'simple',
        left: {type: 'column', name: 'closed'},
        op: '=',
        right: {type: 'literal', value: false},
      },
    },
    // With LIMIT (Take)
    {
      table: 'issues',
      orderBy: [['id', 'asc']],
      limit: 10,
    },
    // With related (Join)
    {
      table: 'issues',
      orderBy: [['id', 'desc']],
      related: [
        {
          system: 'client' as const,
          correlation: {parentField: ['id'], childField: ['issueID']},
          subquery: {
            table: 'comments',
            alias: 'comments',
            orderBy: [['id', 'desc']],
          },
        },
      ],
    },
    // Table scan
    {
      table: 'comments',
      orderBy: [['id', 'asc']],
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

  const NO_TIME_ADVANCEMENT_TIMER = {
    elapsedLap: () => 0,
    totalElapsed: () => 0,
  };

  // Setup DB
  const lc = createSilentLogContext();
  const dbFile = new DbFile(`hydration_bench_${BENCH_MODE}`);
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
  for (let i = 0; i < seedRows; i++) {
    insertIssue.run(`issue-${i}`, i % 2 === 0 ? 0 : 1);
    insertComment.run(`comment-${i}`, `issue-${i}`, i * 100);
  }
  db.exec('COMMIT');
  populateFromExistingTables(db, listTables(db, false));

  // Create PipelineDriver
  const storage = new Database(lc, ':memory:');
  storage.prepare(CREATE_STORAGE_TABLE).run();

  const pipelines = new PipelineDriver(
    lc,
    otelLogConfig,
    new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
    shardID,
    new DatabaseStorage(storage).createClientGroupStorage(BENCH_MODE),
    `${BENCH_MODE}-bench`,
    new InspectorDelegate(undefined),
    () => 200,
  );

  pipelines.init(clientSchema);

  // Benchmark hydration
  const queries = generateQueries(numPipelines);
  let totalRows = 0;

  const start = performance.now();
  // Use addQueries() for batch Rust hydration (Rayon parallelism across all N pipelines).
  for (const change of pipelines.addQueries(
    queries.map(q => ({
      transformationHash: q.hydrationID,
      queryID: q.queryID,
      ast: q.ast,
    })),
    NO_TIME_ADVANCEMENT_TIMER,
  )) {
    if (change !== 'yield') {
      totalRows++;
    }
  }
  const elapsed = performance.now() - start;

  // Cleanup
  dbFile.delete();

  // Output result as JSON (last line parsed by orchestrator)
  console.log(
    JSON.stringify({
      seedRows,
      pipelines: numPipelines,
      elapsedMs: elapsed,
      rowCount: totalRows,
      mode: BENCH_MODE,
    }),
  );
}
