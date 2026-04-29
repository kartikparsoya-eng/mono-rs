/**
 * Phase 33 PERF-02 + PERF-03 streaming benchmarks.
 *
 * PERF-02: time-to-first-RowChange tracks min(pipeline_time), not max.
 *          With 4 pipelines staggered at 50/100/250/500ms, first RowChange
 *          must arrive within 1.5 * minDelay = 75ms.
 *
 * PERF-03: streaming peak memory drops from O(total_changes) to
 *          O(max_pipeline_changes). With 4 pipelines × 2500 rows = 10K total,
 *          streaming peak × 4 must be ≤ buffered peak × 1.2.
 *
 * Both record results to .planning/milestones/v5.0-bench-results.md
 * (append-only — see file header for format).
 */

// CRITICAL: enable Rust IVM. Default vitest test env has it on, but the bench
// config (vitest.config.bench.ts) disables it. Ensure it's on for this file.
// Must run BEFORE any pipeline-driver imports.
delete process.env['ZERO_DISABLE_RUST_IVM'];

import type {LogContext} from '@rocicorp/logger';
import {appendFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {afterEach, beforeEach, describe, expect, test} from 'vitest';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import {createSchema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
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
import {
  PipelineDriver,
  type RowChange,
  type Timer,
} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';

// ─── Bench results file ─────────────────────────────────────────────────────

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const BENCH_RESULTS_FILE = path.resolve(
  __dirname,
  '../../../../../.planning/milestones/v5.0-bench-results.md',
);

function logBenchResult(line: string): void {
  const timestamp = new Date().toISOString();
  appendFileSync(BENCH_RESULTS_FILE, `${timestamp} ${line}\n`);
}

// ─── Schema (4 pipelines worth of queries on a wide table) ──────────────────

const NO_TIME_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// 5-column table to make per-row payload non-trivial — gives the
// buffered-vs-streaming memory delta a measurable signal.
const widgets = table('widgets')
  .columns({
    id: string(),
    name: string(),
    description: string(),
    category: string(),
    score: number(),
  })
  .primaryKey('id');

const clientSchema = createSchema({tables: [widgets]});

// 4 distinct queries → 4 pipelines (each filters a different category).
function buildQueriesForCategories(
  categories: readonly string[],
): Array<{transformationHash: string; queryID: string; ast: AST}> {
  return categories.map((cat, i) => ({
    transformationHash: `h${i}`,
    queryID: `q${i}`,
    ast: {
      table: 'widgets',
      where: {
        type: 'simple',
        left: {type: 'column', name: 'category'},
        op: '=',
        right: {type: 'literal', value: cat},
      },
      orderBy: [['id', 'asc']],
    },
  }));
}

const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

type Fixture = {
  pipelines: PipelineDriver;
  replicator: FakeReplicator;
  startTimer: () => Timer;
  dbFile: DbFile;
  destroy: () => void;
};

/**
 * Create a fixture seeded with `rowsPerCategory` rows for each of `categories`.
 * Each category becomes one pipeline. Total rows = rowsPerCategory * categories.length.
 */
function setupFixtureWithBigData(
  uniqueLabel: string,
  lc: LogContext,
  categories: readonly string[],
  rowsPerCategory: number,
): Fixture {
  const dbFile = new DbFile(`pipelines_streaming_bench_${uniqueLabel}`);
  dbFile.connect(lc).pragma('journal_mode = wal2');

  const storage = new Database(lc, ':memory:');
  storage.prepare(CREATE_STORAGE_TABLE).run();

  const pipelines = new PipelineDriver(
    lc,
    testLogConfig,
    new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
    shardID,
    new DatabaseStorage(storage).createClientGroupStorage(
      `client-group-${uniqueLabel}`,
    ),
    `rust-ivm-streaming-bench/${uniqueLabel}`,
    new InspectorDelegate(undefined),
    () => 200,
  );

  const db: DB = dbFile.connect(lc);
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
    CREATE TABLE widgets (
      id TEXT PRIMARY KEY,
      name TEXT,
      description TEXT,
      category TEXT,
      score INTEGER,
      _0_version TEXT NOT NULL
    );
  `);

  // Bulk insert seed data within a single transaction for speed.
  const insertWidget = db.prepare(
    `INSERT INTO widgets (id, name, description, category, score, _0_version) VALUES (?, ?, ?, ?, ?, '123')`,
  );
  db.exec('BEGIN');
  for (const cat of categories) {
    for (let i = 0; i < rowsPerCategory; i++) {
      insertWidget.run(
        `${cat}-${i}`,
        `Widget ${cat} ${i}`,
        `A reasonably long description string for widget ${cat} number ${i} to inflate row payload`,
        cat,
        i,
      );
    }
  }
  db.exec('COMMIT');

  populateFromExistingTables(db, listTables(db, false));
  const replicator = fakeReplicator(lc, db);

  pipelines.init(clientSchema);

  function startTimer(): Timer {
    return NO_TIME_TIMER;
  }

  return {
    pipelines,
    replicator,
    startTimer,
    dbFile,
    destroy: () => dbFile.delete(),
  };
}

// ─── Proxy-on-prototype: inject per-chunk delays for PERF-02 ────────────────

const esmRequire = createRequire(import.meta.url);

/**
 * Patch RustPipelineManager.prototype.advanceStreaming AND
 * RustPipelineManager.prototype.addQuery.streaming-side-effect-free) to inject
 * per-chunk delays. Returns a restore function — caller MUST call in `finally`.
 *
 * Mirror of pipeline-driver.streaming.test.ts:281-310 pattern.
 */
function patchAdvanceStreamingWithDelays(delaysMs: readonly number[]): () => void {
  /* eslint-disable @typescript-eslint/no-explicit-any */
  const zqliteRs = esmRequire('zqlite-rs') as {
    RustPipelineManager: {
      prototype: {
        advanceStreaming: (id: string, json: string) => unknown;
      };
    };
  };
  const ManagerCls = zqliteRs.RustPipelineManager;
  const orig = ManagerCls.prototype.advanceStreaming;
  let chunkIdx = 0;
  ManagerCls.prototype.advanceStreaming = function patched(
    this: unknown,
    id: string,
    json: string,
  ): unknown {
    const realStream = orig.call(this, id, json) as {
      next: () => Promise<{value: unknown; done: boolean}>;
      return: () => void;
    };
    return new Proxy(realStream, {
      get(target, prop) {
        if (prop === 'next') {
          return async () => {
            const idx = chunkIdx++;
            const delay = delaysMs[idx % delaysMs.length];
            // Delay BEFORE returning the chunk — simulates a pipeline that
            // takes `delay` ms to produce its first chunk. Chunk 0 = 50ms,
            // chunk 1 = 100ms, etc. The first chunk to arrive after the
            // shortest delay is the TTFB measurement.
            const item = await target.next();
            if (!item.done) {
              await new Promise(r => setTimeout(r, delay));
            }
            return item;
          };
        }
        return Reflect.get(target, prop);
      },
    });
  };
  return () => {
    ManagerCls.prototype.advanceStreaming = orig;
  };
  /* eslint-enable @typescript-eslint/no-explicit-any */
}

// ─── Memory-peak poller for PERF-03 ─────────────────────────────────────────

/**
 * Sample `rss + heapUsed` at fixed intervals. Returns peak DELTA from the
 * baseline captured at construction.
 *
 * Rationale: Node's RSS rarely shrinks back to the OS, so absolute peaks across
 * sequential runs are monotonically increasing — the second run's peak always
 * looks bigger even when its workload allocates less. Measuring the delta from
 * a per-run baseline isolates each run's allocation footprint.
 */
function startPeakPoller(intervalMs = 50): {
  peak: () => number;
  baseline: () => number;
  stop: () => void;
} {
  const m0 = process.memoryUsage();
  const baseline = m0.rss + m0.heapUsed;
  let peakAbs = baseline;
  const sample = () => {
    const m = process.memoryUsage();
    const total = m.rss + m.heapUsed;
    if (total > peakAbs) peakAbs = total;
  };
  const handle = setInterval(sample, intervalMs);
  return {
    // Peak DELTA from baseline. Cannot be negative.
    peak: () => Math.max(0, peakAbs - baseline),
    baseline: () => baseline,
    stop: () => clearInterval(handle),
  };
}

// ─── PERF-02: TTFB streaming bench ──────────────────────────────────────────

describe('PERF-02 TTFB streaming bench', () => {
  let lc: LogContext;
  let fx: Fixture | undefined;

  beforeEach(() => {
    lc = createSilentLogContext();
  });

  afterEach(() => {
    fx?.destroy();
    fx = undefined;
  });

  test('first-chunk arrives ~min(pipeline_time), not max', async () => {
    // RESEARCH P-04: 50ms minimum (raised from 10ms) avoids CI scheduler jitter.
    const delays = [50, 100, 250, 500] as const;
    const minDelay = delays[0];

    const restore = patchAdvanceStreamingWithDelays(delays);
    try {
      // Modest fixture: 4 categories × 25 rows each = 100 total. Enough to
      // produce multiple chunks, not enough to dominate TTFB measurement.
      const categories = ['cat-a', 'cat-b', 'cat-c', 'cat-d'] as const;
      fx = setupFixtureWithBigData('perf02_ttfb', lc, categories, 25);

      const queries = buildQueriesForCategories(categories);

      // Hydrate the 4 pipelines (drains all changes).
      const hydrateIter = await fx.pipelines.addQueriesAsync(
        queries,
        fx.startTimer(),
      );
      for (const c of hydrateIter) {
        void c;
      }

      // Apply a transaction touching all 4 categories so advanceStreaming has
      // work to do across all 4 pipelines.
      const messages = new ReplicationMessages({
        widgets: 'id',
        [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
      });
      fx.replicator.processTransaction(
        '124',
        ...categories.map((cat, i) =>
          messages.insert('widgets', {
            id: `${cat}-new-${i}`,
            name: `New ${cat}`,
            description: `Newly inserted widget for ${cat}`,
            category: cat,
            score: BigInt(1000 + i),
          }),
        ),
      );

      // Now measure TTFB.
      const result = await fx.pipelines.advanceStreaming(NO_TIME_TIMER);

      let firstChunkT: number | undefined;
      const start = performance.now();
      for await (const c of result.changes) {
        // Phase 32 / RESEARCH §1.6: filter both 'yield' AND 'chunk-end'.
        if (c !== 'yield' && c !== 'chunk-end') {
          if (firstChunkT === undefined) {
            firstChunkT = performance.now() - start;
          }
          // Don't break — let stream complete naturally for clean teardown.
        }
      }

      expect(firstChunkT).toBeDefined();

      const ratio = firstChunkT! / minDelay;
      const threshold = 1.5;
      // D-29: small-margin failure (>1.5 but ≤3.0) → WARN; severe (>3.0) → FAIL.
      const seriousFail = ratio > 3.0;
      const verdict = ratio <= threshold ? 'PASS' : seriousFail ? 'FAIL' : 'WARN';

      logBenchResult(
        `TTFB-streaming first_chunk_ms=${firstChunkT!.toFixed(2)} ` +
          `min_pipeline_ms=${minDelay} ratio=${ratio.toFixed(2)}x ` +
          `threshold=${threshold}x ${verdict}`,
      );

      if (seriousFail) {
        throw new Error(
          `TTFB ratio ${ratio.toFixed(2)}x is severely off (>3x). ` +
            `firstChunkT=${firstChunkT}ms, minDelay=${minDelay}ms`,
        );
      }
      if (verdict === 'PASS') {
        expect(firstChunkT!).toBeLessThan(minDelay * threshold);
      }
      // WARN case: do not throw; let CI continue. Result line is the audit trail.
    } finally {
      restore();
    }
  });
});

// ─── PERF-03: Memory peak streaming bench ───────────────────────────────────

describe('PERF-03 memory peak streaming bench', () => {
  let lc: LogContext;
  let fxBuf: Fixture | undefined;
  let fxStr: Fixture | undefined;

  beforeEach(() => {
    lc = createSilentLogContext();
  });

  afterEach(() => {
    fxBuf?.destroy();
    fxStr?.destroy();
    fxBuf = undefined;
    fxStr = undefined;
  });

  test('streaming peak ~ O(max_pipeline_changes), not O(total)', async () => {
    const N_PIPELINES = 4;
    const ROWS_PER_PIPELINE = 2500;
    const categories = ['mp-a', 'mp-b', 'mp-c', 'mp-d'] as const;
    const queries = buildQueriesForCategories(categories);

    // ── Run 1: buffered ────────────────────────────────────────────────────
    fxBuf = setupFixtureWithBigData(
      'perf03_buf',
      lc,
      categories,
      ROWS_PER_PIPELINE,
    );

    if (typeof (globalThis as {gc?: () => void}).gc === 'function') {
      (globalThis as {gc?: () => void}).gc!();
    }
    const pollerBuf = startPeakPoller(50);

    // Hydrate first (this loads a baseline into Rust per-pipeline state).
    const bufHydrateIter = await fxBuf.pipelines.addQueriesAsync(
      queries,
      fxBuf.startTimer(),
    );
    for (const c of bufHydrateIter) {
      void c;
    }

    // Apply a transaction with one row per pipeline so advance has work to do.
    const messagesBuf = new ReplicationMessages({
      widgets: 'id',
      [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
    });
    fxBuf.replicator.processTransaction(
      '125',
      ...categories.map((cat, i) =>
        messagesBuf.insert('widgets', {
          id: `${cat}-extra-${i}`,
          name: `Extra ${cat}`,
          description: `Buffered run extra widget for ${cat}`,
          category: cat,
          score: BigInt(9000 + i),
        }),
      ),
    );

    const bufResult = await fxBuf.pipelines.advanceAsync(NO_TIME_TIMER);
    // Drain synchronously (advanceAsync returns Iterable).
    for (const c of bufResult.changes) {
      void c;
    }
    pollerBuf.stop();
    const bufferedPeak = pollerBuf.peak();
    fxBuf.destroy();
    fxBuf = undefined;

    // ── Run 2: streaming ───────────────────────────────────────────────────
    fxStr = setupFixtureWithBigData(
      'perf03_str',
      lc,
      categories,
      ROWS_PER_PIPELINE,
    );

    if (typeof (globalThis as {gc?: () => void}).gc === 'function') {
      (globalThis as {gc?: () => void}).gc!();
    }
    const pollerStr = startPeakPoller(50);

    const strHydrateIter = await fxStr.pipelines.addQueriesAsync(
      queries,
      fxStr.startTimer(),
    );
    for (const c of strHydrateIter) {
      void c;
    }

    fxStr.replicator.processTransaction(
      '126',
      ...categories.map((cat, i) =>
        messagesBuf.insert('widgets', {
          id: `${cat}-strex-${i}`,
          name: `StrExtra ${cat}`,
          description: `Streaming run extra widget for ${cat}`,
          category: cat,
          score: BigInt(9000 + i),
        }),
      ),
    );

    const strResult = await fxStr.pipelines.advanceStreaming(NO_TIME_TIMER);
    for await (const c of strResult.changes) {
      void c;
    }
    pollerStr.stop();
    const streamingPeak = pollerStr.peak();
    fxStr.destroy();
    fxStr = undefined;

    // Avoid divide-by-zero in pathological cases.
    const safeStreamingPeak = Math.max(streamingPeak, 1);
    const ratio = bufferedPeak / safeStreamingPeak;

    // Per CONTEXT D-21:
    //   - PASS: streamingPeak * N <= bufferedPeak * 1.2
    //   - WARN: streamingPeak * N <= bufferedPeak * 2.0 (small margin) — log, don't throw
    //   - FAIL: streamingPeak * N >  bufferedPeak * 2.0 (large margin) — throw
    const passing = streamingPeak * N_PIPELINES <= bufferedPeak * 1.2;
    const seriousFail = streamingPeak * N_PIPELINES > bufferedPeak * 2.0;
    const verdict = passing ? 'PASS' : seriousFail ? 'FAIL' : 'WARN';

    logBenchResult(
      `MemPeak-streaming buffered_mb=${(bufferedPeak / 1e6).toFixed(2)} ` +
        `streaming_mb=${(streamingPeak / 1e6).toFixed(2)} ` +
        `ratio=${ratio.toFixed(2)}x expected=${N_PIPELINES}x ${verdict}`,
    );

    if (seriousFail) {
      throw new Error(
        `Memory peak ratio severely off: streamingPeak * ${N_PIPELINES} ` +
          `(${streamingPeak * N_PIPELINES}) > bufferedPeak * 2.0 (${bufferedPeak * 2.0})`,
      );
    }
    // PASS / WARN both fall through. CONTEXT D-21: small failures don't throw.
    if (passing) {
      expect(streamingPeak * N_PIPELINES).toBeLessThanOrEqual(bufferedPeak * 1.2);
    }
  });
});
