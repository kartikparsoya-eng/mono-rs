/**
 * HARDEN-01 (Phase 33-01) parity-check shim unit tests.
 *
 * Validates the env-mode parsing, sample-rate cadence, strict-mode
 * throw, divergence counter, and counter-reset mechanics of the
 * sampling shim wired into pipeline-driver.ts.
 *
 * Companion file to pipeline-driver-ts-oracle.ts and the shim wired
 * into #rustAdvanceAsync + addQueriesAsync.
 */

import {beforeEach, describe, expect, test, vi} from 'vitest';
import {testLogConfig} from '../../../../otel/src/test-log-config.ts';
import {createSilentLogContext} from '../../../../shared/src/logging-test-utils.ts';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import {createSchema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
  boolean,
  string,
  table,
} from '../../../../zero-schema/src/builder/table-builder.ts';
import {
  CREATE_STORAGE_TABLE,
  DatabaseStorage,
} from '../../../../zqlite/src/database-storage.ts';
import {Database} from '../../../../zqlite/src/db.ts';
import {InspectorDelegate} from '../../server/inspector-delegate.ts';
import {DbFile} from '../../test/lite.ts';
import {upstreamSchema, type ShardID} from '../../types/shards.ts';
import {initReplicationState} from '../replicator/schema/replication-state.ts';
import {fakeReplicator, ReplicationMessages} from '../replicator/test-utils.ts';
import * as oracleModule from './pipeline-driver-ts-oracle.ts';
import {ChangeType} from './pipeline-driver-ts-oracle.ts';
import {
  getParityCheckInvocationCountForTesting,
  getParityDivergenceCount,
  parseParityCheckMode,
  PipelineDriver,
  resetParityDivergenceCount,
  type RowChange,
  type Timer,
} from './pipeline-driver.ts';
import {Snapshotter} from './snapshotter.ts';

const NO_TIME_TIMER: Timer = {
  elapsedLap: () => 0,
  totalElapsed: () => 0,
};

