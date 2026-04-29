import {createRequire} from 'node:module';
import {readdirSync, statSync} from 'node:fs';
import {dirname, join} from 'node:path';
import type {LogContext} from '@rocicorp/logger';
import {assert, unreachable} from '../../../../shared/src/asserts.ts';
import {must} from '../../../../shared/src/must.ts';
import {RustStorage} from '../../../../zero-ivm-rs/index.js';
import {RustTakeStorage} from '../../../../zero-ivm-rs/ts/rust-take-storage.ts';
import type {
  AST,
  CompoundKey,
  LiteralValue,
} from '../../../../zero-protocol/src/ast.ts';
import type {Condition} from '../../../../zero-protocol/src/ast.ts';
import type {ClientSchema} from '../../../../zero-protocol/src/client-schema.ts';
import type {Row} from '../../../../zero-protocol/src/data.ts';
import type {PrimaryKey} from '../../../../zero-protocol/src/primary-key.ts';
import {buildPipeline} from '../../../../zql/src/builder/builder.ts';
import {
  Debug,
  runtimeDebugFlags,
} from '../../../../zql/src/builder/debug-delegate.ts';
import {ChangeIndex} from '../../../../zql/src/ivm/change-index.ts';
import {ChangeType} from '../../../../zql/src/ivm/change-type.ts';
import type {Change} from '../../../../zql/src/ivm/change.ts';
import type {Node} from '../../../../zql/src/ivm/data.ts';
import type {FilterOperator} from '../../../../zql/src/ivm/filter-operators.ts';
import {
  skipYields,
  type Input,
  type Storage,
} from '../../../../zql/src/ivm/operator.ts';
import type {SourceSchema} from '../../../../zql/src/ivm/schema.ts';
import {type Source, type SourceInput} from '../../../../zql/src/ivm/source.ts';
import type {ConnectionCostModel} from '../../../../zql/src/planner/planner-connection.ts';
import {MeasurePushOperator} from '../../../../zql/src/query/measure-push-operator.ts';
import type {ClientGroupStorage} from '../../../../zqlite/src/database-storage.ts';
import type {Database} from '../../../../zqlite/src/db.ts';
import {
  resolveSimpleScalarSubqueries,
  type CompanionSubquery,
} from '../../../../zqlite/src/resolve-scalar-subqueries.ts';
import {createSQLiteCostModel} from '../../../../zqlite/src/sqlite-cost-model.ts';
import {
  decodeAdvanceChunkBuf,
  decodeAdvanceResultBuf,
  type DecodedRowChange,
} from './decode-advance-buf.ts';
import {materializeChanges} from './dual-executor.ts';
import {createRustExistsWrapper} from './rust-exists.ts';

let RustPipelineManagerClass:
  | {
      new (): RustPipelineManagerInstance;
    }
  | undefined;

interface RustPipelineManagerInstance {
  createInstance(id: string, dbPath: string): void;
  removeInstance(id: string): void;
  setTableSpecs(
    id: string,
    syncableTablesJson: string,
    allTableNamesJson: string,
  ): void;
  setPermissionTables(id: string, tablesJson: string): void;
  setQueryCompanions(id: string, queryId: string, companionsJson: string): void;
  addQuery(id: string, queryJson: string): void;
  removeQuery(id: string, queryId: string): void;
  hydrate(id: string): Buffer;
  hydrateQuery(id: string, queryId: string): Buffer;
  advance(id: string, changesJson: string): Buffer;
  advanceAsync(id: string, changesJson: string): Promise<Buffer>;
  hydrateAsync(id: string): Promise<Buffer>;
  hydrateQueryAsync(id: string, queryId: string): Promise<Buffer>;
  swapSnapshot(id: string, newDbPath: string): void;
  pipelineCount(id: string): number;
  // Phase 31-01 streaming surface — additive, alongside the buffered
  // methods above.
  advanceStreaming(id: string, changesJson: string): NapiAdvanceStream;
  hydrateStreaming(id: string): NapiHydrateStream;
  hydrateQueryStreaming(id: string, queryId: string): NapiHydrateStream;
}

/**
 * One stream item pulled from {@link NapiAdvanceStream.next} /
 * {@link NapiHydrateStream.next}. Mirror of `NextChunkValue` exported by
 * `packages/zqlite-rs/index.d.ts`. Inlined here to avoid coupling the
 * pipeline-driver module to the napi `.d.ts` generation cycle (the binary
 * may not exist on disk in some test environments — see
 * {@link assertNapiBinaryFreshness}).
 */
type NapiNextChunkValue = {
  done: boolean;
  kind?: string | undefined;
  chunk?: Buffer | undefined;
  reason?: string | undefined;
  errorMsg?: string | undefined;
  errorKind?: string | undefined;
};

/**
 * Napi-side stream handle returned by `RustPipelineManager.advanceStreaming`.
 *
 * Note on the cancel method's name: the Rust definition is `pub fn return_`
 * (because `return` is a Rust keyword), but napi-rs strips the trailing
 * underscore so the generated JS method is `stream.return()`. Comments
 * elsewhere refer to it as `stream.return_()` per the Rust source.
 */
interface NapiAdvanceStream {
  next(): Promise<NapiNextChunkValue>;
  return(): void;
}

interface NapiHydrateStream {
  next(): Promise<NapiNextChunkValue>;
  return(): void;
}

/**
 * Error raised when the Rust streaming path emits a `StreamItem::Error`.
 * The `kind` discriminator distinguishes panic-class failures from
 * channel/rayon-class failures. Per 31-CONTEXT.md D-10/D-11.
 *
 * Distinct from {@link ResetPipelinesSignal}: ResetPipelinesSignal is a
 * recoverable signal (view-syncer rolls back the advance and retries);
 * RustStreamError is a real failure (view-syncer logs + bubbles).
 *
 * Phase 32's view-syncer migration MUST branch on
 * `error instanceof RustStreamError` and `error.kind === 'panic'` —
 * NOT on string-matching `.message` (per D-13).
 */
export class RustStreamError extends Error {
  readonly source = 'rust-stream' as const;
  readonly kind: 'panic' | 'rayon_error' | 'channel_closed';
  constructor(
    message: string,
    kind: 'panic' | 'rayon_error' | 'channel_closed',
  ) {
    super(message);
    this.name = 'RustStreamError';
    this.kind = kind;
  }
}

/**
 * AUDIT-02 / Plan 30-05 build-freshness gate.
 *
 * Asserts the resolved zqlite-rs `.node` binary is at least as new as the
 * most-recently-modified Rust source file in the same package. If the
 * binary is stale (a developer or CI ran `vitest run` directly without
 * first running `npm run build` in `packages/zqlite-rs/`), throw a clear
 * error pointing at the rebuild command instead of silently running
 * pre-fix IVM logic.
 *
 * The original AUDIT-02 verification gap (`30-VERIFICATION.md` truths #6
 * and #7) was exactly this failure mode: the source contained the 30-02
 * fix but the parent repo's `.node` binary had been built before the fix
 * landed, so the integration test silently observed pre-fix behavior.
 *
 * Disabled in production (`NODE_ENV === 'production'`) — there, the
 * binary ships pre-built and the source tree is not on disk.
 */
function assertNapiBinaryFreshness(resolvedNodePath: string): void {
  if (process.env['NODE_ENV'] === 'production') return;
  if (process.env['ZQLITE_RS_SKIP_FRESHNESS_CHECK'] === '1') return;
  let nodeMtime: number;
  try {
    nodeMtime = statSync(resolvedNodePath).mtimeMs;
  } catch {
    return; // binary not on disk → existing load path will report
  }
  // The resolved `.node` lives at e.g. `packages/zqlite-rs/zqlite-rs.darwin-arm64.node`;
  // its sibling `src/` directory holds the Rust source that compiled it.
  const srcDir = join(dirname(resolvedNodePath), 'src');
  let newestSrcMtime = 0;
  let newestSrcFile = '';
  try {
    for (const entry of readdirSync(srcDir, {withFileTypes: true})) {
      if (!entry.isFile()) continue;
      if (!entry.name.endsWith('.rs')) continue;
      const srcPath = join(srcDir, entry.name);
      const m = statSync(srcPath).mtimeMs;
      if (m > newestSrcMtime) {
        newestSrcMtime = m;
        newestSrcFile = srcPath;
      }
    }
  } catch {
    return; // src dir absent (e.g. installed package, no source tree)
  }
  if (newestSrcMtime === 0) return;
  if (newestSrcMtime > nodeMtime) {
    throw new Error(
      `zqlite-rs napi binary is stale relative to its Rust source.\n` +
        `  binary: ${resolvedNodePath} (mtime ${new Date(nodeMtime).toISOString()})\n` +
        `  source: ${newestSrcFile} (mtime ${new Date(newestSrcMtime).toISOString()})\n` +
        `Rebuild with: (cd packages/zqlite-rs && npm run build)\n` +
        `Or set ZQLITE_RS_SKIP_FRESHNESS_CHECK=1 to bypass (NOT recommended for tests).`,
    );
  }
}

try {
  const esmRequire = createRequire(import.meta.url);
  const resolvedPath = esmRequire.resolve('zqlite-rs');
  // The resolved path points at zqlite-rs/index.js; the actual `.node`
  // file sits next to it. Try to find a sibling .node file matching this
  // platform; if found, gate on its freshness.
  try {
    const pkgDir = dirname(resolvedPath);
    for (const entry of readdirSync(pkgDir)) {
      if (entry.endsWith('.node') && entry.startsWith('zqlite-rs.')) {
        assertNapiBinaryFreshness(join(pkgDir, entry));
        break;
      }
    }
  } catch {
    // Best-effort — if we can't introspect the package directory the load
    // proceeds and the existing fallback warning fires on actual failure.
  }
  const bindings = esmRequire('zqlite-rs');
  RustPipelineManagerClass = bindings?.RustPipelineManager;
} catch (e) {
  // Log so operators know Rust acceleration is unavailable.
  // eslint-disable-next-line no-console
  console.warn(`Failed to load zqlite-rs native bindings: ${e}`);
  RustPipelineManagerClass = undefined;
}

// Rust IVM is the sole execution path for hydration and advance.
// TS IVM pipeline is still built for TableSource (getRow) and companion
// pipelines, but hydration data and advance changes flow exclusively
// through the Rust operator tree.
const RUST_EXISTS_NAME_RE = /:exists\(([^)]+)\)/;

function collectExistsCorrelations(
  condition: Condition | undefined,
  map: Map<string, readonly string[]> = new Map(),
): Map<string, readonly string[]> {
  if (!condition) return map;
  switch (condition.type) {
    case 'correlatedSubquery': {
      const name =
        condition.related.subquery.alias ?? condition.related.subquery.table;
      map.set(name, condition.related.correlation.parentField);
      collectExistsCorrelations(condition.related.subquery.where, map);
      break;
    }
    case 'and':
    case 'or':
      for (const c of condition.conditions) {
        collectExistsCorrelations(c, map);
      }
      break;
    case 'simple':
      break;
  }
  return map;
}

