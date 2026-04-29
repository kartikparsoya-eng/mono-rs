/**
 * TS oracle for HARDEN-01 parity check (Phase 33-01).
 *
 * Adapted from the upstream reference repo at
 * `~/Documents/xy-repo/mono/packages/zero-cache/src/services/view-syncer/pipeline-driver.ts`
 * (lines 450–620 `*addQuery` and 701–817 `*#advance`).
 *
 * Differences from the upstream reference:
 *
 *   - Parameterized via `TsOracleContext` instead of relying on `this.#`-private
 *     state on `PipelineDriver`. This keeps the oracle a pure function library
 *     callable from a sampling shim.
 *
 *   - In mono-rs, hydration AND advance flow exclusively through the Rust
 *     pipeline manager — the per-query TS `Input` is built but its
 *     `setOutput.push` is set to a no-op (see pipeline-driver.ts:751,957).
 *     That means TS-side IVM state never observes pushed changes, so a
 *     full TS replay of `*#advance` is not directly available from the
 *     existing instance state. For HARDEN-01:
 *
 *       - `tsAddQuery` runs a *fresh* TS hydrate via `buildPipeline` +
 *         `hydrateInternal` — this is a real, comparable TS oracle for
 *         the hydrate path.
 *       - `tsAdvance` returns an empty iterable. mono-rs's
 *         all-Rust-hydration architecture does not maintain hydrated TS
 *         pipelines that can replay an advance; running a full TS replay
 *         per-advance would require re-hydrating every pipeline against
 *         the prev snapshot then replaying the diff — a separate, larger
 *         engineering investment beyond HARDEN-01's scope. The shim
 *         caller treats an empty oracle result as "no comparison
 *         performed" (skip) rather than "expected zero changes" so the
 *         full-suite-sample assertion (`getParityDivergenceCount() === 0`)
 *         remains achievable as a smoke check on the wiring itself.
 *
 *     This limitation is documented in `33-01-SUMMARY.md` and is
 *     intentionally surfaced for a follow-up phase (FUZZ-01 / extended
 *     dual-exec) to address.
 */

import type {LogContext} from '@rocicorp/logger';
import type {AST} from '../../../../zero-protocol/src/ast.ts';
import type {Row} from '../../../../zero-protocol/src/data.ts';
import type {PrimaryKey} from '../../../../zero-protocol/src/primary-key.ts';
import {buildPipeline} from '../../../../zql/src/builder/builder.ts';
import {ChangeType} from '../../../../zql/src/ivm/change-type.ts';
import {type Input, type Storage} from '../../../../zql/src/ivm/operator.ts';
import {type Source, type SourceInput} from '../../../../zql/src/ivm/source.ts';
import type {ConnectionCostModel} from '../../../../zql/src/planner/planner-connection.ts';
import type {TableSource} from '../../../../zqlite/src/table-source.ts';
import type {LiteAndZqlSpec} from '../../db/specs.ts';
import type {InspectorDelegate} from '../../server/inspector-delegate.ts';
import type {SnapshotDiff} from './snapshotter.ts';
import {
  hydrateInternal,
  mustGetPrimaryKey,
  type RowChange,
  type Timer,
} from './pipeline-driver.ts';

export interface TsOracleContext {
  readonly lc: LogContext;
  readonly primaryKeys: Map<string, PrimaryKey>;
  readonly tableSpecs: Map<string, LiteAndZqlSpec>;
  readonly tables: Map<string, TableSource>;
  readonly pipelines: Map<string, {input: Input}>;
  readonly getSource: (table: string) => Source;
  readonly createStorage: () => Storage;
  readonly inspectorDelegate: InspectorDelegate;
  readonly costModel: ConnectionCostModel | undefined;
  readonly shouldYieldHydrate: () => boolean;
}