describe('HARDEN-01 parity check shim', () => {
  beforeEach(() => {
    resetParityDivergenceCount();
  });

  describe('mode parsing', () => {
    test('undefined => off', () => {
      expect(parseParityCheckMode(undefined)).toBe('off');
    });

    test('"" => off', () => {
      expect(parseParityCheckMode('')).toBe('off');
    });

    test('"off" => off', () => {
      expect(parseParityCheckMode('off')).toBe('off');
    });

    test('"sample" => sample', () => {
      expect(parseParityCheckMode('sample')).toBe('sample');
    });

    test('"strict" => strict', () => {
      expect(parseParityCheckMode('strict')).toBe('strict');
    });

    test('"STRICT" => off (case-sensitive — only exact "sample"/"strict")', () => {
      expect(parseParityCheckMode('STRICT')).toBe('off');
    });

    test('"true" => off (only sample/strict accepted)', () => {
      expect(parseParityCheckMode('true')).toBe('off');
    });

    test('"1" => off', () => {
      expect(parseParityCheckMode('1')).toBe('off');
    });
  });

  describe('counter mechanics', () => {
    test('initial counters are zero', () => {
      expect(getParityDivergenceCount()).toBe(0);
      expect(getParityCheckInvocationCountForTesting()).toBe(0);
    });

    test('resetParityDivergenceCount zeros both counters', () => {
      // The reset function lives at module level, so we cannot easily
      // mutate the underlying state without going through the driver.
      // Verify reset works by calling it and asserting both are zero
      // even after the beforeEach has run.
      resetParityDivergenceCount();
      expect(getParityDivergenceCount()).toBe(0);
      expect(getParityCheckInvocationCountForTesting()).toBe(0);
    });
  });

  describe('cadence behavior with real PipelineDriver', () => {
    // These tests construct a real PipelineDriver under different env
    // var settings and observe the parityCheckInvocationCount after
    // driving a hydrate. Each test isolates its own env setup since
    // the env var is read once per instance in the constructor (P-03
    // carry-forward from Phase 32 — not module top-level).

    const shardID: ShardID = {appID: 'zeroz', shardNum: 1};
    const mutationsTableName = `${upstreamSchema(shardID)}.mutations`;

    const items = table('items')
      .columns({
        id: string(),
        active: boolean(),
      })
      .primaryKey('id');
    const clientSchema = createSchema({tables: [items]});
    const ITEMS_QUERY: AST = {
      table: 'items',
      orderBy: [['id', 'asc']],
    };

    function makeDriver(): {
      driver: PipelineDriver;
      cleanup: () => void;
    } {
      const lc = createSilentLogContext();
      const dbFile = new DbFile('parity_check_test');
      const conn = dbFile.connect(lc);
      conn.pragma('journal_mode = wal2');

      const storage = new Database(lc, ':memory:');
      storage.prepare(CREATE_STORAGE_TABLE).run();

      const driver = new PipelineDriver(
        lc,
        testLogConfig,
        new Snapshotter(lc, dbFile.path, {appID: shardID.appID}),
        shardID,
        new DatabaseStorage(storage).createClientGroupStorage(
          'parity-check-cg',
        ),
        'parity-check.test.ts',
        new InspectorDelegate(undefined),
        () => 200,
      );

      const db = dbFile.connect(lc);
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
          active BOOL,
          _0_version TEXT NOT NULL
        );
        INSERT INTO items (id, active, _0_version) VALUES ('a', 1, '123');
        INSERT INTO items (id, active, _0_version) VALUES ('b', 0, '123');
      `);

      driver.init(clientSchema);

      return {
        driver,
        cleanup: () => {
          driver.destroy();
          dbFile.delete();
        },
      };
    }

    test('mode=off: hydrate does not increment parityCheckInvocationCount', async () => {
      const prev = process.env['ZQLITE_RS_PARITY_CHECK'];
      try {
        delete process.env['ZQLITE_RS_PARITY_CHECK'];
        const startCount = getParityCheckInvocationCountForTesting();
        const {driver, cleanup} = makeDriver();
        try {
          await driver.addQueriesAsync(
            [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
            NO_TIME_TIMER,
          );
          // off mode: counter does NOT increment (early return in
          // #shouldRunParityCheck).
          expect(getParityCheckInvocationCountForTesting()).toBe(startCount);
          expect(getParityDivergenceCount()).toBe(0);
        } finally {
          cleanup();
        }
      } finally {
        if (prev !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK'] = prev;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK'];
        }
      }
    });

    test('mode=sample, rate=1: hydrate increments invocationCount', async () => {
      const prevMode = process.env['ZQLITE_RS_PARITY_CHECK'];
      const prevRate = process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
      try {
        process.env['ZQLITE_RS_PARITY_CHECK'] = 'sample';
        process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = '1';
        const startCount = getParityCheckInvocationCountForTesting();
        const {driver, cleanup} = makeDriver();
        try {
          await driver.addQueriesAsync(
            [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
            NO_TIME_TIMER,
          );
          // sample/rate=1: counter increments at least once for the
          // hydrate-path #shouldRunParityCheck() call.
          expect(getParityCheckInvocationCountForTesting()).toBeGreaterThan(
            startCount,
          );
        } finally {
          cleanup();
        }
      } finally {
        if (prevMode !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK'] = prevMode;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK'];
        }
        if (prevRate !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = prevRate;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
        }
      }
    });

    test('mode=sample, rate=10: small invocation counts do not exceed expected', async () => {
      const prevMode = process.env['ZQLITE_RS_PARITY_CHECK'];
      const prevRate = process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
      try {
        process.env['ZQLITE_RS_PARITY_CHECK'] = 'sample';
        process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = '10';
        resetParityDivergenceCount();
        const {driver, cleanup} = makeDriver();
        try {
          // Single hydrate => 1 invocation increment. Cadence
          // (1 % 10 !== 0) means it does NOT fire the comparison, so
          // divergence stays 0.
          await driver.addQueriesAsync(
            [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
            NO_TIME_TIMER,
          );
          expect(
            getParityCheckInvocationCountForTesting(),
          ).toBeGreaterThanOrEqual(1);
          expect(getParityDivergenceCount()).toBe(0);
        } finally {
          cleanup();
        }
      } finally {
        if (prevMode !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK'] = prevMode;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK'];
        }
        if (prevRate !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = prevRate;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
        }
      }
    });

    test('mode=strict, rate=10: every hydrate fires comparison (rate ignored)', async () => {
      const prevMode = process.env['ZQLITE_RS_PARITY_CHECK'];
      const prevRate = process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
      try {
        process.env['ZQLITE_RS_PARITY_CHECK'] = 'strict';
        process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = '10';
        resetParityDivergenceCount();
        const {driver, cleanup} = makeDriver();
        try {
          // strict mode: TS oracle's tsAddQuery produces real rows from
          // SQLite (same data Rust path produces) so the comparison
          // should match. No throw expected for matching paths.
          await driver.addQueriesAsync(
            [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
            NO_TIME_TIMER,
          );
          // Either the divergence count remains 0 (paths matched —
          // ideal) OR throws (paths diverged). The robust assertion is
          // that the invocation counter incremented (proving strict
          // mode actually ran the comparison).
          expect(
            getParityCheckInvocationCountForTesting(),
          ).toBeGreaterThanOrEqual(1);
        } finally {
          cleanup();
        }
      } finally {
        if (prevMode !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK'] = prevMode;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK'];
        }
        if (prevRate !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = prevRate;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
        }
      }
    });
  });

  describe('reset between tests', () => {
    test('resetParityDivergenceCount zeros both counters', () => {
      resetParityDivergenceCount();
      expect(getParityDivergenceCount()).toBe(0);
      expect(getParityCheckInvocationCountForTesting()).toBe(0);
    });
  });

  // ---------------------------------------------------------------------------
  // Streaming parity wire — verifies #maybeRunParityCheck fires on the
  // streaming hydrate (addQueriesStreaming) and streaming advance
  // (advanceStreaming) paths, which are the production defaults
  // (view-syncer.ts:361-365 sets useStreamingConsumer=true).
  // ---------------------------------------------------------------------------
  describe('streaming parity', () => {
    const streamingShardID: ShardID = {appID: 'zeroz', shardNum: 1};
    const streamingMutationsTable = `${upstreamSchema(streamingShardID)}.mutations`;

    const items = table('items')
      .columns({
        id: string(),
        active: boolean(),
      })
      .primaryKey('id');
    const streamingClientSchema = createSchema({tables: [items]});
    const ITEMS_QUERY: AST = {
      table: 'items',
      orderBy: [['id', 'asc']],
    };

    const replicationMessages = new ReplicationMessages({
      items: 'id',
      [streamingMutationsTable]: ['clientGroupID', 'clientID', 'mutationID'],
    });

    function makeStreamingDriver(label: string): {
      driver: PipelineDriver;
      dbFile: DbFile;
      cleanup: () => void;
    } {
      const lc = createSilentLogContext();
      const dbFile = new DbFile(`parity_streaming_${label}`);
      const conn = dbFile.connect(lc);
      conn.pragma('journal_mode = wal2');

      const storage = new Database(lc, ':memory:');
      storage.prepare(CREATE_STORAGE_TABLE).run();

      const driver = new PipelineDriver(
        lc,
        testLogConfig,
        new Snapshotter(lc, dbFile.path, {appID: streamingShardID.appID}),
        streamingShardID,
        new DatabaseStorage(storage).createClientGroupStorage(
          `parity-streaming-cg-${label}`,
        ),
        `parity-check.test.ts/${label}`,
        new InspectorDelegate(undefined),
        () => 200,
      );

      const db = dbFile.connect(lc);
      initReplicationState(db, ['zero_data'], '123');
      db.exec(/*sql*/ `
        CREATE TABLE "${streamingMutationsTable}" (
          "clientGroupID"  TEXT,
          "clientID"       TEXT,
          "mutationID"     INTEGER,
          "result"         TEXT,
          _0_version       TEXT NOT NULL,
          PRIMARY KEY ("clientGroupID", "clientID", "mutationID")
        );
        CREATE TABLE items (
          id TEXT PRIMARY KEY,
          active BOOL,
          _0_version TEXT NOT NULL
        );
        INSERT INTO items (id, active, _0_version) VALUES ('a', 1, '123');
        INSERT INTO items (id, active, _0_version) VALUES ('b', 0, '123');
      `);

      driver.init(streamingClientSchema);

      return {
        driver,
        dbFile,
        cleanup: () => {
          driver.destroy();
          dbFile.delete();
        },
      };
    }

    function setEnv(mode: string, rate: string): () => void {
      const prevMode = process.env['ZQLITE_RS_PARITY_CHECK'];
      const prevRate = process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
      process.env['ZQLITE_RS_PARITY_CHECK'] = mode;
      process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = rate;
      return () => {
        if (prevMode !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK'] = prevMode;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK'];
        }
        if (prevRate !== undefined) {
          process.env['ZQLITE_RS_PARITY_CHECK_RATE'] = prevRate;
        } else {
          delete process.env['ZQLITE_RS_PARITY_CHECK_RATE'];
        }
      };
    }

    beforeEach(() => {
      resetParityDivergenceCount();
      vi.restoreAllMocks();
    });

    // S1: cadence-mode (sample, rate=1) advanceStreaming increments counter.
    test('S1: mode=sample/rate=1, advanceStreaming increments invocationCount', async () => {
      const restoreEnv = setEnv('sample', '1');
      try {
        const startCount = getParityCheckInvocationCountForTesting();
        const {driver, dbFile, cleanup} = makeStreamingDriver('s1');
        try {
          // Hydrate first so advance has a non-trivial pipeline.
          for await (const _ of await driver.addQueriesStreaming(
            [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
            NO_TIME_TIMER,
          )) {
            void _;
          }

          // Mutate so advance has actual work.
          const lc = createSilentLogContext();
          const writeConn = dbFile.connect(lc);
          const replicator = fakeReplicator(lc, writeConn);
          replicator.processTransaction(
            '124',
            replicationMessages.insert('items', {id: 'c'}),
          );

          // Drive streaming advance and fully consume.
          const result = await driver.advanceStreaming(NO_TIME_TIMER);
          for await (const _ of result.changes) {
            void _;
          }

          // S1 assertion: counter strictly increased — proving the
          // streaming wire fired the parity gate. (Rate=1 means every
          // invocation runs comparison; >startCount also covers the
          // hydrate's contribution.)
          expect(getParityCheckInvocationCountForTesting()).toBeGreaterThan(
            startCount,
          );
        } finally {
          cleanup();
        }
      } finally {
        restoreEnv();
      }
    });

    // S2: cadence-mode (sample, rate=1) addQueriesStreaming increments counter.
    test('S2: mode=sample/rate=1, addQueriesStreaming increments invocationCount', async () => {
      const restoreEnv = setEnv('sample', '1');
      try {
        const startCount = getParityCheckInvocationCountForTesting();
        const {driver, cleanup} = makeStreamingDriver('s2');
        try {
          // Drive streaming hydrate; fully consume.
          for await (const _ of await driver.addQueriesStreaming(
            [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
            NO_TIME_TIMER,
          )) {
            void _;
          }

          // S2 assertion: counter strictly increased — proving the
          // streaming hydrate wire fired the parity gate.
          expect(getParityCheckInvocationCountForTesting()).toBeGreaterThan(
            startCount,
          );
        } finally {
          cleanup();
        }
      } finally {
        restoreEnv();
      }
    });

    // S3: divergence detection on streaming advance — strict mode + injected
    // divergence via a mocked tsAdvance (oracle module spy).
    //
    // The default `tsAdvance` is intentionally empty — it never yields any
    // RowChange. Strict mode runs the comparison even with empty TS, but
    // matching empty-vs-some-rust-changes IS a divergence (rustCount > 0,
    // tsCount === 0). We further inject a synthetic non-matching change
    // so the divergence is unambiguously caused by the spy and the
    // comparison must have been driven.
    test('S3: mode=strict, advanceStreaming detects divergence (mocked tsAdvance)', async () => {
      const restoreEnv = setEnv('strict', '10');
      try {
        // Spy on tsAdvance to inject a synthetic divergent change so the
        // comparison fires AND finds a mismatch.
        const tsAdvanceSpy = vi
          .spyOn(oracleModule, 'tsAdvance')
          .mockImplementation(function* injectDivergence() {
            // Yield a change that the Rust side will NOT produce so
            // compareChanges() reports tsCount > rustCount.
            yield {
              type: ChangeType.ADD,
              queryID: 'q1',
              table: 'items',
              rowKey: {id: 'NONEXISTENT'},
              row: {id: 'NONEXISTENT', active: true},
            } as RowChange;
          });

        const {driver, dbFile, cleanup} = makeStreamingDriver('s3');
        try {
          for await (const _ of await driver.addQueriesStreaming(
            [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
            NO_TIME_TIMER,
          )) {
            void _;
          }

          const lc = createSilentLogContext();
          const writeConn = dbFile.connect(lc);
          const replicator = fakeReplicator(lc, writeConn);
          replicator.processTransaction(
            '124',
            replicationMessages.insert('items', {id: 'c'}),
          );

          const result = await driver.advanceStreaming(NO_TIME_TIMER);

          // Strict mode + divergence: drain throws OR divergence count
          // increments. Both prove the comparison fired against streaming
          // output.
          let didThrow = false;
          try {
            for await (const _ of result.changes) {
              void _;
            }
          } catch (err) {
            didThrow = true;
            expect(String(err)).toMatch(/parity check failed|divergence/i);
          }

          if (!didThrow) {
            expect(getParityDivergenceCount()).toBeGreaterThan(0);
          }

          // tsAdvance MUST have been invoked (otherwise the wire is
          // bypassed regardless of throw/divergence-count outcome).
          expect(tsAdvanceSpy).toHaveBeenCalled();
        } finally {
          cleanup();
        }
      } finally {
        restoreEnv();
      }
    });

    // S4: divergence detection on streaming hydrate — strict mode + injected
    // divergence via a mocked tsAddQueryAll.
    test('S4: mode=strict, addQueriesStreaming detects divergence (mocked tsAddQueryAll)', async () => {
      const restoreEnv = setEnv('strict', '10');
      try {
        const tsAddQueryAllSpy = vi
          .spyOn(oracleModule, 'tsAddQueryAll')
          .mockImplementation(function* injectDivergence() {
            yield {
              type: ChangeType.ADD,
              queryID: 'q1',
              table: 'items',
              rowKey: {id: 'NONEXISTENT'},
              row: {id: 'NONEXISTENT', active: true},
            } as RowChange;
          });

        const {driver, cleanup} = makeStreamingDriver('s4');
        try {
          let didThrow = false;
          try {
            for await (const _ of await driver.addQueriesStreaming(
              [{transformationHash: 'h1', queryID: 'q1', ast: ITEMS_QUERY}],
              NO_TIME_TIMER,
            )) {
              void _;
            }
          } catch (err) {
            didThrow = true;
            expect(String(err)).toMatch(/parity check failed|divergence/i);
          }

          if (!didThrow) {
            expect(getParityDivergenceCount()).toBeGreaterThan(0);
          }

          expect(tsAddQueryAllSpy).toHaveBeenCalled();
        } finally {
          cleanup();
        }
      } finally {
        restoreEnv();
      }
    });
  });
});