function collectExistsTypes(
  condition: Condition | undefined,
  map: Map<string, 'EXISTS' | 'NOT EXISTS'> = new Map(),
): Map<string, 'EXISTS' | 'NOT EXISTS'> {
  if (!condition) return map;
  switch (condition.type) {
    case 'correlatedSubquery':
      map.set(
        condition.related.subquery.alias ?? condition.related.subquery.table,
        condition.op,
      );
      collectExistsTypes(condition.related.subquery.where, map);
      break;
    case 'and':
    case 'or':
      for (const c of condition.conditions) {
        collectExistsTypes(c, map);
      }
      break;
    case 'simple':
      break;
  }
  return map;
}
import {TableSource} from '../../../../zqlite/src/table-source.ts';
import {
  reloadPermissionsIfChanged,
  type LoadedPermissions,
} from '../../auth/load-permissions.ts';
import type {LogConfig, ZeroConfig} from '../../config/zero-config.ts';
import {computeZqlSpecs, mustGetTableSpec} from '../../db/lite-tables.ts';
import type {LiteAndZqlSpec, LiteTableSpec} from '../../db/specs.ts';

import type {InspectorDelegate} from '../../server/inspector-delegate.ts';
import {type RowKey} from '../../types/row-key.ts';
import {type ShardID} from '../../types/shards.ts';
import {
  getSubscriptionState,
  ZERO_VERSION_COLUMN_NAME,
} from '../replicator/schema/replication-state.ts';
import {checkClientSchema} from './client-schema.ts';
import type {Snapshotter} from './snapshotter.ts';
import {ResetPipelinesSignal} from './snapshotter.ts';

type RowOp<Op extends Omit<ChangeType, ChangeType.CHILD>> = {
  readonly type: Op;
  readonly queryID: string;
  readonly table: string;
  readonly rowKey: Row;
  readonly row: Row;
};

export type RowAdd = RowOp<ChangeType.ADD>;

export type RowRemove = RowOp<ChangeType.REMOVE>;

export type RowEdit = RowOp<ChangeType.EDIT>;

export type RowChange = RowAdd | RowRemove | RowEdit;

type CompanionPipeline = {
  readonly input: Input;
  readonly ast: AST;
  readonly childField: string;
  readonly resolvedValue: LiteralValue | null | undefined;
};

type Pipeline = {
  readonly input: Input;
  readonly hydrationTimeMs: number;
  readonly transformedAst: AST;
  readonly transformationHash: string;
  readonly companions: readonly CompanionPipeline[];
};

type QueryInfo = {
  readonly transformedAst: AST;
  readonly transformationHash: string;
};

type AdvanceContext = {
  readonly timer: Timer;
  readonly totalHydrationTimeMs: number;
  readonly numChanges: number;
  pos: number;
};

type HydrateContext = {
  readonly timer: Timer;
};

export type Timer = {
  elapsedLap: () => number;
  totalElapsed: () => number;
};

/**
 * No matter how fast hydration is, advancement is given at least this long to
 * complete before doing a pipeline reset.
 */
const MIN_ADVANCEMENT_TIME_LIMIT_MS = 50;

/**
 * Manages the state of IVM pipelines for a given ViewSyncer (i.e. client group).
 */
export class PipelineDriver {
  readonly #tables = new Map<string, TableSource>();
  // Query id to pipeline
  readonly #pipelines = new Map<string, Pipeline>();

  readonly #lc: LogContext;
  readonly #snapshotter: Snapshotter;
  readonly #storage: ClientGroupStorage;
  readonly #rustStorage: RustStorage;
  #rustOpID = 0;
  readonly #shardID: ShardID;
  readonly #logConfig: LogConfig;
  readonly #config: ZeroConfig | undefined;
  readonly #tableSpecs = new Map<string, LiteAndZqlSpec>();
  readonly #allTableNames = new Set<string>();
  readonly #costModels: WeakMap<Database, ConnectionCostModel> | undefined;
  readonly #yieldThresholdMs: () => number;

  #hydrateContext: HydrateContext | null = null;
  #advanceContext: AdvanceContext | null = null;
  #replicaVersion: string | null = null;
  #primaryKeys: Map<string, PrimaryKey> | null = null;
  #permissions: LoadedPermissions | null = null;

  readonly #inspectorDelegate: InspectorDelegate;
  readonly #permissionTablesByQuery = new Map<string, Set<string>>();
  #manager: RustPipelineManagerInstance | null = null;
  readonly #instanceId: string;

  constructor(
    lc: LogContext,
    logConfig: LogConfig,
    snapshotter: Snapshotter,
    shardID: ShardID,
    storage: ClientGroupStorage,
    clientGroupID: string,
    inspectorDelegate: InspectorDelegate,
    yieldThresholdMs: () => number,
    enablePlanner?: boolean,
    config?: ZeroConfig,
  ) {
    this.#lc = lc.withContext('clientGroupID', clientGroupID);
    this.#snapshotter = snapshotter;
    this.#storage = storage;
    this.#rustStorage = new RustStorage();
    this.#shardID = shardID;
    this.#logConfig = logConfig;
    this.#config = config;
    this.#inspectorDelegate = inspectorDelegate;
    this.#costModels = enablePlanner ? new WeakMap() : undefined;
    this.#yieldThresholdMs = yieldThresholdMs;
    this.#instanceId = clientGroupID;
  }

  /**
   * Initializes the PipelineDriver to the current head of the database.
   * Queries can then be added (i.e. hydrated) with {@link addQuery()}.
   *
   * Must only be called once.
   */
  init(clientSchema: ClientSchema) {
    assert(!this.#snapshotter.initialized(), 'Already initialized');
    this.#snapshotter.init();
    this.#initAndResetCommon(clientSchema);
  }

  /**
   * @returns Whether the PipelineDriver has been initialized.
   */
  initialized(): boolean {
    return this.#snapshotter.initialized();
  }

  /**
   * Clears the current pipelines and TableSources, returning the PipelineDriver
   * to its initial state. This should be called in response to a schema change,
   * as TableSources need to be recomputed.
   */
  reset(clientSchema: ClientSchema) {
    for (const pipeline of this.#pipelines.values()) {
      pipeline.input.destroy();
      for (const companion of pipeline.companions) {
        companion.input.destroy();
      }
    }
    this.#pipelines.clear();
    this.#tables.clear();
    this.#allTableNames.clear();
    this.#permissionTablesByQuery.clear();
    this.#manager = null;
    this.#initAndResetCommon(clientSchema);
  }

  #initAndResetCommon(clientSchema: ClientSchema) {
    const {db} = this.#snapshotter.current();
    const fullTables = new Map<string, LiteTableSpec>();
    computeZqlSpecs(
      this.#lc,
      db.db,
      {includeBackfillingColumns: false},
      this.#tableSpecs,
      fullTables,
    );
    checkClientSchema(
      this.#shardID,
      clientSchema,
      this.#tableSpecs,
      fullTables,
    );
    this.#allTableNames.clear();
    for (const table of fullTables.keys()) {
      this.#allTableNames.add(table);
    }
    const primaryKeys = this.#primaryKeys ?? new Map<string, PrimaryKey>();
    this.#primaryKeys = primaryKeys;
    primaryKeys.clear();
    for (const [table, spec] of this.#tableSpecs.entries()) {
      primaryKeys.set(table, spec.tableSpec.primaryKey);
    }
    buildPrimaryKeys(clientSchema, primaryKeys);
    const {replicaVersion} = getSubscriptionState(db);
    this.#replicaVersion = replicaVersion;

    // Create the persistent Rust pipeline manager instance eagerly so
    // addQuery/hydrate/advance all share the same operator tree.
    if (RustPipelineManagerClass) {
      this.#manager = new RustPipelineManagerClass();
      this.#manager.createInstance(this.#instanceId, db.db.name);
      this.#setTableSpecsOnManager();
    }
  }

