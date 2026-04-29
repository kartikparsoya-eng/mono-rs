/**
 * TEST-04 (Phase 31-02): streaming-vs-buffered parity fuzz.
 *
 * Property: For any random input changes accepted by `advanceAsync`,
 * `advanceStreaming` produces an equivalent `RowChange` multiset modulo
 * cross-pipeline ordering. Comparison via `compareChanges` from
 * `dual-executor.ts` (multiset-aware).
 *
 * Mirrors the `fuzz-ivm.test.ts` fast-check pattern with
 * `FUZZ_NUM_RUNS=1000` per CONTEXT.md D-07/D-08. The fuzz lives in this
 * file (not folded into fuzz-ivm) because that suite tests a different
 * module (`RustFilterPredicate`); the shared structure is fast-check
 * harness + 1k iteration default.
 *
 * Run more iterations: FUZZ_NUM_RUNS=10000 npx vitest run ...
 *   src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts
 */

import type {LogContext} from '@rocicorp/logger';
import fc from 'fast-check';
import {afterAll, beforeAll, describe, expect, test} from 'vitest';
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
import {compareChanges, materializeChanges} from './dual-executor.ts';
import {PipelineDriver, type RowChange, type Timer} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';
import {TimeSliceTimer} from './view-syncer.ts';

const NUM_RUNS = parseInt(process.env['FUZZ_NUM_RUNS'] ?? '1000', 10);

const NO_TIME_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

// --- Schema (same minimal shape as pipeline-driver.streaming.test.ts) ---

const items = table('items')
  .columns({
    id: string(),
    name: string(),
  })
  .primaryKey('id');

const clientSchema = createSchema({tables: [items]});

const ALL_ITEMS: AST = {
  table: 'items',
  orderBy: [['id', 'asc']],
};

const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

// --- Arbitraries ---

type Insert = {kind: 'insert'; id: string; name: string};
type Update = {kind: 'update'; id: string; name: string};
type Delete = {kind: 'delete'; id: string};
type Op = Insert | Update | Delete;

type Scenario = {
  /** Initial seed rows (deterministic IDs) */
  seedIds: string[];
  /** Mutation operations applied via the replicator */
  ops: Op[];
};

/** Stable id alphabet — keeps SQLite/Diff/Streamer paths stable. */
const arbId = fc.stringOf(fc.constantFrom(...'abcdefgh'.split('')), {
  minLength: 1,
  maxLength: 3,
});

const arbName = fc.stringOf(fc.constantFrom(...'xyzw0123'.split('')), {
  minLength: 1,
  maxLength: 4,
});

const arbInsert: fc.Arbitrary<Insert> = fc
  .tuple(arbId, arbName)
  .map(([id, name]) => ({kind: 'insert' as const, id, name}));
const arbUpdate: fc.Arbitrary<Update> = fc
  .tuple(arbId, arbName)
  .map(([id, name]) => ({kind: 'update' as const, id, name}));
const arbDelete: fc.Arbitrary<Delete> = arbId.map(id => ({
  kind: 'delete' as const,
  id,
}));

const arbOp: fc.Arbitrary<Op> = fc.oneof(arbInsert, arbUpdate, arbDelete);

const arbScenario: fc.Arbitrary<Scenario> = fc
  .tuple(
    fc.uniqueArray(arbId, {minLength: 0, maxLength: 5}),
    fc.array(arbOp, {minLength: 1, maxLength: 4}),
  )
  .map(([seedIds, ops]) => ({seedIds, ops}));

// --- Driver fixture ---

type Fixture = {
  pipelines: PipelineDriver;
  replicator: FakeReplicator;
  destroy: () => void;
};