/**
 * TS oracle for the advance path.
 *
 * Returns an empty iterable in mono-rs. See the file header for why a full
 * TS replay of `*#advance` is not available from the all-Rust-hydration
 * runtime. The sampling shim treats an empty oracle result as "no
 * comparison performed" (it short-circuits before invoking
 * `compareChanges`) so the wiring stays in place for a future phase that
 * supplies a real TS-replay oracle.
 *
 * @param _ctx oracle context (unused in the empty-oracle stub)
 * @param _diff snapshot diff (unused in the empty-oracle stub)
 * @param _timer caller-controlled timer (unused in the empty-oracle stub)
 * @param _numChanges number of changes the Rust path observed (unused)
 */
// oxlint-disable-next-line require-yield
export function* tsAdvance(
  _ctx: TsOracleContext,
  _diff: SnapshotDiff,
  _timer: Timer,
  _numChanges: number,
): Iterable<RowChange | 'yield'> {
  // Intentionally empty — see file header. A future phase supplies the
  // real TS-replay oracle.
  return;
}

/**
 * TS oracle for the hydrate path.
 *
 * Builds a fresh TS pipeline via `buildPipeline` and runs it through
 * `hydrateInternal` to produce a real, comparable RowChange[] from
 * SQLite. This mirrors the upstream `*addQuery` (xy-repo lines 450–620)
 * minus the live-companion setup (HARDEN-01 only needs hydrated rows;
 * companion live-pipelines are a runtime concern handled by the Rust
 * path).
 *
 * Companion rows that the Rust path would emit as "add" changes (xy-repo
 * lines 526–536) are NOT emitted here because mono-rs's
 * `addQueriesAsync` excludes companion-bearing queries from the Rust
 * batch (see pipeline-driver.ts:963 `rustEligible = ... &&
 * companionMeta.length === 0`). Comparison is only meaningful for
 * companion-free queries.
 */
export function* tsAddQuery(
  ctx: TsOracleContext,
  _transformationHash: string,
  queryID: string,
  query: AST,
  _timer: Timer,
): Iterable<RowChange | 'yield'> {
  // Build a fresh TS pipeline. Mirrors the buildPipeline call in
  // pipeline-driver.ts::addQueriesAsync (line 919) but with the simpler
  // delegate set the upstream reference uses for `*addQuery` (no
  // measure-push, no exists wrapper, no debug delegate — this is an
  // oracle, not a production input).
  const input = buildPipeline(
    query,
    {
      enableNotExists: true,
      getSource: name => ctx.getSource(name),
      createStorage: () => ctx.createStorage(),
      decorateSourceInput: (sourceInput: SourceInput, _qid: string): Input =>
        sourceInput,
      decorateInput: i => i,
      addEdge() {},
      decorateFilterInput: i => i,
    },
    queryID,
    ctx.costModel,
  );
  // Set a no-op output so push notifications don't error if any flow
  // through during fetch (none should — fetch() is read-only).
  input.setOutput({push: () => []});

  // Hydrate via the same generator the production path uses for the
  // companion-bearing TS fallback (pipeline-driver.ts:1030). This
  // produces RowChange[] in the same shape as the Rust path.
  yield* hydrateInternal(input, queryID, ctx.primaryKeys, ctx.tableSpecs);
}

/**
 * Convenience: run `tsAddQuery` for an array of queries and chain the
 * iterables. Used by the sampling shim's hydrate-path comparison.
 */
export function* tsAddQueryAll(
  ctx: TsOracleContext,
  queries: ReadonlyArray<{
    readonly transformationHash: string;
    readonly queryID: string;
    readonly resolvedQuery: AST;
  }>,
  timer: Timer,
): Iterable<RowChange | 'yield'> {
  for (const q of queries) {
    yield* tsAddQuery(
      ctx,
      q.transformationHash,
      q.queryID,
      q.resolvedQuery,
      timer,
    );
  }
}

// Re-export ChangeType + helpers for symmetry with the upstream
// reference, in case downstream callers want to construct synthetic
// RowChange objects (e.g., for divergence-injection tests).
export {ChangeType, mustGetPrimaryKey};
export type {Row, PrimaryKey, SnapshotDiff, AST};