  /**
   * Serialize table specs and all table names to the Rust manager so it
   * can perform diffing (changelog → row lookups → type conversion).
   */
  #setTableSpecsOnManager() {
    if (!this.#manager) return;
    // Convert LiteAndZqlSpec map to the JSON format Rust expects:
    // { "tableName": { "tableSpec": {...}, "zqlSpec": {...} } }
    const syncableTablesObj: Record<string, unknown> = {};
    for (const [name, spec] of this.#tableSpecs.entries()) {
      syncableTablesObj[name] = {
        tableSpec: {
          name: spec.tableSpec.name,
          columns: spec.tableSpec.columns,
          primaryKey: spec.tableSpec.primaryKey,
          uniqueKeys: spec.tableSpec.uniqueKeys,
          minRowVersion: spec.tableSpec.minRowVersion,
          allPotentialPrimaryKeys: spec.tableSpec.allPotentialPrimaryKeys ?? [],
        },
        zqlSpec: {columns: spec.zqlSpec},
      };
    }
    this.#manager.setTableSpecs(
      this.#instanceId,
      JSON.stringify(syncableTablesObj),
      JSON.stringify([...this.#allTableNames]),
    );
  }

  /** @returns The replica version. The PipelineDriver must have been initialized. */
  get replicaVersion(): string {
    return must(this.#replicaVersion, 'Not yet initialized');
  }

  /**
   * Returns the current version of the database. This will reflect the
   * latest version change when calling {@link advance()} once the
   * iteration has begun.
   */
  currentVersion(): string {
    assert(this.initialized(), 'Not yet initialized');
    return this.#snapshotter.current().version;
  }

  /**
   * Returns the current upstream {app}.permissions, or `null` if none are defined.
   */
  currentPermissions(): LoadedPermissions | null {
    assert(this.initialized(), 'Not yet initialized');
    const res = reloadPermissionsIfChanged(
      this.#lc,
      this.#snapshotter.current().db,
      this.#shardID.appID,
      this.#permissions,
      this.#config,
    );
    if (res.changed) {
      this.#permissions = res.permissions;
      this.#lc.debug?.(
        'Reloaded permissions',
        JSON.stringify(this.#permissions),
      );
    }
    return this.#permissions;
  }

  advanceWithoutDiff(): string {
    const {db, version} = this.#snapshotter.advanceWithoutDiff().curr;
    for (const table of this.#tables.values()) {
      table.setDB(db.db);
    }
    this.#manager?.swapSnapshot(this.#instanceId, db.db.name);
    return version;
  }

  #ensureCostModelExistsIfEnabled(db: Database) {
    let existing = this.#costModels?.get(db);
    if (existing) {
      return existing;
    }
    if (this.#costModels) {
      const costModel = createSQLiteCostModel(db, this.#tableSpecs);
      this.#costModels.set(db, costModel);
      return costModel;
    }
    return undefined;
  }

  /**
   * Clears storage used for the pipelines. Call this when the
   * PipelineDriver will no longer be used.
   */
  destroy() {
    this.#storage.destroy();
    this.#snapshotter.destroy();
  }

  /** @return Map from query ID to PipelineInfo for all added queries. */
  queries(): ReadonlyMap<string, QueryInfo> {
    return this.#pipelines;
  }

  totalHydrationTimeMs(): number {
    let total = 0;
    for (const pipeline of this.#pipelines.values()) {
      total += pipeline.hydrationTimeMs;
    }
    return total;
  }

  #resolveScalarSubqueries(ast: AST): {
    ast: AST;
    companionRows: {table: string; row: Row}[];
    companions: CompanionSubquery[];
    companionInputs: Input[];
  } {
    const companionRows: {table: string; row: Row}[] = [];
    const companionInputs: Input[] = [];

    const executor = (
      subqueryAST: AST,
      childField: string,
    ): LiteralValue | null | undefined => {
      const input = buildPipeline(
        subqueryAST,
        {
          getSource: name => this.#getSource(name),
          createStorage: () => this.#createStorage(),
          decorateSourceInput: (input: SourceInput): Input => input,
          decorateInput: input => input,
          addEdge() {},
          decorateFilterInput: input => input,
        },
        'scalar-subquery',
      );
      // Consume the full stream rather than using first() to avoid
      // triggering early return on Take's #initialFetch assertion.
      // The subquery AST already has limit: 1, so at most one row is produced.
      let node: Node | undefined;
      for (const n of skipYields(input.fetch({}))) {
        node ??= n;
      }
      if (!node) {
        // Keep the companion alive even with no results — it will
        // detect a future insert that creates the row.
        companionInputs.push(input);
        return undefined;
      }
      companionRows.push({table: subqueryAST.table, row: node.row as Row});
      companionInputs.push(input);
      return (node.row[childField] as LiteralValue) ?? null;
    };

    const {ast: resolved, companions} = resolveSimpleScalarSubqueries(
      ast,
      this.#tableSpecs,
      executor,
    );
    return {ast: resolved, companionRows, companions, companionInputs};
  }

  /**
   * Adds a pipeline for the query. The method will hydrate the query using the
   * driver's current snapshot of the database and return a stream of results.
   * Henceforth, updates to the query will be returned when the driver is
   * {@link advance}d. The query and its pipeline can be removed with
   * {@link removeQuery()}.
   *
   * If a query with the same queryID is already added, the existing pipeline
   * will be removed and destroyed before adding the new pipeline.
   *
   * @param timer The caller-controlled {@link Timer} used to determine the
   *        final hydration time. (The caller may pause and resume the timer
   *        when yielding the thread for time-slicing).
   * @return The rows from the initial hydration of the query.
   */
  *addQuery(
    transformationHash: string,
    queryID: string,
    query: AST,
    timer: Timer,
  ): Iterable<RowChange | 'yield'> {
    assert(
      this.initialized(),
      'Pipeline driver must be initialized before adding queries',
    );
    this.removeQuery(queryID);
    const debugDelegate = runtimeDebugFlags.trackRowsVended
      ? new Debug()
      : undefined;

    const costModel = this.#ensureCostModelExistsIfEnabled(
      this.#snapshotter.current().db.db,
    );

    assert(
      this.#advanceContext === null,
      'Cannot hydrate while advance is in progress',
    );
    this.#hydrateContext = {
      timer,
    };
    try {
      const {
        ast: resolvedQuery,
        companionRows,
        companions: companionMeta,
        companionInputs,
      } = this.#resolveScalarSubqueries(query);

      const existsTypes = collectExistsTypes(resolvedQuery.where);
      const existsCorrelations = collectExistsCorrelations(resolvedQuery.where);

      const input = buildPipeline(
        resolvedQuery,
        {
          debug: debugDelegate,
          enableNotExists: true, // Server-side can handle NOT EXISTS
          getSource: name => this.#getSource(name),
          createStorage: () => this.#createStorage(),
          decorateSourceInput: (input: SourceInput, _queryID: string): Input =>
            new MeasurePushOperator(
              input,
              queryID,
              this.#inspectorDelegate,
              'query-update-server',
            ),
          decorateInput: input => input,
          addEdge() {},
          decorateFilterInput: (input, name) => {
            if (name.includes(':exists(')) {
              const match = name.match(RUST_EXISTS_NAME_RE);
              if (match) {
                const relationshipName = match[1];
                const parentField = existsCorrelations.get(relationshipName);
                if (parentField) {
                  return createRustExistsWrapper(
                    input as FilterOperator,
                    relationshipName,
                    parentField as CompoundKey,
                    existsTypes.get(relationshipName) ?? 'EXISTS',
                  );
                }
              }
            }
            return input;
          },
        },
        queryID,
        costModel,
      );
      input.setOutput({
        push: () => [],
      });

      // Hydration flows exclusively through the Rust operator tree.
      yield* this.#rustHydrateQuery(queryID, resolvedQuery);

      for (const {table, row} of companionRows) {
        const primaryKey = mustGetPrimaryKey(this.#primaryKeys, table);
        yield {
          type: ChangeType.ADD,
          queryID,
          table,
          rowKey: getRowKey(primaryKey, row),
          row,
        } as RowChange;
      }

      const hydrationTimeMs = timer.totalElapsed();
      if (runtimeDebugFlags.trackRowCountsVended) {
        if (hydrationTimeMs > this.#logConfig.slowHydrateThreshold) {
          let totalRowsConsidered = 0;
          const lc = this.#lc
            .withContext('queryID', queryID)
            .withContext('hydrationTimeMs', hydrationTimeMs);
          for (const tableName of this.#tables.keys()) {
            const entries = Object.entries(
              debugDelegate?.getVendedRowCounts()[tableName] ?? {},
            );
            totalRowsConsidered += entries.reduce(
              (acc, entry) => acc + entry[1],
              0,
            );
            lc.info?.(tableName + ' VENDED: ', entries);
          }
          lc.info?.(`Total rows considered: ${totalRowsConsidered}`);
        }
      }
      debugDelegate?.reset();

      // Set up live companion pipelines for reactive scalar subquery monitoring.
      const liveCompanions: CompanionPipeline[] = [];
      for (let i = 0; i < companionMeta.length; i++) {
        const meta = companionMeta[i];
        const companionInput = companionInputs[i];
        const {childField, resolvedValue} = meta;
        companionInput.setOutput({push: () => []});
        liveCompanions.push({
          input: companionInput,
          ast: meta.ast,
          childField,
          resolvedValue,
        });
      }

      // Note: This hydrationTime is a wall-clock overestimate, as it does
      // not take time slicing into account.
      this.#pipelines.set(queryID, {
        input,
        hydrationTimeMs,
        transformedAst: resolvedQuery,
        transformationHash,
        companions: liveCompanions,
      });

      const permTables = collectPermissionTables(resolvedQuery);
      if (permTables.size > 0) {
        this.#permissionTablesByQuery.set(queryID, permTables);
      } else {
        this.#permissionTablesByQuery.delete(queryID);
      }