let counter = 0;
function setupFixture(scenario: Scenario, lc: LogContext): Fixture {
  const uniqueLabel = `fuzz_${counter++}`;
  const dbFile = new DbFile(`pipelines_streaming_parity_${uniqueLabel}`);
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
    `streaming-vs-buffered-parity.fuzz.test.ts/${uniqueLabel}`,
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
    CREATE TABLE items (
      id TEXT PRIMARY KEY,
      name TEXT,
      _0_version TEXT NOT NULL
    );
  `);
  // Seed deterministically from scenario.
  const seedStmt = db.prepare(
    `INSERT INTO items (id, name, _0_version) VALUES (?, ?, '123')`,
  );
  for (const id of scenario.seedIds) {
    seedStmt.run([id, `seed_${id}`]);
  }
  populateFromExistingTables(db, listTables(db, false));
  const replicator = fakeReplicator(lc, db);

  pipelines.init(clientSchema);

  return {
    pipelines,
    replicator,
    destroy: () => dbFile.delete(),
  };
}

const messages = new ReplicationMessages({
  items: 'id',
  [mutationsTableName]: ['clientGroupID', 'clientID', 'mutationID'],
});

function applyOps(
  replicator: FakeReplicator,
  version: string,
  ops: Op[],
): void {
  // Compose all ops into one transaction. Skip degenerate ops (e.g.
  // update/delete of an id that doesn't exist) — fakeReplicator ignores
  // those at the replication-message layer.
  if (ops.length === 0) return;
  const wireOps = ops.map(op => {
    if (op.kind === 'insert') {
      return messages.insert('items', {id: op.id, name: op.name});
    }
    if (op.kind === 'update') {
      return messages.update('items', {id: op.id, name: op.name});
    }
    return messages.delete('items', {id: op.id});
  });
  try {
    replicator.processTransaction(version, ...wireOps);
  } catch {
    // Some op combinations are rejected by SQLite (e.g. PK conflict).
    // Skip — both drivers see the same transaction outcome (or both
    // throw), so parity is preserved.
  }
}

function startTimer(lc: LogContext): Timer {
  return new TimeSliceTimer(lc).startWithoutYielding();
}

// --- Property test ---

describe('streaming-vs-buffered parity', () => {
  let lc: LogContext;
  beforeAll(() => {
    lc = createSilentLogContext();
  });

  afterAll(() => {
    // Ensure no lingering DB files from the fuzz iterations.
  });

  test('advance_streaming RowChange multiset equals advance_async (1k fuzz iter)', async () => {
    await fc.assert(
      fc.asyncProperty(arbScenario, async scenario => {
        // Build TWO identical drivers (same seed, same op sequence).
        const fxA = setupFixture(scenario, lc);
        const fxB = setupFixture(scenario, lc);
        try {
          // Hydrate identical query on both.
          for (const _ of await fxA.pipelines.addQueriesAsync(
            [{transformationHash: 'h1', queryID: 'q1', ast: ALL_ITEMS}],
            startTimer(lc),
          )) {
            /* drain */
          }
          for (const _ of await fxB.pipelines.addQueriesAsync(
            [{transformationHash: 'h1', queryID: 'q1', ast: ALL_ITEMS}],
            startTimer(lc),
          )) {
            /* drain */
          }

          // Apply identical ops to both replicas.
          applyOps(fxA.replicator, '124', scenario.ops);
          applyOps(fxB.replicator, '124', scenario.ops);

          // Buffered advance.
          const bufferedResult =
            await fxA.pipelines.advanceAsync(NO_TIME_TIMER);
          const bufferedChanges = materializeChanges(bufferedResult.changes);

          // Streaming advance.
          const streamingResult =
            await fxB.pipelines.advanceStreaming(NO_TIME_TIMER);
          const streamingChanges: RowChange[] = [];
          for await (const c of streamingResult.changes) {
            // Phase 32 MIGRATE-03: streaming wrapper now emits 'chunk-end'
            // markers at per-chunk boundaries. Filter both string sentinels
            // — parity assertion is on RowChange multisets only.
            if (c !== 'yield' && c !== 'chunk-end') streamingChanges.push(c);
          }

          // Multiset comparison via compareChanges (sorts both sides).
          const cmp = compareChanges(bufferedChanges, streamingChanges);
          if (!cmp.match) {
            throw new Error(
              `Parity mismatch:\n  scenario: ${JSON.stringify(scenario)}\n` +
                `  ts(buffered) count: ${cmp.tsCount}, rust(streaming) count: ${cmp.rustCount}\n` +
                `  mismatches: ${JSON.stringify(cmp.mismatches.slice(0, 3), null, 2)}`,
            );
          }
          // Also: version + numChanges agreement.
          expect(streamingResult.version).toBe(bufferedResult.version);
          expect(streamingResult.numChanges).toBe(bufferedResult.numChanges);
        } finally {
          fxA.destroy();
          fxB.destroy();
        }
      }),
      {numRuns: NUM_RUNS},
    );
  }, /* timeout: */ 600_000); // 1k iterations × per-fixture build is ~minutes worst-case.
});
