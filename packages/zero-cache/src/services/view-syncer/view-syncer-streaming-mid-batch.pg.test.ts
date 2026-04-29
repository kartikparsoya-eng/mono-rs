/**
 * MIGRATE-03 — view-syncer streaming mid-batch pokePart timing.
 *
 * Per CONTEXT D-15..D-17: with 3 pipelines whose chunk completion is artificially
 * staggered at 10ms / 50ms / 200ms, the first `pokePart` message on the client
 * downstream must arrive BEFORE the slowest pipeline's 200ms completion timestamp.
 * Test runs only when `ZQLITE_RS_USE_STREAMING_CONSUMER === 'true'` (default) —
 * buffered mode cannot fire pokePart mid-batch by construction (D-17).
 *
 * Mechanism: Patch `RustPipelineManager.prototype.advanceStreaming` with a Proxy
 * that injects setTimeout sleeps between successive `next()` calls — pattern
 * borrowed from `pipeline-driver.streaming.test.ts:265-299`. The Proxy is always
 * restored in the finally block per RESEARCH §5.
 *
 * Note: filename uses `.pg.test.ts` (not the plan's `.test.ts`) because the test
 * uses the PG-based `setup()` harness from `view-syncer-test-util.ts` — it would
 * not run under `vitest.config.no-pg.ts` regardless of name. (Rule 3 path
 * deviation — required for the test infrastructure to actually run.)
 */

import {afterEach, beforeEach, describe, expect, vi} from 'vitest';
import type {Downstream} from '../../../../zero-protocol/src/down.ts';
import {PROTOCOL_VERSION} from '../../../../zero-protocol/src/protocol-version.ts';
import {type PgTest, test} from '../../test/db.ts';
import {
  ALL_ISSUES_QUERY,
  COMMENTS_QUERY,
  ISSUES_QUERY_WITH_OWNER,
  messages,
  nextPoke,
  permissionsAll,
  setup,
} from './view-syncer-test-util.ts';
import {type SyncContext} from './view-syncer.ts';