      // Pass companion metadata to Rust for advance-time scalar checks.
      if (this.#manager && liveCompanions.length > 0) {
        const companionInfos = liveCompanions.map(c => {
          const conditions: Array<{column: string; value: LiteralValue}> = [];
          collectLiteralConditions(c.ast.where, conditions);
          const pk = this.#primaryKeys?.get(c.ast.table) ?? [];
          return {
            queryId: queryID,
            table: c.ast.table,
            childField: c.childField,
            resolvedValue: c.resolvedValue ?? null,
            whereConditions: conditions,
            primaryKey: pk,
          };
        });
        this.#manager.setQueryCompanions(
          this.#instanceId,
          queryID,
          JSON.stringify(companionInfos),
        );
      }
    } finally {
      this.#hydrateContext = null;
    }
  }

  /**
   * Async version of addQueries() — offloads Rust batch hydration to a
   * libuv worker thread so multiple client groups can hydrate in parallel.
   * Returns a materialized array (not a generator) since we can't yield
   * from an async method.
   */
  async addQueriesAsync(
    queries: ReadonlyArray<{
      readonly transformationHash: string;
      readonly queryID: string;
      readonly ast: AST;
    }>,
    timer: Timer,
  ): Promise<Iterable<RowChange | 'yield'>> {
    if (queries.length === 0) {
      return [];
    }

    assert(
      this.initialized(),
      'Pipeline driver must be initialized before adding queries',
    );
    assert(
      this.#advanceContext === null,
      'Cannot hydrate while advance is in progress',
    );

    // Phase 1: Build all TS pipelines and collect Rust hydration requests.
    type PreparedQuery = {
      queryID: string;
      transformationHash: string;
      resolvedQuery: AST;
      companionRows: {table: string; row: Row}[];
      companionMeta: CompanionSubquery[];
      companionInputs: Input[];
      existsTypes: Map<string, 'EXISTS' | 'NOT EXISTS'>;
      existsCorrelations: Map<string, readonly string[]>;
      input: Input;
      debugDelegate: Debug | undefined;
      rustEligible: boolean;
    };

    const prepared: PreparedQuery[] = [];
    const rustPayloads: Array<{
      query_id: string;
      ast: AST;
      primary_key: string[];
      column_types?: Record<string, Record<string, string>>;
      all_primary_keys?: Record<string, string[]>;
    }> = [];

    const costModel = this.#ensureCostModelExistsIfEnabled(
      this.#snapshotter.current().db.db,
    );

    for (const q of queries) {
      this.removeQuery(q.queryID);
      const debugDelegate = runtimeDebugFlags.trackRowsVended
        ? new Debug()
        : undefined;

      const {
        ast: resolvedQuery,
        companionRows,
        companions: companionMeta,
        companionInputs,
      } = this.#resolveScalarSubqueries(q.ast);

      const existsTypes = collectExistsTypes(resolvedQuery.where);
      const existsCorrelations = collectExistsCorrelations(resolvedQuery.where);

      const input = buildPipeline(
        resolvedQuery,
        {
          debug: debugDelegate,
          enableNotExists: true,
          getSource: name => this.#getSource(name),
          createStorage: () => this.#createStorage(),
          decorateSourceInput: (input: SourceInput, _queryID: string): Input =>
            new MeasurePushOperator(
              input,
              q.queryID,
              this.#inspectorDelegate,
              'query-update-server',
            ),
          decorateInput: input => input,
          addEdge() {},
          decorateFilterInput: (input, name) => {
            if (name.includes(':exists(')) {
              const match = name.match(RUST_EXISTS_NAME_RE);
              if (match) {
                const relationshipName = match[1];
                const parentField = existsCorrelations.get(relationshipName);
                if (parentField) {
                  return createRustExistsWrapper(
                    input as FilterOperator,
                    relationshipName,
                    parentField as CompoundKey,
                    existsTypes.get(relationshipName) ?? 'EXISTS',
                  );
                }
              }
            }
            return input;
          },
        },
        q.queryID,
        costModel,
      );
      input.setOutput({
        push: () => [],
      });

      const tableName = resolvedQuery.table ?? '';
      const pk = this.#primaryKeys?.get(tableName) ?? [];
      const rustEligible = this.#manager !== null && companionMeta.length === 0;
      if (rustEligible) {
        rustPayloads.push({
          query_id: q.queryID,
          ast: resolvedQuery,
          primary_key: [...pk],
          column_types: this.#collectColumnTypes(resolvedQuery),
          all_primary_keys: this.#collectAllPrimaryKeys(resolvedQuery),
        });
      }

      prepared.push({
        queryID: q.queryID,
        transformationHash: q.transformationHash,
        resolvedQuery,
        companionRows,
        companionMeta,
        companionInputs,
        existsTypes,
        existsCorrelations,
        input,
        debugDelegate,
        rustEligible,
      });
    }

    // Phase 2: Add queries and batch hydrate — ASYNC via libuv worker thread.
    let changesByQuery: Map<string, DecodedRowChange[]> | undefined;
    if (rustPayloads.length > 0) {
      assert(this.#manager, 'RustPipelineManager not available');
      for (const payload of rustPayloads) {
        this.#manager.addQuery(this.#instanceId, JSON.stringify(payload));
      }
      const resultBuf = await this.#manager.hydrateAsync(this.#instanceId);
      const decoded = decodeAdvanceResultBuf(resultBuf);
      if (decoded.error) {
        throw new Error(`Rust batch hydration failed: ${decoded.error}`);
      }
      changesByQuery = new Map();
      for (const change of decoded.changes) {
        let arr = changesByQuery.get(change.queryID);
        if (!arr) {
          arr = [];
          changesByQuery.set(change.queryID, arr);
        }
        arr.push(change);
      }
    }

    // Phase 3: Materialize results and finalize pipelines.
    const allChanges: (RowChange | 'yield')[] = [];
    let lastTotalElapsed = 0;
    for (const p of prepared) {
      this.#hydrateContext = {timer};
      try {
        if (p.rustEligible && changesByQuery) {
          const changes = changesByQuery.get(p.queryID) ?? [];
          const permTables = collectPermissionTables(p.resolvedQuery);
          for (const change of this.#convertDecodedChanges(
            p.queryID,
            changes,
            permTables.size > 0 ? permTables : undefined,
          )) {
            allChanges.push(change);
          }
        } else {
          // Companion queries can't go through Rust batch; hydrate via TS.
          for (const change of hydrateInternal(
            p.input,
            p.queryID,
            must(this.#primaryKeys),
            this.#tableSpecs,
          )) {
            allChanges.push(change);
          }
        }

        for (const {table, row} of p.companionRows) {
          const primaryKey = mustGetPrimaryKey(this.#primaryKeys, table);
          allChanges.push({
            type: ChangeType.ADD,
            queryID: p.queryID,
            table,
            rowKey: getRowKey(primaryKey, row),
            row,
          } as RowChange);
        }

        const currentTotal = timer.totalElapsed();
        const hydrationTimeMs = currentTotal - lastTotalElapsed;
        lastTotalElapsed = currentTotal;
        p.debugDelegate?.reset();

        // Set up live companion pipelines for reactive scalar subquery monitoring.
        const liveCompanions: CompanionPipeline[] = [];
        for (let i = 0; i < p.companionMeta.length; i++) {
          const meta = p.companionMeta[i];
          const companionInput = p.companionInputs[i];
          const {childField, resolvedValue} = meta;
          companionInput.setOutput({push: () => []});
          liveCompanions.push({
            input: companionInput,
            ast: meta.ast,
            childField,
            resolvedValue,
          });
        }

        this.#pipelines.set(p.queryID, {
          input: p.input,
          hydrationTimeMs,
          transformedAst: p.resolvedQuery,
          transformationHash: p.transformationHash,
          companions: liveCompanions,
        });

        const permTables = collectPermissionTables(p.resolvedQuery);
        if (permTables.size > 0) {
          this.#permissionTablesByQuery.set(p.queryID, permTables);
        } else {
          this.#permissionTablesByQuery.delete(p.queryID);
        }
      } finally {
        this.#hydrateContext = null;
      }
    }

    return allChanges;
  }

  /**
   * Streaming version of {@link addQueriesAsync}. Returns an
   * `AsyncIterable<RowChange | 'yield'>` that drains each pipeline's chunk
   * from Rust's streaming hydrate as soon as it completes.
   *
   * Companion-bearing queries (the same ones `addQueriesAsync` excludes
   * from its Rust batch — `companionMeta.length > 0`) fall back to TS
   * hydrate via {@link hydrateInternal}. Per RESEARCH Open Q #4 (drain
   * TS-hydrate first), the iterable yields all companion-bearing query
   * rows BEFORE starting the Rust streaming hydrate. This preserves the
   * existing addQueriesAsync ordering while delivering Rust-eligible rows
   * via the streaming path.
   *
   * Errors:
   *   - `StreamItem::Error(msg, kind)` → `throw new RustStreamError(msg, kind)`.
   *   - hydrateStreaming does NOT emit `StreamItem::ResetSignal` (companions
   *     only run on advance — see RESEARCH §"Per-pipeline...").
   *
   * Note: this method is alongside (not replacing) `addQueriesAsync`.
   */
  async addQueriesStreaming(
    queries: ReadonlyArray<{
      readonly transformationHash: string;
      readonly queryID: string;
      readonly ast: AST;
    }>,
    timer: Timer,
  ): Promise<AsyncIterable<RowChange | 'yield'>> {
    if (queries.length === 0) {
      return (async function* empty() {})();
    }

    assert(
      this.initialized(),
      'Pipeline driver must be initialized before adding queries',
    );
    assert(
      this.#advanceContext === null,
      'Cannot hydrate while advance is in progress',
    );

    // Phase 1: prepare all queries (mirrors addQueriesAsync's Phase 1
    // structure). Build TS pipelines, classify rust-eligible vs companion,
    // collect Rust hydrate payloads.
    type PreparedQuery = {
      queryID: string;
      transformationHash: string;
      resolvedQuery: AST;
      companionRows: {table: string; row: Row}[];
      companionMeta: CompanionSubquery[];
      companionInputs: Input[];
      existsTypes: Map<string, 'EXISTS' | 'NOT EXISTS'>;
      existsCorrelations: Map<string, readonly string[]>;
      input: Input;
      debugDelegate: Debug | undefined;
      rustEligible: boolean;
    };

    const prepared: PreparedQuery[] = [];
    const rustPayloads: Array<{
      query_id: string;
      ast: AST;
      primary_key: string[];
      column_types?: Record<string, Record<string, string>>;
      all_primary_keys?: Record<string, string[]>;
    }> = [];

    const costModel = this.#ensureCostModelExistsIfEnabled(
      this.#snapshotter.current().db.db,
    );

    for (const q of queries) {
      this.removeQuery(q.queryID);
      const debugDelegate = runtimeDebugFlags.trackRowsVended
        ? new Debug()
        : undefined;

      const {
        ast: resolvedQuery,
        companionRows,
        companions: companionMeta,
        companionInputs,
      } = this.#resolveScalarSubqueries(q.ast);

      const existsTypes = collectExistsTypes(resolvedQuery.where);
      const existsCorrelations = collectExistsCorrelations(resolvedQuery.where);

      const input = buildPipeline(
        resolvedQuery,
        {
          debug: debugDelegate,
          enableNotExists: true,
          getSource: name => this.#getSource(name),
          createStorage: () => this.#createStorage(),
          decorateSourceInput: (
            sourceInput: SourceInput,
            _queryID: string,
          ): Input =>
            new MeasurePushOperator(
              sourceInput,
              q.queryID,
              this.#inspectorDelegate,
              'query-update-server',
            ),
          decorateInput: filterInput => filterInput,
          addEdge() {},
          decorateFilterInput: (filterInput, name) => {
            if (name.includes(':exists(')) {
              const match = name.match(RUST_EXISTS_NAME_RE);
              if (match) {
                const relationshipName = match[1];
                const parentField = existsCorrelations.get(relationshipName);
                if (parentField) {
                  return createRustExistsWrapper(
                    filterInput as FilterOperator,
                    relationshipName,
                    parentField as CompoundKey,
                    existsTypes.get(relationshipName) ?? 'EXISTS',
                  );
                }
              }
            }
            return filterInput;
          },
        },
        q.queryID,
        costModel,
      );
      input.setOutput({
        push: () => [],
      });

      const tableName = resolvedQuery.table ?? '';
      const pk = this.#primaryKeys?.get(tableName) ?? [];
      const rustEligible = this.#manager !== null && companionMeta.length === 0;
      if (rustEligible) {
        rustPayloads.push({
          query_id: q.queryID,
          ast: resolvedQuery,
          primary_key: [...pk],
          column_types: this.#collectColumnTypes(resolvedQuery),
          all_primary_keys: this.#collectAllPrimaryKeys(resolvedQuery),
        });
      }

      prepared.push({
        queryID: q.queryID,
        transformationHash: q.transformationHash,
        resolvedQuery,
        companionRows,
        companionMeta,
        companionInputs,
        existsTypes,
        existsCorrelations,
        input,
        debugDelegate,
        rustEligible,
      });
    }

    // Phase 2: register rust-eligible queries on the manager so the
    // streaming hydrate sees them. (Done synchronously here; the actual
    // hydrateStreaming call happens lazily inside the async generator
    // returned below so the caller can `for await` immediately.)
    if (rustPayloads.length > 0) {
      assert(this.#manager, 'RustPipelineManager not available');
      for (const payload of rustPayloads) {
        this.#manager.addQuery(this.#instanceId, JSON.stringify(payload));
      }
    }

    return this.#streamAddQueries(prepared, timer);
  }

  /**
   * Materialize the prepared queries: yield TS-hydrate (companion-bearing)
   * first, then drain Rust streaming hydrate, then finalize pipelines.
   *
   * Per RESEARCH Open Q #4: TS-first ordering preserves the existing
   * addQueriesAsync semantics. Companion rows for each query are emitted
   * after that query's main result set (mirrors addQueriesAsync's
   * per-query loop body).
   */
  async *#streamAddQueries(
    prepared: ReadonlyArray<{
      queryID: string;
      transformationHash: string;
      resolvedQuery: AST;
      companionRows: {table: string; row: Row}[];
      companionMeta: CompanionSubquery[];
      companionInputs: Input[];
      existsTypes: Map<string, 'EXISTS' | 'NOT EXISTS'>;
      existsCorrelations: Map<string, readonly string[]>;
      input: Input;
      debugDelegate: Debug | undefined;
      rustEligible: boolean;
    }>,
    timer: Timer,
  ): AsyncIterable<RowChange | 'yield'> {
    let lastTotalElapsed = 0;

    // Per-query bookkeeping helper (mirror of addQueriesAsync's per-query
    // tail-end work). Runs after each query's main result set + companion
    // rows have been yielded.
    const finalizeQuery = (p: (typeof prepared)[number]) => {
      for (const {table, row} of p.companionRows) {
        const primaryKey = mustGetPrimaryKey(this.#primaryKeys, table);
        // Companion rows are appended via the iterator itself (see below);
        // this helper only handles the post-emit pipeline state mutation.
        void primaryKey;
        void row;
      }

      const currentTotal = timer.totalElapsed();
      const hydrationTimeMs = currentTotal - lastTotalElapsed;
      lastTotalElapsed = currentTotal;
      p.debugDelegate?.reset();

      const liveCompanions: CompanionPipeline[] = [];
      for (let i = 0; i < p.companionMeta.length; i++) {
        const meta = p.companionMeta[i];
        const companionInput = p.companionInputs[i];
        const {childField, resolvedValue} = meta;
        companionInput.setOutput({push: () => []});
        liveCompanions.push({
          input: companionInput,
          ast: meta.ast,
          childField,
          resolvedValue,
        });
      }

      this.#pipelines.set(p.queryID, {
        input: p.input,
        hydrationTimeMs,
        transformedAst: p.resolvedQuery,
        transformationHash: p.transformationHash,
        companions: liveCompanions,
      });

      const permTables = collectPermissionTables(p.resolvedQuery);
      if (permTables.size > 0) {
        this.#permissionTablesByQuery.set(p.queryID, permTables);
      } else {
        this.#permissionTablesByQuery.delete(p.queryID);
      }

      if (this.#manager && liveCompanions.length > 0) {
        const companionsJson = JSON.stringify(
          liveCompanions.map(c => ({
            queryId: p.queryID,
            table: c.ast.table,
            childField: c.childField,
            resolvedValue: c.resolvedValue,
            whereConditions: c.ast.where,
            primaryKey:
              this.#primaryKeys?.get(c.ast.table) ?? mustGetPrimaryKey(
                this.#primaryKeys,
                c.ast.table,
              ),
          })),
        );
        this.#manager.setQueryCompanions(
          this.#instanceId,
          p.queryID,
          companionsJson,
        );
      }
    };

    // Phase 3a: drain TS-hydrate FIRST for companion-bearing queries
    // (RESEARCH Open Q #4).
    for (const p of prepared) {
      if (p.rustEligible) continue;
      this.#hydrateContext = {timer};
      try {
        for (const change of hydrateInternal(
          p.input,
          p.queryID,
          must(this.#primaryKeys),
          this.#tableSpecs,
        )) {
          yield change;
        }
        for (const {table, row} of p.companionRows) {
          const primaryKey = mustGetPrimaryKey(this.#primaryKeys, table);
          yield {
            type: ChangeType.ADD,
            queryID: p.queryID,
            table,
            rowKey: getRowKey(primaryKey, row),
            row,
          } as RowChange;
        }
        finalizeQuery(p);
      } finally {
        this.#hydrateContext = null;
      }
    }

    // Phase 3b: drain Rust streaming hydrate for rust-eligible queries.
    const rustQueries = prepared.filter(p => p.rustEligible);
    if (rustQueries.length === 0) return;

    assert(this.#manager, 'RustPipelineManager not available');
    const stream = this.#manager.hydrateStreaming(this.#instanceId);

    // Bucket decoded changes per queryID so each prepared query can be
    // finalized once its rows are fully drained from the stream.
    const changesByQuery = new Map<string, DecodedRowChange[]>();
    try {
      while (true) {
        const item = await stream.next();
        if (item.done) break;
        switch (item.kind) {
          case 'error':
            throw new RustStreamError(
              item.errorMsg ?? '',
              (item.errorKind ?? 'panic') as
                | 'panic'
                | 'rayon_error'
                | 'channel_closed',
            );
          case 'chunk': {
            assert(
              item.chunk,
              'StreamItem::Chunk must carry a non-null buffer',
            );
            const decoded = decodeAdvanceChunkBuf(item.chunk);
            for (const change of decoded) {
              let arr = changesByQuery.get(change.queryID);
              if (!arr) {
                arr = [];
                changesByQuery.set(change.queryID, arr);
              }
              arr.push(change);
            }
            break;
          }
          // hydrateStreaming does not emit 'reset' (companions only run
          // on advance, not hydrate — see chunk_encoder + 31-01 SUMMARY).
          default:
            break;
        }
      }
    } finally {
      // D-15: belt-and-suspenders. The JS method is `stream.return()`
      // — alias for what Rust calls `stream.return_()`. Drop on the Rust
      // side also fires when the HydrateStream is GC'd.
      try {
        stream.return();
      } catch {
        /* swallow — Drop handles the GC path */
      }
    }

    // Phase 3c: yield Rust-eligible query results in `queries` order so
    // the iterable's overall sequence is stable.
    for (const p of rustQueries) {
      this.#hydrateContext = {timer};
      try {
        const changes = changesByQuery.get(p.queryID) ?? [];
        const permTables = collectPermissionTables(p.resolvedQuery);
        for (const change of this.#convertDecodedChanges(
          p.queryID,
          changes,
          permTables.size > 0 ? permTables : undefined,
        )) {
          yield change;
        }
        for (const {table, row} of p.companionRows) {
          const primaryKey = mustGetPrimaryKey(this.#primaryKeys, table);
          yield {
            type: ChangeType.ADD,
            queryID: p.queryID,
            table,
            rowKey: getRowKey(primaryKey, row),
            row,
          } as RowChange;
        }
        finalizeQuery(p);
      } finally {
        this.#hydrateContext = null;
      }
    }
  }

  /**
   * Batch-hydrate multiple queries in a single Rust NAPI call, enabling
   * Rayon parallelism across all pipelines. Falls back to sequential
   * {@link addQuery} when Rust hydration is unavailable or for queries
   * with scalar subquery companions.
   *
   * @param queries The queries to hydrate.
   * @param timer   Shared timer for the batch.
   * @return The rows from the initial hydration of all queries.
   */
  *addQueries(
    queries: ReadonlyArray<{
      readonly transformationHash: string;
      readonly queryID: string;
      readonly ast: AST;
    }>,
    timer: Timer,
  ): Iterable<RowChange | 'yield'> {
    if (queries.length === 0) {
      return;
    }

    assert(
      this.initialized(),
      'Pipeline driver must be initialized before adding queries',
    );
    assert(
      this.#advanceContext === null,
      'Cannot hydrate while advance is in progress',
    );

    // Phase 1: Build all TS pipelines and collect Rust hydration requests.
    type PreparedQuery = {
      queryID: string;
      transformationHash: string;
      resolvedQuery: AST;
      companionRows: {table: string; row: Row}[];
      companionMeta: CompanionSubquery[];
      companionInputs: Input[];
      existsTypes: Map<string, 'EXISTS' | 'NOT EXISTS'>;
      existsCorrelations: Map<string, readonly string[]>;
      input: Input;
      debugDelegate: Debug | undefined;
      rustEligible: boolean;
    };

    const prepared: PreparedQuery[] = [];
    const rustPayloads: Array<{
      query_id: string;
      ast: AST;
      primary_key: string[];
      column_types?: Record<string, Record<string, string>>;
      all_primary_keys?: Record<string, string[]>;
    }> = [];

    const costModel = this.#ensureCostModelExistsIfEnabled(
      this.#snapshotter.current().db.db,
    );

    for (const q of queries) {
      this.removeQuery(q.queryID);
      const debugDelegate = runtimeDebugFlags.trackRowsVended
        ? new Debug()
        : undefined;

      const {
        ast: resolvedQuery,
        companionRows,
        companions: companionMeta,
        companionInputs,
      } = this.#resolveScalarSubqueries(q.ast);

      const existsTypes = collectExistsTypes(resolvedQuery.where);
      const existsCorrelations = collectExistsCorrelations(resolvedQuery.where);

      const input = buildPipeline(
        resolvedQuery,
        {
          debug: debugDelegate,
          enableNotExists: true,
          getSource: name => this.#getSource(name),
          createStorage: () => this.#createStorage(),
          decorateSourceInput: (input: SourceInput, _queryID: string): Input =>
            new MeasurePushOperator(
              input,
              q.queryID,
              this.#inspectorDelegate,
              'query-update-server',
            ),
          decorateInput: input => input,
          addEdge() {},
          decorateFilterInput: (input, name) => {
            if (name.includes(':exists(')) {
              const match = name.match(RUST_EXISTS_NAME_RE);
              if (match) {
                const relationshipName = match[1];
                const parentField = existsCorrelations.get(relationshipName);
                if (parentField) {
                  return createRustExistsWrapper(
                    input as FilterOperator,
                    relationshipName,
                    parentField as CompoundKey,
                    existsTypes.get(relationshipName) ?? 'EXISTS',
                  );
                }
              }
            }
            return input;
          },
        },
        q.queryID,
        costModel,
      );
      input.setOutput({
        push: () => [],
      });

      const tableName = resolvedQuery.table ?? '';
      const pk = this.#primaryKeys?.get(tableName) ?? [];
      const rustEligible = this.#manager !== null && companionMeta.length === 0;
      if (rustEligible) {
        rustPayloads.push({
          query_id: q.queryID,
          ast: resolvedQuery,
          primary_key: [...pk],
          column_types: this.#collectColumnTypes(resolvedQuery),
          all_primary_keys: this.#collectAllPrimaryKeys(resolvedQuery),
        });
      }

      prepared.push({
        queryID: q.queryID,
        transformationHash: q.transformationHash,
        resolvedQuery,
        companionRows,
        companionMeta,
        companionInputs,
        existsTypes,
        existsCorrelations,
        input,
        debugDelegate,
        rustEligible,
      });
    }

    // Phase 2: Add queries to persistent pipeline and batch hydrate.
    let changesByQuery: Map<string, DecodedRowChange[]> | undefined;
    if (rustPayloads.length > 0) {
      assert(this.#manager, 'RustPipelineManager not available');
      for (const payload of rustPayloads) {
        this.#manager.addQuery(this.#instanceId, JSON.stringify(payload));
      }
      const resultBuf = this.#manager.hydrate(this.#instanceId);
      const decoded = decodeAdvanceResultBuf(resultBuf);
      if (decoded.error) {
        throw new Error(`Rust batch hydration failed: ${decoded.error}`);
      }
      changesByQuery = new Map();
      for (const change of decoded.changes) {
        let arr = changesByQuery.get(change.queryID);
        if (!arr) {
          arr = [];
          changesByQuery.set(change.queryID, arr);
        }
        arr.push(change);
      }
    }

    // Phase 3: Yield results per-query and finalize pipelines.
    let lastTotalElapsed = 0;
    for (const p of prepared) {
      this.#hydrateContext = {timer};
      try {
        if (p.rustEligible && changesByQuery) {
          const changes = changesByQuery.get(p.queryID) ?? [];
          const permTables = collectPermissionTables(p.resolvedQuery);
          yield* this.#convertDecodedChanges(
            p.queryID,
            changes,
            permTables.size > 0 ? permTables : undefined,
          );
        } else {
          // Companion queries can't go through Rust batch; hydrate via TS.
          yield* hydrateInternal(
            p.input,
            p.queryID,
            must(this.#primaryKeys),
            this.#tableSpecs,
          );
        }

        for (const {table, row} of p.companionRows) {
          const primaryKey = mustGetPrimaryKey(this.#primaryKeys, table);
          yield {
            type: ChangeType.ADD,
            queryID: p.queryID,
            table,
            rowKey: getRowKey(primaryKey, row),
            row,
          } as RowChange;
        }

        const currentTotal = timer.totalElapsed();
        const hydrationTimeMs = currentTotal - lastTotalElapsed;
        lastTotalElapsed = currentTotal;
        p.debugDelegate?.reset();

        // Set up live companion pipelines for reactive scalar subquery monitoring.
        const liveCompanions: CompanionPipeline[] = [];
        for (let i = 0; i < p.companionMeta.length; i++) {
          const meta = p.companionMeta[i];
          const companionInput = p.companionInputs[i];
          const {childField, resolvedValue} = meta;
          companionInput.setOutput({push: () => []});
          liveCompanions.push({
            input: companionInput,
            ast: meta.ast,
            childField,
            resolvedValue,
          });
        }

        this.#pipelines.set(p.queryID, {
          input: p.input,
          hydrationTimeMs,
          transformedAst: p.resolvedQuery,
          transformationHash: p.transformationHash,
          companions: liveCompanions,
        });

        const permTables = collectPermissionTables(p.resolvedQuery);
        if (permTables.size > 0) {
          this.#permissionTablesByQuery.set(p.queryID, permTables);
        } else {
          this.#permissionTablesByQuery.delete(p.queryID);
        }
      } finally {
        this.#hydrateContext = null;
      }
    }
  }

  /**
   * Removes the pipeline for the query. This is a no-op if the query
   * was not added.
   */
  removeQuery(queryID: string) {
    const pipeline = this.#pipelines.get(queryID);
    if (pipeline) {
      this.#pipelines.delete(queryID);
      pipeline.input.destroy();
      for (const companion of pipeline.companions) {
        companion.input.destroy();
      }
    }
    this.#permissionTablesByQuery.delete(queryID);
    this.#manager?.removeQuery(this.#instanceId, queryID);
  }

  /**
   * Returns the value of the row with the given primary key `pk`,
   * or `undefined` if there is no such row. The pipeline must have been
   * initialized.
   */
  getRow(table: string, pk: RowKey): Row | undefined {
    assert(this.initialized(), 'Not yet initialized');
    const source = must(this.#tables.get(table));
    return source.getRow(pk as Row);
  }

  /**
   * Advances to the new head of the database.
   *
   * @param timer The caller-controlled {@link Timer} that will be used to
   *        measure the progress of the advancement and abort with a
   *        {@link ResetPipelinesSignal} if it is estimated to take longer
   *        than a hydration.
   * @return The resulting row changes for all added queries. Note that the
   *         `changes` must be iterated over in their entirety in order to
   *         advance the database snapshot.
   */
  advance(
    timer: Timer,
    _vsId?: string | undefined,
  ): {
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  } {
    assert(
      this.initialized(),
      'Pipeline driver must be initialized before advancing',
    );
    assert(
      this.#manager,
      'RustPipelineManager must be available — Rust is the sole advance path',
    );
    return this.#rustAdvance(timer);
  }

  /**
   * Async version of advance() — offloads IVM fan-out to a libuv worker thread
   * so multiple client groups can advance in parallel across cores.
   * Sync diff computation still happens on the JS thread.
   */
  async advanceAsync(
    timer: Timer,
    _vsId?: string | undefined,
  ): Promise<{
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  }> {
    assert(
      this.initialized(),
      'Pipeline driver must be initialized before advancing',
    );
    assert(
      this.#manager,
      'RustPipelineManager must be available — Rust is the sole advance path',
    );
    return this.#rustAdvanceAsync(timer);
  }

  /**
   * Convert decoded Rust advance changes to typed RowChange objects.
   * Permission filtering and minRowVersion bump are handled by Rust.
   */
  *#convertDispatchChanges(
    changes: DecodedRowChange[],
  ): Iterable<RowChange | 'yield'> {
    for (const change of changes) {
      const type =
        change.type === 'add'
          ? ChangeType.ADD
          : change.type === 'edit'
            ? ChangeType.EDIT
            : ChangeType.REMOVE;
      const row = change.row ?? (change.row_key as Row);
      yield {
        type,
        queryID: change.queryID,
        table: change.table,
        rowKey: change.row_key,
        row: type === ChangeType.REMOVE ? undefined : row,
      } as RowChange;
    }
  }

  /**
   * Streaming version of {@link advanceAsync}. Returns the same `version` /
   * `numChanges` shape, plus an `AsyncIterable<RowChange | 'yield'>` that
   * yields each pipeline's chunk as soon as Rust completes it.
   *
   * Use this instead of `advanceAsync` when you want to begin downstream
   * work (encoding, network send) before all pipelines have finished —
   * peak Rust heap drops from O(total_changes) to O(max_pipeline_changes).
   *
   * Cancellation: `for await { break }` triggers `stream.return()` in the
   * `finally` block (D-15), which flips the Rust cancel flag. Pipelines
   * stop work within one push boundary (STREAM-04 / TEST-05).
   *
   * Errors:
   *   - `StreamItem::ResetSignal` → `throw new ResetPipelinesSignal(reason, 'scalar-subquery')`
   *     (WRAP-04: same throw shape as the buffered `#rustAdvanceAsync` path).
   *   - `StreamItem::Error(msg, kind)` → `throw new RustStreamError(msg, kind)`
   *     (D-10/D-11: kind discriminator preserved verbatim from Rust).
   *
   * Note: this method is alongside (not replacing) `advanceAsync`. The
   * buffered path is preserved for tests/benches per IVM-STREAMING-PLAN.md §8.
   */
  async advanceStreaming(
    timer: Timer,
    _vsId?: string | undefined,
  ): Promise<{
    version: string;
    numChanges: number;
    changes: AsyncIterable<RowChange | 'yield'>;
  }> {
    assert(
      this.initialized(),
      'Pipeline driver must be initialized before advancing',
    );
    assert(
      this.#manager,
      'RustPipelineManager must be available — Rust is the sole advance path',
    );

    // Reuse the existing snapshotter diff path verbatim — same as
    // #rustAdvanceAsync. (TS owns the diff/swap path; Rust cannot open
    // its own BEGIN CONCURRENT connections.)
    const diff = this.#snapshotter.advance(
      this.#tableSpecs,
      this.#allTableNames,
    );
    const {prev, curr, changes: numChanges} = diff;

    this.#lc.debug?.(
      `rust_advance_streaming ${prev.version} => ${curr.version}: ${numChanges} changes, ${this.#pipelines.size} pipelines`,
    );

    const collectedChanges: Array<{
      table: string;
      prevValues: ReadonlyArray<Readonly<Row>>;
      nextValue: Readonly<Row> | null;
      rowKey: unknown;
    }> = [];
    for (const change of diff) {
      collectedChanges.push(change);
    }

    this.#manager.swapSnapshot(this.#instanceId, curr.db.db.name);

    const permTables = this.#combinedPermissionTables();
    if (permTables && permTables.size > 0) {
      this.#manager.setPermissionTables(
        this.#instanceId,
        JSON.stringify([...permTables]),
      );
    }

    for (const table of this.#tables.values()) {
      table.setDB(curr.db.db);
    }
    this.#ensureCostModelExistsIfEnabled(curr.db.db);

    const stream = this.#manager.advanceStreaming(
      this.#instanceId,
      JSON.stringify(collectedChanges),
    );

    return {
      version: curr.version,
      numChanges,
      changes: this.#streamChanges(stream, timer, numChanges),
    };
  }

  /**
   * Drain a Napi stream, decoding chunks via {@link decodeAdvanceChunkBuf} and
   * mapping `StreamItem::ResetSignal` / `StreamItem::Error` to TS throws.
   *
   * D-15 belt-and-suspenders: `try { for await ... } finally { stream.return_() }`
   * (the JS method is named `return` because napi-rs strips the trailing
   * underscore from Rust's `pub fn return_`; comments use the Rust name).
   */
  async *#streamChanges(
    stream: NapiAdvanceStream,
    timer: Timer,
    numChanges: number,
  ): AsyncIterable<RowChange | 'yield'> {
    const totalHydrationTimeMs = this.totalHydrationTimeMs();
    this.#advanceContext = {
      timer,
      totalHydrationTimeMs,
      numChanges,
      pos: 0,
    };
    try {
      while (true) {
        const item = await stream.next();
        if (item.done) break;
        switch (item.kind) {
          case 'reset':
            // WRAP-04: same throw shape as today's buffered #rustAdvanceAsync.
            throw new ResetPipelinesSignal(
              item.reason ?? '',
              'scalar-subquery',
            );
          case 'error':
            // D-10/D-11: Rust→TS error mapping with kind discriminator.
            throw new RustStreamError(
              item.errorMsg ?? '',
              (item.errorKind ?? 'panic') as
                | 'panic'
                | 'rayon_error'
                | 'channel_closed',
            );
          case 'chunk': {
            assert(
              item.chunk,
              'StreamItem::Chunk must carry a non-null buffer',
            );
            const decoded = decodeAdvanceChunkBuf(item.chunk);
            for (const change of this.#convertDispatchChanges(decoded)) {
              if (change !== 'yield') {
                if (this.#shouldAdvanceYieldMaybeAbortAdvance()) {
                  yield 'yield';
                }
                yield change;
                this.#advanceContext!.pos++;
              } else {
                yield change;
              }
            }
            break;
          }
          default:
            // Forward-compat: unknown kinds are ignored (D-11 additive policy).
            break;
        }
      }
    } finally {
      // D-15: belt-and-suspenders cancel. The JS method is `stream.return()`
      // — alias for what Rust calls `stream.return_()`. Drop on the Rust side
      // also fires when the AdvanceStream is GC'd, so even if this throws
      // (it shouldn't) the cancel flag will eventually flip.
      try {
        stream.return();
      } catch {
        /* swallow — Drop handles the GC path */
      }
      this.#advanceContext = null;
    }
  }

  async #rustAdvanceAsync(timer: Timer): Promise<{
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  }> {
    assert(this.#manager, 'RustPipelineManager must be available for advance');

    const diff = this.#snapshotter.advance(
      this.#tableSpecs,
      this.#allTableNames,
    );
    const {prev, curr, changes: numChanges} = diff;

    this.#lc.debug?.(
      `rust_advance_async ${prev.version} => ${curr.version}: ${numChanges} changes, ${this.#pipelines.size} pipelines`,
    );

    const collectedChanges: Array<{
      table: string;
      prevValues: ReadonlyArray<Readonly<Row>>;
      nextValue: Readonly<Row> | null;
      rowKey: unknown;
    }> = [];
    for (const change of diff) {
      collectedChanges.push(change);
    }

    this.#manager.swapSnapshot(this.#instanceId, curr.db.db.name);

    const permTables = this.#combinedPermissionTables();
    if (permTables && permTables.size > 0) {
      this.#manager.setPermissionTables(
        this.#instanceId,
        JSON.stringify([...permTables]),
      );
    }

    const changesJson = JSON.stringify(collectedChanges);
    // Async: offload IVM fan-out to libuv worker thread
    const resultBuf = await this.#manager.advanceAsync(
      this.#instanceId,
      changesJson,
    );
    const decoded = decodeAdvanceResultBuf(resultBuf);
    if (decoded.error) {
      throw new Error(`Rust pipeline advance failed: ${decoded.error}`);
    }

    if (decoded.reset_signal) {
      throw new ResetPipelinesSignal(decoded.reset_signal, 'scalar-subquery');
    }

    const changes = materializeChanges(
      this.#convertDispatchChanges(decoded.changes),
    );

    for (const table of this.#tables.values()) {
      table.setDB(curr.db.db);
    }
    this.#ensureCostModelExistsIfEnabled(curr.db.db);

    this.#lc.debug?.(`Rust advance (async) advanced to ${curr.version}`);

    return {
      version: curr.version,
      numChanges,
      changes: this.#wrapWithTimeout(changes, timer, numChanges),
    };
  }

  #rustAdvance(timer: Timer): {
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  } {
    assert(this.#manager, 'RustPipelineManager must be available for advance');

    // Use TS diff (correct two-snapshot isolation via BEGIN CONCURRENT)
    // then Rust for operator tree fan-out.  Rust cannot open its own
    // connections because both snapshots live on the same WAL2 file and
    // only the TS connections hold the frozen read marks.
    const diff = this.#snapshotter.advance(
      this.#tableSpecs,
      this.#allTableNames,
    );
    const {prev, curr, changes: numChanges} = diff;

    this.#lc.debug?.(
      `rust_advance ${prev.version} => ${curr.version}: ${numChanges} changes, ${this.#pipelines.size} pipelines`,
    );

    // Collect changes from TS diff iterator
    const collectedChanges: Array<{
      table: string;
      prevValues: ReadonlyArray<Readonly<Row>>;
      nextValue: Readonly<Row> | null;
      rowKey: unknown;
    }> = [];
    for (const change of diff) {
      collectedChanges.push(change);
    }

    // Swap manager's snapshot to the new DB path
    this.#manager.swapSnapshot(this.#instanceId, curr.db.db.name);

    // Update permission tables on Rust side before advance
    const permTables = this.#combinedPermissionTables();
    if (permTables && permTables.size > 0) {
      this.#manager.setPermissionTables(
        this.#instanceId,
        JSON.stringify([...permTables]),
      );
    }

    // Push changes through Rust operator trees
    // Rust handles: IVM fan-out, permission filtering, minRowVersion bump,
    // companion scalar checks, and companion row change emission.
    const changesJson = JSON.stringify(collectedChanges);
    const resultBuf = this.#manager.advance(this.#instanceId, changesJson);
    const decoded = decodeAdvanceResultBuf(resultBuf);
    if (decoded.error) {
      throw new Error(`Rust pipeline advance failed: ${decoded.error}`);
    }

    // Handle companion scalar value change — Rust detected the change
    if (decoded.reset_signal) {
      throw new ResetPipelinesSignal(decoded.reset_signal, 'scalar-subquery');
    }

    const changes = materializeChanges(
      this.#convertDispatchChanges(decoded.changes),
    );

    for (const table of this.#tables.values()) {
      table.setDB(curr.db.db);
    }
    this.#ensureCostModelExistsIfEnabled(curr.db.db);

    this.#lc.debug?.(`Rust advance advanced to ${curr.version}`);

    return {
      version: curr.version,
      numChanges,
      changes: this.#wrapWithTimeout(changes, timer, numChanges),
    };
  }

  *#rustHydrateQuery(
    queryID: string,
    resolvedQuery: AST,
  ): Iterable<RowChange | 'yield'> {
    assert(this.#manager, 'RustPipelineManager not available');
    const tableName = resolvedQuery.table ?? '';
    const pk = this.#primaryKeys?.get(tableName) ?? [];
    const queryJson = JSON.stringify({
      query_id: queryID,
      ast: resolvedQuery,
      primary_key: [...pk],
      column_types: this.#collectColumnTypes(resolvedQuery),
      all_primary_keys: this.#collectAllPrimaryKeys(resolvedQuery),
    });
    this.#manager.addQuery(this.#instanceId, queryJson);
    const resultBuf = this.#manager.hydrateQuery(this.#instanceId, queryID);
    const decoded = decodeAdvanceResultBuf(resultBuf);
    if (decoded.error) {
      throw new Error(`Rust hydration failed: ${decoded.error}`);
    }
    const permTables = collectPermissionTables(resolvedQuery);
    yield* this.#convertDecodedChanges(
      queryID,
      decoded.changes,
      permTables.size > 0 ? permTables : undefined,
    );
  }

  /**
   * Build a map of table_name → {column_name → value_type} for all tables
   * referenced by the AST (root, related children, and exists subqueries).
   * This tells Rust which columns to include and how to coerce types.
   */
  #collectColumnTypes(ast: AST): Record<string, Record<string, string>> {
    const result: Record<string, Record<string, string>> = {};
    const addTable = (tableName: string, alias?: string | undefined) => {
      const key = alias || tableName;
      // Skip mutation result tables — TS Streamer suppresses these rows
      if (tableName.includes('.mutations')) {
        return;
      }
      if (!result[key]) {
        const spec = this.#tableSpecs.get(tableName);
        if (spec) {
          const cols: Record<string, string> = {};
          for (const [colName, sv] of Object.entries(spec.zqlSpec)) {
            cols[colName] = sv.type;
          }
          result[key] = cols;
          // Also add by table name if different from key
          if (key !== tableName && !result[tableName]) {
            result[tableName] = cols;
          }
        }
      }
    };
    const visit = (node: AST) => {
      addTable(node.table, node.alias);
      // Walk related subqueries
      if (node.related) {
        for (const rel of node.related) {
          visit(rel.subquery);
        }
      }
      // Walk where conditions for correlated subqueries (EXISTS)
      if (node.where) {
        this.#collectColumnTypesFromCondition(node.where, result);
      }
    };
    visit(ast);
    return result;
  }

  #collectColumnTypesFromCondition(
    cond: Condition,
    result: Record<string, Record<string, string>>,
  ): void {
    if (cond.type === 'and' || cond.type === 'or') {
      for (const sub of cond.conditions) {
        this.#collectColumnTypesFromCondition(sub, result);
      }
    } else if (cond.type === 'correlatedSubquery') {
      const tableName = cond.related.subquery.table;
      if (!result[tableName]) {
        const spec = this.#tableSpecs.get(tableName);
        if (spec) {
          const cols: Record<string, string> = {};
          for (const [colName, sv] of Object.entries(spec.zqlSpec)) {
            cols[colName] = sv.type;
          }
          result[tableName] = cols;
        }
      }
      // Recurse into the subquery
      if (cond.related.subquery.related) {
        for (const rel of cond.related.subquery.related) {
          // Use the visit pattern — but we need to call the outer method
          const subAst = rel.subquery;
          const subResult = this.#collectColumnTypes(subAst);
          Object.assign(result, subResult);
        }
      }
      if (cond.related.subquery.where) {
        this.#collectColumnTypesFromCondition(
          cond.related.subquery.where,
          result,
        );
      }
    }
  }

  /**
   * Build a map of table_name → primary_key columns for all tables
   * referenced by the AST. Uses Zero schema PKs from #primaryKeys,
   * which may differ from SQLite PKs (e.g. issueLabels has Zero PK
   * "legacyID" but SQLite PK "(issueID, labelID)").
   */
  #collectAllPrimaryKeys(ast: AST): Record<string, string[]> {
    const result: Record<string, string[]> = {};
    const visit = (node: AST) => {
      const tableName = node.table;
      if (!result[tableName]) {
        const pk = this.#primaryKeys?.get(tableName);
        if (pk) {
          result[tableName] = [...pk];
        }
      }
      if (node.related) {
        for (const rel of node.related) {
          visit(rel.subquery);
        }
      }
      if (node.where) {
        this.#collectAllPrimaryKeysFromCondition(node.where, result);
      }
    };
    visit(ast);
    return result;
  }

  #collectAllPrimaryKeysFromCondition(
    cond: Condition,
    result: Record<string, string[]>,
  ): void {
    if (cond.type === 'and' || cond.type === 'or') {
      for (const sub of cond.conditions) {
        this.#collectAllPrimaryKeysFromCondition(sub, result);
      }
    } else if (cond.type === 'correlatedSubquery') {
      const tableName = cond.related.subquery.table;
      if (!result[tableName]) {
        const pk = this.#primaryKeys?.get(tableName);
        if (pk) {
          result[tableName] = [...pk];
        }
      }
      // Recurse into subquery's related and where
      if (cond.related.subquery.related) {
        for (const rel of cond.related.subquery.related) {
          const subResult = this.#collectAllPrimaryKeys(rel.subquery);
          Object.assign(result, subResult);
        }
      }
      if (cond.related.subquery.where) {
        this.#collectAllPrimaryKeysFromCondition(
          cond.related.subquery.where,
          result,
        );
      }
    }
  }

  *#convertDecodedChanges(
    queryID: string,
    changes: DecodedRowChange[],
    permissionTables?: Set<string> | undefined,
  ): Iterable<RowChange | 'yield'> {
    for (const change of changes) {
      // Skip rows from permission-system tables (matching Streamer behavior).
      if (permissionTables?.has(change.table)) {
        continue;
      }
      const type =
        change.type === 'add'
          ? ChangeType.ADD
          : change.type === 'edit'
            ? ChangeType.EDIT
            : ChangeType.REMOVE;
      let row = change.row ?? (change.row_key as Row);
      // Apply minRowVersion bump, matching Streamer.#streamNodes behavior.
      if (type !== ChangeType.REMOVE && row) {
        const spec = this.#tableSpecs.get(change.table)?.tableSpec;
        if (spec) {
          const rowVersion = row[ZERO_VERSION_COLUMN_NAME];
          if (
            typeof rowVersion === 'string' &&
            rowVersion < (spec.minRowVersion ?? '00')
          ) {
            row = {...row, [ZERO_VERSION_COLUMN_NAME]: spec.minRowVersion};
          }
        }
      }
      yield {
        type,
        queryID,
        table: change.table,
        rowKey: change.row_key,
        row,
      } as RowChange;
    }
  }

  #combinedPermissionTables(): Set<string> | undefined {
    if (this.#permissionTablesByQuery.size === 0) {
      return undefined;
    }
    const combined = new Set<string>();
    for (const tables of this.#permissionTablesByQuery.values()) {
      for (const t of tables) {
        combined.add(t);
      }
    }
    return combined.size > 0 ? combined : undefined;
  }

  /** Implements `BuilderDelegate.getSource()` */
  #getSource(tableName: string): Source {
    let source = this.#tables.get(tableName);
    if (source) {
      return source;
    }

    const tableSpec = mustGetTableSpec(this.#tableSpecs, tableName);
    const primaryKey = mustGetPrimaryKey(this.#primaryKeys, tableName);

    const {db} = this.#snapshotter.current();
    source = new TableSource(
      this.#lc,
      this.#logConfig,
      db.db,
      tableName,
      tableSpec.zqlSpec,
      primaryKey,
      () => this.#shouldYield(),
    );
    this.#tables.set(tableName, source);
    this.#lc.debug?.(`created TableSource for ${tableName}`);
    return source;
  }

  #shouldYield(): boolean {
    if (this.#hydrateContext) {
      return this.#hydrateContext.timer.elapsedLap() > this.#yieldThresholdMs();
    }
    if (this.#advanceContext) {
      return this.#shouldAdvanceYieldMaybeAbortAdvance();
    }
    throw new Error('shouldYield called outside of hydration or advancement');
  }

  /**
   * Cancel the advancement processing, by throwing a ResetPipelinesSignal, if
   * it has taken longer than half the total hydration time to make it through
   * half of the advancement, or if processing time exceeds total hydration
   * time.  This serves as both a circuit breaker for very large transactions,
   * as well as a bound on the amount of time the previous connection locks
   * the inactive WAL file (as the lock prevents WAL2 from switching to the
   * free WAL when the current one is over the size limit, which can make
   * the WAL grow continuously and compound slowness).
   * This is checked:
   * 1. before starting to process each change in an advancement is processed
   * 2. whenever a row is fetched from a TableSource during push processing
   */
  #shouldAdvanceYieldMaybeAbortAdvance(): boolean {
    const {
      pos,
      numChanges,
      timer: advanceTimer,
      totalHydrationTimeMs,
    } = must(this.#advanceContext);
    const elapsed = advanceTimer.totalElapsed();
    if (
      elapsed > MIN_ADVANCEMENT_TIME_LIMIT_MS &&
      (elapsed > totalHydrationTimeMs ||
        (elapsed > totalHydrationTimeMs / 2 && pos <= numChanges / 2))
    ) {
      throw new ResetPipelinesSignal(
        `Advancement exceeded timeout at ${pos} of ${numChanges} changes ` +
          `after ${elapsed} ms. Advancement time limited based on total ` +
          `hydration time of ${totalHydrationTimeMs} ms.`,
        'advancement-timeout',
      );
    }
    return advanceTimer.elapsedLap() > this.#yieldThresholdMs();
  }

  /**
   * Wraps an iterable of changes with timeout and yield checks,
   * matching the behavior of the TS #advance() generator.
   */
  *#wrapWithTimeout(
    changes: Iterable<RowChange | 'yield'>,
    timer: Timer,
    numChanges: number,
  ): Iterable<RowChange | 'yield'> {
    const totalHydrationTimeMs = this.totalHydrationTimeMs();
    this.#advanceContext = {
      timer,
      totalHydrationTimeMs,
      numChanges,
      pos: 0,
    };
    try {
      for (const change of changes) {
        if (this.#shouldAdvanceYieldMaybeAbortAdvance()) {
          yield 'yield';
        }
        yield change;
        if (change !== 'yield') {
          this.#advanceContext.pos++;
        }
      }
    } finally {
      this.#advanceContext = null;
    }
  }

  /** Implements `BuilderDelegate.createStorage()` */
  #createStorage(): Storage {
    return new RustTakeStorage(this.#rustStorage, ++this.#rustOpID);
  }
}

