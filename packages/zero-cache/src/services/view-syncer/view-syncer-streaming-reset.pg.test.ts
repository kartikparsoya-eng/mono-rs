/**
 * MIGRATE-04 — view-syncer streaming ResetPipelinesSignal mid-stream cancel.
 *
 * Per CONTEXT D-18..D-19: when a ResetPipelinesSignal is thrown mid-stream
 * (companion-scalar reset), view-syncer's #advancePipelines catch block must:
 *   1. Call pokers.cancel() exactly once (D-18 — exact count, not derived).
 *   2. NOT call pokers.end() (no CVR commit).
 *   3. Allow recovery: a subsequent (non-reset) advance must succeed.
 *
 * Mechanism: We spy on `ClientHandler.prototype.startPoke` (the per-client
 * builder that the module-level startPoke() aggregates) and wrap the returned
 * PokeHandler with counted cancel/end. The aggregator's `cancel`/`end` invokes
 * each per-client child once, so for the typical 1-client case the parent
 * count equals the child count. We then patch
 * `RustPipelineManager.prototype.advanceStreaming` so the SECOND .next()
 * emits a `reset` StreamItem; the Phase 31 wrapper translates that to
 * `throw new ResetPipelinesSignal(..., 'scalar-subquery')`. View-syncer's
 * catch block (Task 5) must call pokers.cancel() exactly once.
 *
 * Filename uses .pg.test.ts because the test relies on the PG-based setup()
 * harness — same Rule 3 deviation from MIGRATE-03.
 */

import {afterEach, beforeEach, describe, expect, vi} from 'vitest';
import type {Downstream} from '../../../../zero-protocol/src/down.ts';
import {PROTOCOL_VERSION} from '../../../../zero-protocol/src/protocol-version.ts';
import {type PgTest, test} from '../../test/db.ts';
import {ClientHandler, type PokeHandler} from './client-handler.ts';
import {
  ALL_ISSUES_QUERY,
  messages,
  nextPoke,
  permissionsAll,
  setup,
} from './view-syncer-test-util.ts';
import {type SyncContext} from './view-syncer.ts';

