import {createRequire} from 'node:module';
import type {LogContext} from '@rocicorp/logger';
import {assert, unreachable} from '../../../../shared/src/asserts.ts';
import {deepEqual, type JSONValue} from '../../../../shared/src/json.ts';
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
import {
  type Source,
  type SourceChange,
  type SourceInput,
  makeSourceChangeAdd,
  makeSourceChangeEdit,
  makeSourceChangeRemove,
} from '../../../../zql/src/ivm/source.ts';
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
  decodeAdvanceResultBuf,
  type DecodedRowChange,
} from './decode-advance-buf.ts';
import {decodeDispatchPokeBuf} from './decode-dispatch-buf.ts';
import {
  isDualExecEnabled,
  dualExecCompare,
  materializeChanges,
} from './dual-executor.ts';
import {isRustExistsAvailable, createRustExistsWrapper} from './rust-exists.ts';
import {isRustJoinAvailable} from './rust-join.ts';

type RustFanOutFn = (
  changesJson: string,
  pipelineConfigsJson: string,
) => string;

type RustHydrateFn = (dbPath: string, queriesJson: string) => Buffer;

type RustDispatchPokeFn = (
  changesJson: string,
  vsPipelinesJson: string,
) => Buffer;

let rustFanOutFn: RustFanOutFn | undefined;
let rustHydrateFn: RustHydrateFn | undefined;
let rustDispatchPokeFn: RustDispatchPokeFn | undefined;
try {
  const esmRequire = createRequire(import.meta.url);
  const bindings = esmRequire('zqlite-rs');
  rustFanOutFn = bindings?.rustFanOut;
  rustHydrateFn = bindings?.rustHydrate;
  rustDispatchPokeFn = bindings?.rustDispatchPoke;
} catch {
  rustFanOutFn = undefined;
  rustHydrateFn = undefined;
  rustDispatchPokeFn = undefined;
}

const DISABLE_RUST_DISPATCH =
  process.env.ZERO_DISABLE_RUST_DISPATCH === '1' ||
  process.env.ZERO_DISABLE_RUST_DISPATCH === 'true';

interface RustPipelineConfig {
  query_id: string;
  source_tables: string[];
  operators: RustOperator[];
  primary_key: string[];
  related?:
    | Array<{
        relationship: string;
        parent_field: string[];
        child_field: string[];
        child_ast: unknown;
      }>
    | undefined;
  limit?: number | null | undefined;
  order_by?: [string, string][] | undefined;
}
type RustOperator =
  | {type: 'filter'; predicate: unknown}
  | {type: 'take'; sort: [string, string][]; limit: number | null}
  | {
      type: 'exists';
      relationship: string;
      parent_field: string[];
      not_exists: boolean;
    }
  | {
      type: 'join';
      relationship: string;
      parent_field: string[];
      child_field: string[];
      child_ast: unknown;
    };