class Streamer {
  readonly #primaryKeys: Map<string, PrimaryKey>;
  readonly #tableSpecs: Map<string, LiteAndZqlSpec>;

  constructor(
    primaryKeys: Map<string, PrimaryKey>,
    tableSpecs: Map<string, LiteAndZqlSpec>,
  ) {
    this.#primaryKeys = primaryKeys;
    this.#tableSpecs = tableSpecs;
  }

  readonly #changes: [
    queryID: string,
    schema: SourceSchema,
    changes: Iterable<Change | 'yield'>,
  ][] = [];

  accumulate(
    queryID: string,
    schema: SourceSchema,
    changes: Iterable<Change | 'yield'>,
  ): this {
    this.#changes.push([queryID, schema, changes]);
    return this;
  }

  *stream(): Iterable<RowChange | 'yield'> {
    for (const [queryID, schema, changes] of this.#changes) {
      yield* this.#streamChanges(queryID, schema, changes);
    }
  }

  *#streamChanges(
    queryID: string,
    schema: SourceSchema,
    changes: Iterable<Change | 'yield'>,
  ): Iterable<RowChange | 'yield'> {
    // We do not sync rows gathered by the permissions
    // system to the client.
    if (schema.system === 'permissions') {
      return;
    }

    for (const change of changes) {
      if (change === 'yield') {
        yield change;
        continue;
      }
      const type = change[ChangeIndex.TYPE];
      switch (type) {
        case ChangeType.REMOVE:
        case ChangeType.ADD: {
          yield* this.#streamNodes(queryID, schema, type, () => [
            change[ChangeIndex.NODE],
          ]);
          break;
        }

        case ChangeType.CHILD: {
          const child = change[ChangeIndex.CHILD_DATA];
          const childSchema = must(
            schema.relationships[child.relationshipName],
          );

          yield* this.#streamChanges(queryID, childSchema, [child.change]);
          break;
        }
        case ChangeType.EDIT:
          yield* this.#streamNodes(queryID, schema, type, () => [
            {row: change[ChangeIndex.NODE].row, relationships: {}},
          ]);
          break;
        default:
          unreachable(change[ChangeIndex.TYPE]);
      }
    }
  }

  *#streamNodes(
    queryID: string,
    schema: SourceSchema,
    op: ChangeType.ADD | ChangeType.REMOVE | ChangeType.EDIT,
    nodes: () => Iterable<Node | 'yield'>,
  ): Iterable<RowChange | 'yield'> {
    const {tableName: table, system} = schema;

    const primaryKey = must(this.#primaryKeys.get(table));
    const spec = must(this.#tableSpecs.get(table)).tableSpec;

    // We do not sync rows gathered by the permissions
    // system to the client.
    if (system === 'permissions') {
      return;
    }

    for (const node of nodes()) {
      if (node === 'yield') {
        yield node;
        continue;
      }
      const {relationships} = node;
      let {row} = node;
      const rowKey = getRowKey(primaryKey, row);
      if (op !== ChangeType.REMOVE) {
        const rowVersion = row[ZERO_VERSION_COLUMN_NAME];
        if (
          typeof rowVersion === 'string' &&
          rowVersion < (spec.minRowVersion ?? '00')
        ) {
          row = {...row, [ZERO_VERSION_COLUMN_NAME]: spec.minRowVersion};
        }
      }

      yield {
        type: op,
        queryID,
        table,
        rowKey,
        row: op === ChangeType.REMOVE ? undefined : row,
      } as RowChange;

      for (const [relationship, children] of Object.entries(relationships)) {
        const childSchema = must(schema.relationships[relationship]);
        yield* this.#streamNodes(queryID, childSchema, op, children);
      }
    }
  }
}

function* toAdds(nodes: Iterable<Node | 'yield'>): Iterable<Change | 'yield'> {
  for (const node of nodes) {
    if (node === 'yield') {
      yield node;
      continue;
    }
    yield [ChangeType.ADD, node, null];
  }
}

function getRowKey(cols: PrimaryKey, row: Row): RowKey {
  return Object.fromEntries(cols.map(col => [col, must(row[col])]));
}

/**
 * Core hydration logic used by {@link PipelineDriver#addQuery}, extracted to a
 * function for reuse by bin-analyze so that bin-analyze's hydration logic
 * is as close as possible to zero-cache's real hydration logic.
 */
export function* hydrate(
  input: Input,
  hash: string,
  clientSchema: ClientSchema,
  tableSpecs: Map<string, LiteAndZqlSpec>,
): Iterable<RowChange | 'yield'> {
  const res = input.fetch({});
  const streamer = new Streamer(
    buildPrimaryKeys(clientSchema),
    tableSpecs,
  ).accumulate(hash, input.getSchema(), toAdds(res));
  yield* streamer.stream();
}

export function* hydrateInternal(
  input: Input,
  hash: string,
  primaryKeys: Map<string, PrimaryKey>,
  tableSpecs: Map<string, LiteAndZqlSpec>,
): Iterable<RowChange | 'yield'> {
  const res = input.fetch({});
  const streamer = new Streamer(primaryKeys, tableSpecs).accumulate(
    hash,
    input.getSchema(),
    toAdds(res),
  );
  yield* streamer.stream();
}

function buildPrimaryKeys(
  clientSchema: ClientSchema,
  primaryKeys: Map<string, PrimaryKey> = new Map<string, PrimaryKey>(),
) {
  for (const [tableName, {primaryKey}] of Object.entries(clientSchema.tables)) {
    primaryKeys.set(tableName, primaryKey as unknown as PrimaryKey);
  }
  return primaryKeys;
}

function mustGetPrimaryKey(
  primaryKeys: Map<string, PrimaryKey> | null,
  table: string,
): PrimaryKey {
  const pKeys = must(primaryKeys, 'primaryKey map must be non-null');

  const rv = pKeys.get(table);
  assert(
    rv,
    () =>
      // oxlint-disable-next-line typescript/restrict-template-expressions e18e/prefer-array-to-sorted
      `table '${table}' is not one of: ${[...pKeys.keys()].sort()}. ` +
      `Check the spelling and ensure that the table has a primary key.`,
  );
  return rv;
}

/**
 * Collect table names from EXISTS subqueries that have `system: 'permissions'`.
 * Rust hydration doesn't know about the permissions system, so these child
 * rows need to be filtered out in `#convertDecodedChanges`.
 */
function collectPermissionTables(ast: AST): Set<string> {
  const result = new Set<string>();
  const visitCondition = (cond: Condition) => {
    if (cond.type === 'and' || cond.type === 'or') {
      for (const sub of cond.conditions) {
        visitCondition(sub);
      }
    } else if (cond.type === 'correlatedSubquery') {
      if (cond.related.system === 'permissions') {
        // Collect all tables reachable from this permission subquery
        collectAllTables(cond.related.subquery, result);
      } else {
        // Still need to recurse into non-permission subqueries
        visitAst(cond.related.subquery);
      }
    }
  };
  const visitAst = (node: AST) => {
    if (node.where) {
      visitCondition(node.where);
    }
    if (node.related) {
      for (const rel of node.related) {
        visitAst(rel.subquery);
      }
    }
  };
  visitAst(ast);
  return result;
}

function collectAllTables(ast: AST, out: Set<string>): void {
  out.add(ast.table);
  if (ast.alias) {
    out.add(ast.alias);
  }
  if (ast.related) {
    for (const rel of ast.related) {
      collectAllTables(rel.subquery, out);
    }
  }
  if (ast.where) {
    collectAllTablesFromCondition(ast.where, out);
  }
}

function collectAllTablesFromCondition(
  cond: Condition,
  out: Set<string>,
): void {
  if (cond.type === 'and' || cond.type === 'or') {
    for (const sub of cond.conditions) {
      collectAllTablesFromCondition(sub, out);
    }
  } else if (cond.type === 'correlatedSubquery') {
    collectAllTables(cond.related.subquery, out);
  }
}

function collectLiteralConditions(
  cond: Condition | undefined,
  out: Array<{column: string; value: LiteralValue}>,
): void {
  if (!cond) {
    return;
  }
  if (cond.type === 'simple') {
    if (cond.left.type === 'column' && cond.right.type === 'literal') {
      out.push({column: cond.left.name, value: cond.right.value});
    }
  } else if (cond.type === 'and') {
    for (const sub of cond.conditions) {
      collectLiteralConditions(sub, out);
    }
  }
}