describe('view-syncer streaming mid-batch pokePart (MIGRATE-03)', () => {
  let fixture: Awaited<ReturnType<typeof setup>>;

  const CLIENT_ID = 'client1';
  const SYNC_CONTEXT: SyncContext = {
    clientID: CLIENT_ID,
    profileID: 'p0000g00000003203',
    wsID: 'ws1',
    baseCookie: null,
    protocolVersion: PROTOCOL_VERSION,
    httpCookie: undefined,
    origin: undefined,
    userID: 'bar',
    auth: undefined,
  };

  let originalEnv: string | undefined;

  beforeEach<PgTest>(async ({testDBs}) => {
    originalEnv = process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'];
    process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'] = 'true';

    fixture = await setup(
      testDBs,
      'view_syncer_streaming_mid_batch',
      permissionsAll,
    );

    return async () => {
      fixture.clearMocks();
      await fixture.vs.stop();
      await fixture.viewSyncerDone;
      await testDBs.drop(fixture.cvrDB, fixture.upstreamDb);
      fixture.replicaDbFile.delete();
      if (originalEnv === undefined) {
        delete process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'];
      } else {
        process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'] = originalEnv;
      }
      vi.restoreAllMocks();
    };
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  // Skip in buffered mode per D-17 (mid-batch pokePart is impossible there).
  test.runIf(
    process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'] === undefined ||
      process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'] === 'true',
  )('first pokePart fires before slowest pipeline completes', async () => {
    // Patch RustPipelineManager.prototype.advanceStreaming with a Proxy that
    // injects setTimeout sleeps between successive next() calls — pattern from
    // pipeline-driver.streaming.test.ts:265-299. Each chunk's resolution is
    // delayed by chunkDelaysMs[idx % len], so per-pipeline emission timestamps
    // span 10..200ms.
    const {default: zqliteRs} = await import('zqlite-rs');
    const ManagerCls = (zqliteRs as {RustPipelineManager: unknown})
      .RustPipelineManager as {
      prototype: {
        advanceStreaming: (id: string, changesJson: string) => unknown;
      };
    };
    const original = ManagerCls.prototype.advanceStreaming;
    const chunkDelaysMs = [10, 50, 200];
    let nextChunkIdx = 0;
    const pipelineCompletionTs: number[] = [];

    ManagerCls.prototype.advanceStreaming = function patched(
      this: unknown,
      id: string,
      changesJson: string,
    ): unknown {
      const realStream = original.call(this, id, changesJson) as {
        next: () => Promise<{
          done: boolean;
          kind?: string;
        }>;
        return: () => void;
      };
      return new Proxy(realStream, {
        get(target, prop) {
          if (prop === 'next') {
            return async () => {
              const item = await target.next();
              if (!item.done && item.kind === 'chunk') {
                const idx = nextChunkIdx++;
                const delay = chunkDelaysMs[idx % chunkDelaysMs.length];
                await new Promise(r => setTimeout(r, delay));
                pipelineCompletionTs.push(performance.now());
              }
              return item;
            };
          }
          return Reflect.get(target, prop);
        },
      });
    };

    try {
      // Capture pokeStart / pokePart / pokeEnd timestamps via the new
      // onMessage callback (Task 1).
      const pokeTimestamps: Array<{
        type: string;
        t: number;
        pokeID?: string | undefined;
      }> = [];
      const {queue} = fixture.connectWithQueueAndSource(
        SYNC_CONTEXT,
        [
          {op: 'put', hash: 'q1', ast: ALL_ISSUES_QUERY},
          {op: 'put', hash: 'q2', ast: ISSUES_QUERY_WITH_OWNER},
          {op: 'put', hash: 'q3', ast: COMMENTS_QUERY},
        ],
        undefined,
        undefined,
        (msg, t) => {
          if (
            msg[0] === 'pokeStart' ||
            msg[0] === 'pokePart' ||
            msg[0] === 'pokeEnd'
          ) {
            pokeTimestamps.push({
              type: msg[0],
              t,
              pokeID: (msg[1] as {pokeID?: string}).pokeID,
            });
          }
        },
      );

      // Initial connection / hydration poke (chunkDelays apply only to advance
      // chunks; hydration uses addQueriesStreaming which we don't proxy here).
      await nextPoke(queue);
      fixture.stateChanges.push({state: 'version-ready'});
      await nextPoke(queue);

      // Reset accumulators just before the staggered advance.
      pokeTimestamps.length = 0;
      pipelineCompletionTs.length = 0;
      nextChunkIdx = 0;

      // Apply a transaction that touches the pipelines.
      fixture.replicator.processTransaction(
        '02',
        messages.insert('issues', {
          id: '11',
          title: 't11',
          owner: 'u11',
          big: 11,
          _0_version: '02',
        }),
        messages.insert('comments', {
          id: 'c1',
          issueID: '1',
          text: 'hi',
          _0_version: '02',
        }),
      );

      fixture.stateChanges.push({state: 'version-ready'});

      // Wait for pokeEnd of the staggered advance.
      await nextPoke(queue);

      // Assert: first pokePart arrived BEFORE the slowest pipeline finished.
      // This is the falsifiable mid-batch claim per CONTEXT D-15.
      expect(pipelineCompletionTs.length).toBeGreaterThan(0);
      const slowestPipelineT = Math.max(...pipelineCompletionTs);
      const firstPokePart = pokeTimestamps.find(p => p.type === 'pokePart');
      expect(firstPokePart).toBeDefined();
      expect(firstPokePart!.t).toBeLessThan(slowestPipelineT);
    } finally {
      // CRITICAL per RESEARCH §5: always restore the prototype, no matter
      // how the test exits. Otherwise sibling tests inherit the Proxy.
      ManagerCls.prototype.advanceStreaming = original;
    }
  }, 10000);

  // Touch unused imports so oxlint doesn't strip; nothing references this
  // marker, but it documents the dependency on Downstream.
  void (null as unknown as Downstream);
});