const USE_RUST_IVM = process.env.ZERO_DISABLE_RUST_IVM !== '1';
const USE_RUST_JOIN = USE_RUST_IVM && isRustJoinAvailable();
const USE_RUST_EXISTS = USE_RUST_IVM && isRustExistsAvailable();
const USE_RUST_ADVANCE = USE_RUST_IVM && rustFanOutFn !== undefined;
const USE_RUST_HYDRATION =
  USE_RUST_IVM &&
  process.env.ZERO_DISABLE_RUST_HYDRATION !== '1' &&
  rustHydrateFn !== undefined;
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
import {
  getOrCreateCounter,
  getOrCreateHistogram,
} from '../../observability/metrics.ts';
import type {InspectorDelegate} from '../../server/inspector-delegate.ts';
import {type RowKey} from '../../types/row-key.ts';
import {type ShardID} from '../../types/shards.ts';
import {
  getSubscriptionState,
  ZERO_VERSION_COLUMN_NAME,
} from '../replicator/schema/replication-state.ts';
import {checkClientSchema} from './client-schema.ts';
import type {Snapshotter} from './snapshotter.ts';
import {ResetPipelinesSignal, type SnapshotDiff} from './snapshotter.ts';

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
  readonly #rustStorage: RustStorage | null;
  readonly #rustJoinAvailable: boolean;
  #rustOpID = 0;
  readonly #shardID: ShardID;
  readonly #logConfig: LogConfig;
  readonly #config: ZeroConfig | undefined;
  readonly #tableSpecs = new Map<string, LiteAndZqlSpec>();
  readonly #allTableNames = new Set<string>();
  readonly #costModels: WeakMap<Database, ConnectionCostModel> | undefined;
  readonly #yieldThresholdMs: () => number;
  #streamer: Streamer | null = null;
  #hydrateContext: HydrateContext | null = null;
  #advanceContext: AdvanceContext | null = null;
  #replicaVersion: string | null = null;
  #primaryKeys: Map<string, PrimaryKey> | null = null;
  #permissions: LoadedPermissions | null = null;

  readonly #advanceTime = getOrCreateHistogram('sync', 'ivm.advance-time', {
    description:
      'Time to advance all queries for a given client group for in response to a single change.',
    unit: 's',
  });

  readonly #conflictRowsDeleted = getOrCreateCounter(
    'sync',
    'ivm.conflict-rows-deleted',
    'Number of rows deleted because they conflicted with added row',
  );

  readonly #inspectorDelegate: InspectorDelegate;
  readonly #pipelineConfigs = new Map<string, RustPipelineConfig>();
  #useRustAdvance = false;

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
    this.#rustStorage = USE_RUST_IVM ? new RustStorage() : null;
    this.#rustJoinAvailable = USE_RUST_JOIN;
    this.#shardID = shardID;
    this.#logConfig = logConfig;
    this.#config = config;
    this.#inspectorDelegate = inspectorDelegate;
    this.#costModels = enablePlanner ? new WeakMap() : undefined;
    this.#yieldThresholdMs = yieldThresholdMs;
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
    if (this.#rustJoinAvailable) {
      this.#lc.debug?.('Rust join acceleration available');
    }
    if (USE_RUST_EXISTS) {
      this.#lc.debug?.('Rust exists acceleration available');
    }
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
    this.#pipelineConfigs.clear();
    this.#useRustAdvance = false;
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

      // Capture Take storage reference so we can initialize it after Rust hydration.
      let takeStorage: Storage | null = null;
      const input = buildPipeline(
        resolvedQuery,
        {
          debug: debugDelegate,
          enableNotExists: true, // Server-side can handle NOT EXISTS
          getSource: name => this.#getSource(name),
          createStorage: (name: string) => {
            const storage = this.#createStorage();
            // Capture only the top-level Take storage (name === ':take').
            // Child Takes in .related() subqueries have names like '.comments:take'
            // and are partitioned — they don't need this initialization.
            if (name === ':take') {
              takeStorage = storage;
            }
            return storage;
          },
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
            if (USE_RUST_EXISTS && name.includes(':exists(')) {
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
      const schema = input.getSchema();
      input.setOutput({
        push: change => {
          const streamer = this.#streamer;
          assert(streamer, 'must #startAccumulating() before pushing changes');
          streamer.accumulate(queryID, schema, [change]);
          return [];
        },
      });

      if (USE_RUST_HYDRATION) {
        let hydratedRowCount = 0;
        let lastHydratedRow: Row | undefined;
        if (isDualExecEnabled()) {
          for (const item of this.#dualExecHydrate(
            queryID,
            resolvedQuery,
            input,
          )) {
            if (item.type === ChangeType.ADD) {
              hydratedRowCount++;
              lastHydratedRow = item.row;
            }
            yield item;
          }
        } else {
          for (const item of this.#rustHydrate(queryID, resolvedQuery)) {
            if (item !== 'yield' && item.type === ChangeType.ADD) {
              hydratedRowCount++;
              lastHydratedRow = item.row;
            }
            yield item;
          }
        }
        // After Rust hydration, initialize Take state so TS advance works.
        // Rust hydration bypasses input.fetch(), leaving Take's storage empty.
        // Without this, Take.push() silently drops all changes (non-reactive).
        if (takeStorage && resolvedQuery.limit !== undefined) {
          initializeTakeState(takeStorage, hydratedRowCount, lastHydratedRow);
        }
        // Warm up the TS pipeline to initialize child Take operators
        // (e.g., in EXISTS/JOIN subqueries). Rust hydration bypasses
        // input.fetch(), so child Takes have empty storage and silently
        // drop all pushes. Running a fetch through the pipeline triggers
        // each child Take's #initialFetch, populating their partition state.
        for (const node of input.fetch({})) {
          if (node === 'yield') {
            continue;
          }
          // Discard results — we only need the side effect of
          // initializing Take state in child subquery pipelines.
        }
      } else {
        yield* hydrateInternal(
          input,
          queryID,
          must(this.#primaryKeys),
          this.#tableSpecs,
        );
      }

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
        const companionSchema = companionInput.getSchema();
        const {childField, resolvedValue} = meta;
        companionInput.setOutput({
          push: (change: Change) => {
            let newValue: LiteralValue | null | undefined;
            switch (change[ChangeIndex.TYPE]) {
              case ChangeType.ADD:
              case ChangeType.EDIT:
                newValue =
                  (change[ChangeIndex.NODE].row[childField] as LiteralValue) ??
                  null;
                break;
              case ChangeType.REMOVE:
                newValue = undefined;
                break;
              case ChangeType.CHILD:
                return [];
            }
            if (!scalarValuesEqual(newValue, resolvedValue)) {
              throw new ResetPipelinesSignal(
                `Scalar subquery value changed for ${meta.ast.table}: ` +
                  `${String(resolvedValue)} -> ${String(newValue)}`,
                'scalar-subquery',
              );
            }
            const streamer = this.#streamer;
            assert(
              streamer,
              'must #startAccumulating() before pushing changes',
            );
            streamer.accumulate(queryID, companionSchema, [change]);
            return [];
          },
        });
        liveCompanions.push({input: companionInput, childField, resolvedValue});
      }

      // Note: This hydrationTime is a wall-clock overestimate, as it does
      // not take time slicing into account. The view-syncer resets this
      // to a more precise processing-time measurement with setHydrationTime().
      this.#pipelines.set(queryID, {
        input,
        hydrationTimeMs,
        transformedAst: resolvedQuery,
        transformationHash,
        companions: liveCompanions,
      });

      if (USE_RUST_ADVANCE) {
        const config = this.#extractPipelineConfig(
          queryID,
          resolvedQuery,
          liveCompanions,
        );
        if (config) {
          this.#pipelineConfigs.set(queryID, config);
        } else {
          this.#pipelineConfigs.delete(queryID);
        }
        this.#reevaluateRustAdvance();
      }
    } finally {
      this.#hydrateContext = null;
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
    if (!USE_RUST_HYDRATION || queries.length === 0) {
      for (const q of queries) {
        yield* this.addQuery(q.transformationHash, q.queryID, q.ast, timer);
      }
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
      takeStorage: Storage | null;
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

      let takeStorage: Storage | null = null;
      const input = buildPipeline(
        resolvedQuery,
        {
          debug: debugDelegate,
          enableNotExists: true,
          getSource: name => this.#getSource(name),
          createStorage: (name: string) => {
            const storage = this.#createStorage();
            if (name === ':take') {
              takeStorage = storage;
            }
            return storage;
          },
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
            if (USE_RUST_EXISTS && name.includes(':exists(')) {
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
      const schema = input.getSchema();
      input.setOutput({
        push: change => {
          const streamer = this.#streamer;
          assert(streamer, 'must #startAccumulating() before pushing changes');
          streamer.accumulate(q.queryID, schema, [change]);
          return [];
        },
      });

      const rustEligible = true;
      if (rustEligible) {
        const tableName = resolvedQuery.table ?? '';
        const pk = this.#primaryKeys?.get(tableName) ?? [];
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
        takeStorage,
        input,
        debugDelegate,
        rustEligible,
      });
    }

    // Phase 2: Single batch Rust hydration call.
    let changesByQuery: Map<string, DecodedRowChange[]> | undefined;
    if (rustPayloads.length > 0) {
      assert(rustHydrateFn, 'Rust hydrate not available');
      const db = this.#snapshotter.current().db;
      const queriesJson = JSON.stringify(rustPayloads);
      const resultBuf = rustHydrateFn(db.db.name, queriesJson);
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
    for (const p of prepared) {
      this.#hydrateContext = {timer};
      try {
        if (p.rustEligible && changesByQuery) {
          const changes = changesByQuery.get(p.queryID) ?? [];
          let hydratedRowCount = 0;
          let lastHydratedRow: Row | undefined;

          if (isDualExecEnabled()) {
            // Dual-exec: run both Rust and TS, compare, yield TS (source of truth).
            const permTables = collectPermissionTables(p.resolvedQuery);
            const rustChanges = materializeChanges(
              this.#convertDecodedChanges(
                p.queryID,
                changes,
                permTables.size > 0 ? permTables : undefined,
              ),
            );
            const tsChanges = materializeChanges(
              hydrateInternal(
                p.input,
                p.queryID,
                must(this.#primaryKeys),
                this.#tableSpecs,
              ),
            );
            const verified = dualExecCompare(
              'hydrate-batch',
              tsChanges,
              rustChanges,
              this.#lc,
            );
            for (const item of verified) {
              if (item.type === ChangeType.ADD) {
                hydratedRowCount++;
                lastHydratedRow = item.row;
              }
              yield item;
            }
          } else {
            const permTables = collectPermissionTables(p.resolvedQuery);
            for (const item of this.#convertDecodedChanges(
              p.queryID,
              changes,
              permTables.size > 0 ? permTables : undefined,
            )) {
              if (item !== 'yield' && item.type === ChangeType.ADD) {
                hydratedRowCount++;
                lastHydratedRow = item.row;
              }
              yield item;
            }
          }

          if (p.takeStorage && p.resolvedQuery.limit !== undefined) {
            initializeTakeState(
              p.takeStorage,
              hydratedRowCount,
              lastHydratedRow,
            );
          }
        } else {
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

        const hydrationTimeMs = timer.totalElapsed();
        p.debugDelegate?.reset();

        // Set up live companion pipelines for reactive scalar subquery monitoring.
        const liveCompanions: CompanionPipeline[] = [];
        for (let i = 0; i < p.companionMeta.length; i++) {
          const meta = p.companionMeta[i];
          const companionInput = p.companionInputs[i];
          const companionSchema = companionInput.getSchema();
          const {childField, resolvedValue} = meta;
          companionInput.setOutput({
            push: (change: Change) => {
              let newValue: LiteralValue | null | undefined;
              switch (change[ChangeIndex.TYPE]) {
                case ChangeType.ADD:
                case ChangeType.EDIT:
                  newValue =
                    (change[ChangeIndex.NODE].row[
                      childField
                    ] as LiteralValue) ?? null;
                  break;
                case ChangeType.REMOVE:
                  newValue = undefined;
                  break;
                case ChangeType.CHILD:
                  return [];
              }
              if (!scalarValuesEqual(newValue, resolvedValue)) {
                throw new ResetPipelinesSignal(
                  `Scalar subquery value changed for ${meta.ast.table}: ` +
                    `${String(resolvedValue)} -> ${String(newValue)}`,
                  'scalar-subquery',
                );
              }
              const streamer = this.#streamer;
              assert(
                streamer,
                'must #startAccumulating() before pushing changes',
              );
              streamer.accumulate(p.queryID, companionSchema, [change]);
              return [];
            },
          });
          liveCompanions.push({
            input: companionInput,
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

        if (USE_RUST_ADVANCE) {
          const config = this.#extractPipelineConfig(
            p.queryID,
            p.resolvedQuery,
            liveCompanions,
          );
          if (config) {
            this.#pipelineConfigs.set(p.queryID, config);
          } else {
            this.#pipelineConfigs.delete(p.queryID);
          }
          this.#reevaluateRustAdvance();
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
    if (this.#pipelineConfigs.delete(queryID)) {
      this.#reevaluateRustAdvance();
    }
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
    vsId?: string | undefined,
  ): {
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  } {
    assert(
      this.initialized(),
      'Pipeline driver must be initialized before advancing',
    );
    // Try dispatch poke path first (Phase 27: cross-VS batching)
    if (
      vsId &&
      !DISABLE_RUST_DISPATCH &&
      rustDispatchPokeFn &&
      this.#useRustAdvance &&
      this.#pipelineConfigs.size > 0
    ) {
      try {
        return this.#rustDispatchAdvance(vsId);
      } catch (e) {
        if (e instanceof ResetPipelinesSignal) throw e;
        this.#lc.warn?.(`Rust dispatch advance failed, falling back: ${e}`);
      }
    }
    if (this.#useRustAdvance && this.#pipelineConfigs.size > 0) {
      if (isDualExecEnabled()) {
        // Dual-exec: run both Rust and TS, compare results
        return this.#dualExecAdvance(timer);
      }
      try {
        return this.#rustAdvance();
      } catch (e) {
        if (e instanceof ResetPipelinesSignal) throw e;
        this.#lc.warn?.(`Rust advance failed, falling back to TS: ${e}`);
        this.#useRustAdvance = false;
      }
    }
    const diff = this.#snapshotter.advance(
      this.#tableSpecs,
      this.#allTableNames,
    );
    const {prev, curr, changes} = diff;
    this.#lc.debug?.(
      `advance ${prev.version} => ${curr.version}: ${changes} changes`,
    );

    return {
      version: curr.version,
      numChanges: changes,
      changes: this.#advance(diff, timer, changes),
    };
  }

  *#advance(
    diff: SnapshotDiff,
    timer: Timer,
    numChanges: number,
  ): Iterable<RowChange | 'yield'> {
    assert(
      this.#hydrateContext === null,
      'Cannot advance while hydration is in progress',
    );
    const totalHydrationTimeMs = this.totalHydrationTimeMs();
    this.#advanceContext = {
      timer,
      totalHydrationTimeMs,
      numChanges,
      pos: 0,
    };
    this.#lc.info?.(
      `starting pipeline advancement of ${numChanges} changes with an ` +
        `advancement time limited based on total hydration time of ` +
        `${totalHydrationTimeMs} ms.`,
    );
    try {
      for (const {table, prevValues, nextValue} of diff) {
        // Advance progress is checked each time a row is fetched
        // from a TableSource during push processing, but some pushes
        // don't read any rows.  Check progress here before processing
        // the next change.
        if (this.#shouldAdvanceYieldMaybeAbortAdvance()) {
          yield 'yield';
        }
        const start = timer.totalElapsed();

        try {
          const tableSource = this.#tables.get(table);
          if (!tableSource) {
            // no pipelines read from this table, so no need to process the change
            continue;
          }
          const primaryKey = mustGetPrimaryKey(this.#primaryKeys, table);
          let editOldRow: Row | undefined = undefined;
          for (const prevValue of prevValues) {
            if (
              nextValue &&
              deepEqual(
                getRowKey(primaryKey, prevValue as Row) as JSONValue,
                getRowKey(primaryKey, nextValue as Row) as JSONValue,
              )
            ) {
              editOldRow = prevValue;
            } else {
              if (nextValue) {
                this.#conflictRowsDeleted.add(1);
              }
              yield* this.#push(
                tableSource,
                makeSourceChangeRemove(prevValue as Row),
              );
            }
          }
          if (nextValue) {
            if (editOldRow) {
              yield* this.#push(
                tableSource,
                makeSourceChangeEdit(nextValue as Row, editOldRow),
              );
            } else {
              yield* this.#push(
                tableSource,
                makeSourceChangeAdd(nextValue as Row),
              );
            }
          }
        } finally {
          this.#advanceContext.pos++;
        }

        const elapsed = timer.totalElapsed() - start;
        this.#advanceTime.record(elapsed / 1000, {
          table,
        });
      }

      // Set the new snapshot on all TableSources.
      const {curr} = diff;
      for (const table of this.#tables.values()) {
        table.setDB(curr.db.db);
      }
      this.#ensureCostModelExistsIfEnabled(curr.db.db);
      this.#lc.debug?.(`Advanced to ${curr.version}`);
    } finally {
      this.#advanceContext = null;
    }
  }

  #extractPipelineConfig(
    queryID: string,
    ast: AST,
    _companions: readonly CompanionPipeline[],
  ): RustPipelineConfig | null {
    if (ast.where && this.#conditionHasCorrelatedSubquery(ast.where)) {
      return null;
    }

    const operators: RustOperator[] = [];
    const existsTypes = collectExistsTypes(ast.where);

    if (ast.where) {
      operators.push({type: 'filter', predicate: ast.where});
    }

    if (ast.related) {
      for (const rel of ast.related) {
        if ((rel.system as string) === 'exists') {
          operators.push({
            type: 'exists',
            relationship: rel.subquery.table ?? '',
            parent_field: [...rel.correlation.parentField],
            not_exists:
              existsTypes.get(
                rel.subquery.alias ?? rel.subquery.table ?? '',
              ) === 'NOT EXISTS',
          });
        } else {
          operators.push({
            type: 'join',
            relationship: rel.subquery.table ?? '',
            parent_field: [...rel.correlation.parentField],
            child_field: [...rel.correlation.childField],
            child_ast: rel.subquery as unknown,
          });
        }
      }
    }

    if (ast.limit !== undefined) {
      operators.push({
        type: 'take',
        sort: (ast.orderBy ?? []).map(([col, dir]) => [col, dir]),
        limit: ast.limit,
      });
    }

    const sourceTables = [ast.table ?? ''];
    const tableName = ast.table ?? '';
    const pk = this.#primaryKeys?.get(tableName) ?? [];
    return {
      query_id: queryID,
      source_tables: sourceTables,
      operators,
      primary_key: [...pk],
      related:
        ast.related?.map(r => ({
          relationship: r.subquery.table ?? '',
          parent_field: [...r.correlation.parentField],
          child_field: [...r.correlation.childField],
          child_ast: r.subquery as unknown,
        })) ?? [],
      limit: ast.limit ?? null,
      order_by: ast.orderBy?.map(
        ([col, dir]) => [col, dir] as [string, string],
      ),
    };
  }

  #conditionHasCorrelatedSubquery(cond: Condition): boolean {
    if (cond.type === 'correlatedSubquery') return true;
    if (cond.type === 'and' || cond.type === 'or') {
      return cond.conditions.some(c => this.#conditionHasCorrelatedSubquery(c));
    }
    return false;
  }

  #reevaluateRustAdvance() {
    this.#useRustAdvance =
      USE_RUST_ADVANCE &&
      this.#pipelines.size > 0 &&
      this.#pipelineConfigs.size === this.#pipelines.size &&
      // When dual-exec is enabled, allow ALL operator types through
      // so they get compared. In production, restrict to filter-only.
      (isDualExecEnabled() ||
        [...this.#pipelineConfigs.values()].every(c =>
          c.operators.every(op => op.type === 'filter'),
        )) &&
      // Rust advance cannot monitor companion pipelines (scalar subquery
      // change detection requires pushing through TS IVM).
      [...this.#pipelines.values()].every(p => p.companions.length === 0) &&
      // Rust fan-out is stateless per-change and cannot correctly handle
      // unique key conflicts that span multiple diff entries.
      [...this.#tableSpecs.values()].every(
        s => s.tableSpec.uniqueKeys.length <= 1,
      );
  }

  #serializePipelineConfigs(): string {
    return JSON.stringify([...this.#pipelineConfigs.values()]);
  }

  #rustDispatchAdvance(vsId: string): {
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  } {
    assert(rustDispatchPokeFn, 'Rust dispatch poke not available');

    const diff = this.#snapshotter.advance(
      this.#tableSpecs,
      this.#allTableNames,
    );
    const {prev, curr, changes: numChanges} = diff;

    this.#lc.debug?.(
      `rust_dispatch_poke ${prev.version} => ${curr.version}: ${numChanges} changes, ${this.#pipelineConfigs.size} pipelines, vs=${vsId}`,
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

    const changesJson = JSON.stringify(collectedChanges);
    const vsPipelinesJson = JSON.stringify([
      {vs_id: vsId, pipelines: [...this.#pipelineConfigs.values()]},
    ]);

    const resultBuf = rustDispatchPokeFn(changesJson, vsPipelinesJson);
    const vsResults = decodeDispatchPokeBuf(resultBuf);

    // Find result for this VS
    const vsResult = vsResults.find(r => r.vsId === vsId);
    if (!vsResult) {
      throw new Error(`No dispatch result for VS ${vsId}`);
    }
    if (vsResult.error) {
      throw new ResetPipelinesSignal(
        `Rust dispatch poke error for VS ${vsId}: ${vsResult.error}`,
        'rust-dispatch-error',
      );
    }

    // Update table DBs
    for (const table of this.#tables.values()) {
      table.setDB(curr.db.db);
    }
    this.#ensureCostModelExistsIfEnabled(curr.db.db);
    this.#lc.debug?.(`Rust dispatch poke advanced to ${curr.version}`);

    return {
      version: curr.version,
      numChanges,
      changes: this.#convertDispatchChanges(vsResult.changes),
    };
  }

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
      yield {
        type,
        queryID: change.queryID,
        table: change.table,
        rowKey: change.row_key,
        row: change.row ?? (change.row_key as Row),
      } as RowChange;
    }
  }

  #rustAdvance(): {
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  } {
    assert(rustFanOutFn, 'Rust fan-out not available');

    // Use TS diff (correct two-snapshot isolation) then Rust for Rayon fan-out
    const diff = this.#snapshotter.advance(
      this.#tableSpecs,
      this.#allTableNames,
    );
    const {prev, curr, changes: numChanges} = diff;

    this.#lc.debug?.(
      `rust_fan_out ${prev.version} => ${curr.version}: ${numChanges} changes, ${this.#pipelineConfigs.size} pipelines`,
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

    const changesJson = JSON.stringify(collectedChanges);
    const pipelineConfigsJson = this.#serializePipelineConfigs();

    const resultJson = rustFanOutFn(changesJson, pipelineConfigsJson);

    const result = JSON.parse(resultJson) as {
      changes: Array<{
        queryID: string;
        table: string;
        row_key: Row;
        row: Row | null;
        type: string;
      }>;
      error?: string;
      error_type?: string;
    };

    if (result.error) {
      throw new Error(result.error);
    }

    for (const table of this.#tables.values()) {
      table.setDB(curr.db.db);
    }
    this.#ensureCostModelExistsIfEnabled(curr.db.db);
    this.#lc.debug?.(`Rust fan-out advanced to ${curr.version}`);

    return {
      version: curr.version,
      numChanges,
      changes: this.#convertRustChanges(result.changes),
    };
  }

  *#dualExecHydrate(
    queryID: string,
    resolvedQuery: AST,
    input: Input,
  ): Iterable<RowChange> {
    // Run both Rust and TS hydration paths and compare results.
    const rustChanges = materializeChanges(
      this.#rustHydrate(queryID, resolvedQuery),
    );
    const tsChanges = materializeChanges(
      hydrateInternal(
        input,
        queryID,
        must(this.#primaryKeys),
        this.#tableSpecs,
      ),
    );
    const result = dualExecCompare('hydrate', tsChanges, rustChanges, this.#lc);
    yield* result;
  }

  *#rustHydrate(
    queryID: string,
    resolvedQuery: AST,
  ): Iterable<RowChange | 'yield'> {
    assert(rustHydrateFn, 'Rust hydrate not available');
    const db = this.#snapshotter.current().db;
    const tableName = resolvedQuery.table ?? '';
    const pk = this.#primaryKeys?.get(tableName) ?? [];
    const queriesJson = JSON.stringify([
      {
        query_id: queryID,
        ast: resolvedQuery,
        primary_key: [...pk],
        column_types: this.#collectColumnTypes(resolvedQuery),
        all_primary_keys: this.#collectAllPrimaryKeys(resolvedQuery),
      },
    ]);
    const resultBuf = rustHydrateFn(db.db.name, queriesJson);
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

  *#convertRustChanges(
    changes: Array<{
      queryID: string;
      table: string;
      row_key: Row;
      row: Row | null;
      type: string;
    }>,
  ): Iterable<RowChange | 'yield'> {
    for (const change of changes) {
      const type =
        change.type === 'add'
          ? ChangeType.ADD
          : change.type === 'edit'
            ? ChangeType.EDIT
            : ChangeType.REMOVE;
      yield {
        type,
        queryID: change.queryID,
        table: change.table,
        rowKey: change.row_key,
        row:
          type === ChangeType.REMOVE
            ? undefined
            : (change.row ?? change.row_key),
      } as RowChange;
    }
  }

  /**
   * Dual-execution advance: runs both TS and Rust fan-out on the SAME diff,
   * then compares results. TS is the source of truth.
   *
   * Uses one snapshotter.advance() call. The diff is materialized once and
   * fed to both Rust (via rust_fan_out JSON) and TS (via #advance with a
   * synthetic iterable). The TS #advance path handles db state updates.
   */
  #dualExecAdvance(timer: Timer): {
    version: string;
    numChanges: number;
    changes: Iterable<RowChange | 'yield'>;
  } {
    assert(rustFanOutFn, 'Rust fan-out not available');

    // 1. Get diff once, materialize changes
    const diff = this.#snapshotter.advance(
      this.#tableSpecs,
      this.#allTableNames,
    );
    const {prev, curr, changes: numChanges} = diff;

    const collectedChanges: Array<{
      table: string;
      prevValues: ReadonlyArray<Readonly<Row>>;
      nextValue: Readonly<Row> | null;
      rowKey: unknown;
    }> = [];
    for (const change of diff) {
      collectedChanges.push(change);
    }

    this.#lc.debug?.(
      `dual-exec: ${this.#pipelineConfigs.size} pipelines (full tree)`,
    );

    // 2. Run Rust fan-out on collected changes
    let rustChanges: RowChange[] = [];
    try {
      const changesJson = JSON.stringify(collectedChanges);
      const pipelineConfigsJson = this.#serializePipelineConfigs();
      const resultJson = rustFanOutFn(changesJson, pipelineConfigsJson);
      const result = JSON.parse(resultJson) as {
        changes: Array<{
          queryID: string;
          table: string;
          row_key: Row;
          row: Row | null;
          type: string;
        }>;
        error?: string;
      };
      if (!result.error) {
        rustChanges = materializeChanges(
          this.#convertRustChanges(result.changes),
        );
      } else {
        this.#lc.warn?.(`[dual-exec] Rust fan-out error: ${result.error}`);
      }
    } catch (e) {
      this.#lc.warn?.(`[dual-exec] Rust fan-out exception: ${e}`);
    }

    // 3. Run TS path on the same collected changes via synthetic diff
    const syntheticDiff = Object.assign(collectedChanges[Symbol.iterator](), {
      prev,
      curr,
      changes: numChanges,
    }) as unknown as SnapshotDiff;

    return {
      version: curr.version,
      numChanges,
      changes: this.#dualExecYieldAndCompare(
        this.#advance(syntheticDiff, timer, numChanges),
        rustChanges,
      ),
    };
  }

  *#dualExecYieldAndCompare(
    tsGen: Iterable<RowChange | 'yield'>,
    rustChanges: RowChange[],
  ): Iterable<RowChange | 'yield'> {
    const tsChanges: RowChange[] = [];
    for (const item of tsGen) {
      if (item !== 'yield') {
        tsChanges.push(item);
      }
      yield item;
    }
    // Compare after all TS changes have been yielded.
    // TS is always the source of truth; this validates Rust correctness.
    dualExecCompare('advance', tsChanges, rustChanges, this.#lc);
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

  /** Implements `BuilderDelegate.createStorage()` */
  #createStorage(): Storage {
    if (this.#rustStorage) {
      return new RustTakeStorage(this.#rustStorage, ++this.#rustOpID);
    }
    return this.#storage.createStorage();
  }

  *#push(
    source: TableSource,
    change: SourceChange,
  ): Iterable<RowChange | 'yield'> {
    this.#startAccumulating();
    try {
      for (const val of source.genPush(change)) {
        if (val === 'yield') {
          yield 'yield';
        }
        for (const changeOrYield of this.#stopAccumulating().stream()) {
          yield changeOrYield;
        }
        this.#startAccumulating();
      }
    } finally {
      if (this.#streamer !== null) {
        this.#stopAccumulating();
      }
    }
  }

  #startAccumulating() {
    assert(this.#streamer === null, 'Streamer already started');
    this.#streamer = new Streamer(must(this.#primaryKeys), this.#tableSpecs);
  }

  #stopAccumulating(): Streamer {
    const streamer = this.#streamer;
    assert(streamer, 'Streamer not started');
    this.#streamer = null;
    return streamer;
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

/**
 * Initializes Take operator storage after Rust hydration.
 *
 * Rust hydration bypasses the TS operator pipeline (specifically input.fetch()),
 * which means Take's #initialFetch never runs and its storage stays empty.
 * When Take.push() sees empty storage, it silently drops all changes — making
 * the query non-reactive after initial hydration.
 *
 * This function writes the same state that Take.#initialFetch would have written:
 * - TakeState {size, bound} under the take state key
 * - MAX_BOUND_KEY with the bound row (for partitioned takes / nested fetches)
 *
 * For non-partitioned queries (the common case with Rust hydration),
 * the take state key is simply '["take"]'.
 */
function initializeTakeState(
  storage: Storage,
  size: number,
  bound: Row | undefined,
): void {
  // The take state key for non-partitioned queries.
  // See getTakeStateKey() in take.ts — with no partition key, it's JSON.stringify(['take']).
  const takeStateKey = JSON.stringify(['take']);
  storage.set(takeStateKey, {size, bound} as unknown as JSONValue);
  if (bound !== undefined) {
    storage.set('maxBound', bound as unknown as JSONValue);
  }
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
 * Compares two scalar subquery resolved values for equality.
 * Unlike `valuesEqual` in data.ts (which treats null != null for join
 * semantics), this uses identity semantics: undefined === undefined
 * (no row matched), null === null (row matched but field was NULL).
 */
function scalarValuesEqual(
  a: LiteralValue | null | undefined,
  b: LiteralValue | null | undefined,
): boolean {
  return a === b;
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