describe('view-syncer streaming ResetPipelinesSignal mid-stream (MIGRATE-04)', () => {
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
      'view_syncer_streaming_reset',
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

  test.runIf(
    process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'] === undefined ||
      process.env['ZQLITE_RS_USE_STREAMING_CONSUMER'] === 'true',
  )(
    'companion-scalar change mid-stream triggers pokers.cancel() and skips CVR commit',
    async () => {
      // ---- 1. Spy on ClientHandler.prototype.startPoke so each per-client
      //         PokeHandler returned has its cancel/end wrapped with counters.
      //         The module-level startPoke() aggregator forwards cancel/end
      //         to each child once.
      const cancelSpies: Array<ReturnType<typeof vi.fn>> = [];
      const endSpies: Array<ReturnType<typeof vi.fn>> = [];
      const origPrototypeStartPoke = ClientHandler.prototype.startPoke;
      const startPokeSpy = vi
        .spyOn(ClientHandler.prototype, 'startPoke')
        .mockImplementation(function (this: ClientHandler, tentativeVersion) {
          const real: PokeHandler = origPrototypeStartPoke.call(
            this,
            tentativeVersion,
          );
          const cancelSpy = vi.fn(real.cancel.bind(real));
          const endSpy = vi.fn(real.end.bind(real));
          cancelSpies.push(cancelSpy);
          endSpies.push(endSpy);
          return {
            addPatch: real.addPatch.bind(real),
            // Phase 32 MIGRATE-03: forward flush() through the wrapper so
            // chunk-boundary flushes from #processChanges still reach the
            // real per-client poker (otherwise the streaming consumer's
            // pokers.flush() would be a silent no-op for this test).
            flush: real.flush.bind(real),
            cancel: cancelSpy as unknown as () => Promise<void>,
            end: endSpy as unknown as (
              version: Parameters<typeof real.end>[0],
            ) => Promise<void>,
          };
        });

      // ---- 2. Connect a client and observe pokeStart / pokePart / pokeEnd.
      const observed: Array<{type: string}> = [];
      const {queue} = fixture.connectWithQueueAndSource(
        SYNC_CONTEXT,
        [{op: 'put', hash: 'q1', ast: ALL_ISSUES_QUERY}],
        undefined,
        undefined,
        msg => {
          if (
            msg[0] === 'pokeStart' ||
            msg[0] === 'pokePart' ||
            msg[0] === 'pokeEnd'
          ) {
            observed.push({type: msg[0]});
          }
        },
      );

      // Initial connection / hydration — completes a pokeStart/pokePart/pokeEnd
      // cycle. We measure the RESET-injected advance after this.
      await nextPoke(queue);
      fixture.stateChanges.push({state: 'version-ready'});
      await nextPoke(queue);

      // Snapshot startPoke spy state at the boundary so we can isolate the
      // reset-injected advance's cancel/end counts. NOTE: after a reset,
      // view-syncer's recovery path (#syncQueryPipelineSet at line 521)
      // creates its OWN poker that DOES call .end() — so we must check
      // the FIRST new poker (the advance poker) specifically, not the
      // aggregate end count across all newly created pokers.
      const numPokersBeforeAdvance = cancelSpies.length;
      observed.length = 0;

      // ---- 3. Patch RustPipelineManager.prototype.advanceStreaming so the
      //         second .next() emits a `reset` StreamItem. The Phase 31
      //         wrapper maps that to `throw new ResetPipelinesSignal(...)`,
      //         which view-syncer's #advancePipelines catch must handle by
      //         calling pokers.cancel() exactly once and skipping pokeEnd.
      const {default: zqliteRs} = await import('zqlite-rs');
      const ManagerCls = (zqliteRs as {RustPipelineManager: unknown})
        .RustPipelineManager as {
        prototype: {
          advanceStreaming: (id: string, changesJson: string) => unknown;
        };
      };
      const originalAdvanceStreaming = ManagerCls.prototype.advanceStreaming;
      let advanceCallCount = 0;
      ManagerCls.prototype.advanceStreaming = function patched(
        this: unknown,
        id: string,
        changesJson: string,
      ): unknown {
        advanceCallCount++;
        const realStream = originalAdvanceStreaming.call(
          this,
          id,
          changesJson,
        ) as {
          next: () => Promise<{
            done: boolean;
            kind?: string;
            chunk?: Buffer;
            errorMsg?: string;
            errorKind?: string;
            reason?: string;
          }>;
          return: () => void;
        };
        // Only inject the reset on the FIRST advance call. Second advance
        // (recovery) uses the real stream untouched.
        if (advanceCallCount > 1) {
          return realStream;
        }
        let nextCount = 0;
        return new Proxy(realStream, {
          get(target, prop) {
            if (prop === 'next') {
              return async () => {
                nextCount++;
                if (nextCount === 1) {
                  return target.next();
                }
                // Second call: synthesize a reset StreamItem (companion
                // scalar). Returns immediately without forwarding to the
                // real stream.
                return {
                  done: false,
                  kind: 'reset',
                  reason: 'companion-scalar mid-stream injected (MIGRATE-04)',
                };
              };
            }
            return Reflect.get(target, prop);
          },
        });
      };

      try {
        // ---- 4. Trigger an advance that will hit the patched stream.
        fixture.replicator.processTransaction(
          '02',
          messages.insert('issues', {
            id: '11',
            title: 't11',
            owner: 'u11',
            big: 11,
            _0_version: '02',
          }),
        );
        fixture.stateChanges.push({state: 'version-ready'});

        // ---- 5. Wait briefly for the reset-injected advance to settle.
        //         Reset path will not produce a pokeEnd downstream by
        //         construction (per D-18). 150ms is enough for the
        //         #advancePipelines rejection + cancel handler to run.
        await new Promise(r => setTimeout(r, 150));

        // ---- 6. Assert the reset-injected advance.
        // The FIRST new poker (created by #advancePipelines for the
        // reset-bound advance) must satisfy CONTEXT D-18 verbatim:
        //   cancel called exactly once, end NEVER called.
        //
        // Subsequent pokers (created by recovery #syncQueryPipelineSet
        // re-hydration) DO call .end() — that's expected, not a violation.
        // We isolate the advance poker by indexing into the spy arrays
        // at numPokersBeforeAdvance.
        expect(cancelSpies.length).toBeGreaterThan(numPokersBeforeAdvance);
        const advancePokerCancelCalls =
          cancelSpies[numPokersBeforeAdvance].mock.calls.length;
        const advancePokerEndCalls =
          endSpies[numPokersBeforeAdvance].mock.calls.length;

        // CONTEXT D-18 verbatim: cancel called exactly once.
        expect(advancePokerCancelCalls).toBe(1);
        // No CVR commit / pokeEnd on the advance poker (recovery pokers
        // are separate handlers — we don't constrain them here).
        expect(advancePokerEndCalls).toBe(0);
        // No client-visible pokeEnd was emitted from the cancel path during
        // the reset window (the recovery's pokeEnd may arrive afterwards
        // — covered by step 7's recovery assertion).
        // We don't assert observed pokeEnd here because the recovery
        // hydrate may have already emitted one within the 150ms window;
        // the per-poker spy assertions above are the authoritative D-18
        // signal.

        // ---- 7. Recovery: subsequent advance succeeds.
        // The reset's cancel pushes a pokeEnd(cancel:true) to source, and
        // the recovery #syncQueryPipelineSet emits its own pokeStart/Part/End.
        // Both already flowed through onMessage during the 150ms wait, so
        // we drain queue fully (multiple pokeEnd cycles) before triggering
        // tx03 so the next pokeEnd we observe is unambiguously tx03's.
        const dummy = 'drained' as unknown as Downstream;
        // Drain until the queue is fully empty (timeout-based peek).
        // Each `await dequeue(dummy, 50)` returns dummy if no msg arrives
        // in 50ms, signalling the queue is empty.
        for (let i = 0; i < 50; i++) {
          const got = await queue.dequeue(dummy, 50);
          if (got === dummy) break;
        }
        observed.length = 0;
        fixture.replicator.processTransaction(
          '03',
          messages.insert('issues', {
            id: '12',
            title: 't12',
            owner: 'u12',
            big: 12,
            _0_version: '03',
          }),
        );
        fixture.stateChanges.push({state: 'version-ready'});
        await nextPoke(queue);
        const sawPokeEndAfterRecovery = observed.some(
          m => m.type === 'pokeEnd',
        );
        expect(sawPokeEndAfterRecovery).toBe(true);
      } finally {
        ManagerCls.prototype.advanceStreaming = originalAdvanceStreaming;
        startPokeSpy.mockRestore();
      }
    },
    10000,
  );
});
