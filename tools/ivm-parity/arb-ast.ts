/**
 * fast-check generator — full operator surface per FUZZ-01 spec; targeted arbs
 * guarantee B1/B2/B3 shape coverage per CONTEXT.md D-08.
 *
 * Phase 34 Wave 1 — production fast-check arbitraries for random AST generation.
 *
 * Schema-driven (re-uses `schema as paritySchema` from `./zero-schema.ts`,
 * same pattern as `ast-fuzz.ts:adaptSchema`). Exports `buildArbitraries(schema)`
 * returning `{ arbAst, arbAstWithTargeted, arbB1Targeted, arbB2Targeted,
 *   arbB3Targeted, tables }`.
 *
 * Coexists with `ast-fuzz.ts` (D-04 BFS baseline preserved AS-IS). The two
 * generators feed into the same `harness-coverage.ts` / `harness-advance-coverage.ts`
 * pipeline because both emit the same `AST` JSON shape.
 *
 * Default fast-check shrinker is sufficient (CD-01 — researcher decided default
 * is adequate; no custom shrinker — see RESEARCH §"Custom shrinkers vs default").
 * Recursion via `fc.letrec`. Per-operator arbitraries listed in 34-RESEARCH
 * §"fast-check Generator Design (CD-01)".
 *
 * Depth bounds match `ast-fuzz.ts` (D-04 spirit — fuzzer can't exceed BFS budget):
 *   MAX_WHERE_DEPTH      = 3 — and/or/csq nesting under `where`
 *   MAX_RELATED_DEPTH    = 2 — nesting under `related[]`
 *   MAX_BRANCHES_PER_NODE= 3 — children of an And/Or
 *
 * Targeted shapes (D-08):
 *   arbB1Targeted — Skip + EXISTS — `{table, where: EXISTS, orderBy, start}`
 *   arbB2Targeted — OR(simple, EXISTS) — `{table, where: OR(s, exists)}`
 *   arbB3Targeted — related with limit — `{table, related: [{subquery: {limit}}]}`
 *
 * The targeted arbitraries are deterministic in shape but stochastic in details
 * — each iteration draws a different table/columns/values combination, so
 * coverage spans the schema while guaranteeing the bug-triggering pattern.
 *
 * Cap operator: dead code per IVM-PORT-AUDIT.md Risk #4 — not generated.
 *
 * Source patterns:
 *   - Existing usage of fast-check in
 *     `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts:14-100`
 *   - Schema adapter pattern from `tools/ivm-parity/ast-fuzz.ts:112-166`
 *   - Category C related-orderBy-limit pattern from `ast-fuzz.ts:601-625`
 *   - Skip+orderBy semantics pattern from `ast-fuzz.ts:topOptionsForTable:712-723`
 */
import fc from 'fast-check';
import type {
  AST,
  Condition,
  CorrelatedSubquery,
  CorrelatedSubqueryCondition,
  LiteralValue,
  Ordering,
  SimpleCondition,
  SimpleOperator,
} from '../../packages/zero-protocol/src/ast.ts';

// ---------------------------------------------------------------------------
// Depth bounds — mirror ast-fuzz.ts. Bounded recursion via these caps avoids
// pathological combinatorial blowup. See 34-CONTEXT.md D-04.
// ---------------------------------------------------------------------------

export const MAX_WHERE_DEPTH = 3;
export const MAX_RELATED_DEPTH = 2;
export const MAX_BRANCHES_PER_NODE = 3;

// ---------------------------------------------------------------------------
// Schema adapter — minimal subset of `ast-fuzz.ts:adaptSchema`. Identical
// shape so callers can pass the same `paritySchema` import to both generators.
// ---------------------------------------------------------------------------

export type ColType =
  | 'string'
  | 'string?'
  | 'number'
  | 'number?'
  | 'boolean'
  | 'unknown';

export interface ColumnDef {
  name: string;
  type: ColType;
}

export interface RelationshipDef {
  name: string;
  childTable: string;
  parentField: readonly string[];
  childField: readonly string[];
  cardinality: 'one' | 'many';
}

export interface TableDef {
  name: string;
  pk: readonly string[];
  columns: readonly ColumnDef[];
  relationships: readonly RelationshipDef[];
}

export type AdaptedSchema = Record<string, TableDef>;

export function adaptSchema(zeroSchema: {
  tables: Record<
    string,
    {
      name: string;
      columns: Record<string, {type: string; optional: boolean}>;
      primaryKey: readonly string[];
    }
  >;
  relationships: Record<
    string,
    Record<
      string,
      readonly {
        sourceField: readonly string[];
        destField: readonly string[];
        destSchema: string;
        cardinality: 'one' | 'many';
      }[]
    >
  >;
}): AdaptedSchema {
  const out: AdaptedSchema = {};
  for (const [tableName, table] of Object.entries(zeroSchema.tables)) {
    const columns: ColumnDef[] = Object.entries(table.columns).map(
      ([name, c]) => ({
        name,
        type: zeroTypeToInternal(c.type, c.optional),
      }),
    );
    const rels = zeroSchema.relationships[tableName] ?? {};
    const relationships: RelationshipDef[] = [];
    for (const [relName, hops] of Object.entries(rels)) {
      const first = hops[0];
      if (!first) continue;
      relationships.push({
        name: relName,
        childTable: first.destSchema,
        parentField: first.sourceField,
        childField: first.destField,
        cardinality: first.cardinality,
      });
    }
    out[tableName] = {
      name: table.name,
      pk: table.primaryKey,
      columns,
      relationships,
    };
  }
  return out;
}

function zeroTypeToInternal(type: string, optional: boolean): ColType {
  if (type === 'string') return optional ? 'string?' : 'string';
  if (type === 'number') return optional ? 'number?' : 'number';
  if (type === 'boolean') return 'boolean';
  return 'unknown';
}

// ---------------------------------------------------------------------------
// Per-operator arbitraries.
// Mirrors `ast-fuzz.ts:opsForColumn` (line 185) and `valuesForOp` (line 199).
// Per RESEARCH per-operator arbitraries table.
// ---------------------------------------------------------------------------

const EQ_OPS: SimpleOperator[] = ['=', '!='];
const ORD_OPS: SimpleOperator[] = ['<', '>', '<=', '>='];
const LIKE_OPS: SimpleOperator[] = ['LIKE', 'NOT LIKE', 'ILIKE', 'NOT ILIKE'];
const IN_OPS: SimpleOperator[] = ['IN', 'NOT IN'];
const NULL_OPS: SimpleOperator[] = ['IS', 'IS NOT'];

function opsForColumn(col: ColumnDef): SimpleOperator[] {
  const isString = col.type === 'string' || col.type === 'string?';
  const isNumber = col.type === 'number' || col.type === 'number?';
  const isBool = col.type === 'boolean';
  const isOptional = col.type === 'string?' || col.type === 'number?';
  const ops: SimpleOperator[] = [];
  if (isOptional) ops.push(...NULL_OPS);
  ops.push(...EQ_OPS);
  if (isString || isNumber) ops.push(...IN_OPS);
  if (isString) ops.push(...LIKE_OPS, ...ORD_OPS);
  if (isNumber) ops.push(...ORD_OPS);
  // boolean: only EQ_OPS (= and !=) — no IN/LIKE/ORD per Zero schema.
  void isBool;
  return ops;
}

function literalForCol(col: ColumnDef, op: SimpleOperator): fc.Arbitrary<LiteralValue> {
  if (op === 'IS' || op === 'IS NOT') return fc.constant(null);
  if (op === 'IN' || op === 'NOT IN') {
    if (col.type === 'number' || col.type === 'number?') {
      return fc.array(fc.integer({min: 0, max: 100}), {minLength: 0, maxLength: 3});
    }
    return fc.array(fc.constantFrom('a', 'b', 'c', 'standup'), {
      minLength: 0,
      maxLength: 3,
    });
  }
  if (op === 'LIKE' || op === 'NOT LIKE' || op === 'ILIKE' || op === 'NOT ILIKE') {
    return fc.constantFrom('%standup%', 'standup%', '%notes', 'a%', '%');
  }
  if (col.type === 'boolean') {
    return fc.boolean();
  }
  if (col.type === 'number' || col.type === 'number?') {
    return fc.integer({min: 0, max: 10000});
  }
  return fc.constantFrom('u1', 'u2', 'ch-pub-1', 'ch-priv-1', 'standup-monday');
}

/**
 * Per-table simple-condition arbitrary. Draws (col, op, value) from this
 * table's columns — fixes the Wave 0 cross-tab bug where columns from any
 * table could attach to any other table.
 */
function arbSimpleConditionForTable(table: TableDef): fc.Arbitrary<SimpleCondition> {
  if (table.columns.length === 0) {
    // Degenerate schema — return a no-op constant. Caller's fc.pre may discard.
    return fc.constant({
      type: 'simple' as const,
      op: '=' as SimpleOperator,
      left: {type: 'column' as const, name: 'id'},
      right: {type: 'literal' as const, value: 'never-match'},
    });
  }
  return fc.constantFrom(...table.columns).chain(col =>
    fc.constantFrom(...opsForColumn(col)).chain(op =>
      literalForCol(col, op).map(value => ({
        type: 'simple' as const,
        op,
        left: {type: 'column' as const, name: col.name},
        right: {type: 'literal' as const, value},
      })),
    ),
  );
}

/**
 * Per-table CSQ arbitrary. Picks one of `table.relationships` and emits a
 * CorrelatedSubquery with the proper correlation. Returns undefined if the
 * table has no relationships — caller falls back via fc.oneof to a simple cond.
 *
 * Optional `withLimit` adds an explicit `limit` to the subquery — used by
 * arbB3Targeted to guarantee B3 trigger shape (related-subquery with limit).
 */
function arbCorrelatedSubqueryForTable(
  table: TableDef,
  schema: AdaptedSchema,
  opts: {withLimit?: number} = {},
): fc.Arbitrary<CorrelatedSubquery> | undefined {
  if (table.relationships.length === 0) return undefined;
  return fc.constantFrom(...table.relationships).map((rel, _seed) => {
    const subquery: AST = {
      table: rel.childTable,
      alias: `arb_${table.name}_${rel.name}`,
    };
    if (opts.withLimit !== undefined) {
      subquery.limit = opts.withLimit;
    }
    void schema;
    return {
      correlation: {
        parentField: [...rel.parentField],
        childField: [...rel.childField],
      },
      subquery,
    };
  });
}

/**
 * Wraps a CSQ arbitrary into a CorrelatedSubqueryCondition (i.e. EXISTS on
 * that CSQ). Used by the generic `cond` arb and by arbB1/B2 targeted arbs.
 */
function arbCorrelatedSubqueryCondition(
  csqArb: fc.Arbitrary<CorrelatedSubquery>,
): fc.Arbitrary<CorrelatedSubqueryCondition> {
  return csqArb.map(csq => ({
    type: 'correlatedSubquery' as const,
    related: csq,
    op: 'EXISTS' as const,
  }));
}

/**
 * arbOrderBy(table) — emits a 1-2 element array of [columnName, 'asc'|'desc']
 * tuples drawn from table.columns. Cite RESEARCH per-operator arbitraries
 * table — this is the Skip+Take ordering target.
 */
function arbOrderBy(table: TableDef): fc.Arbitrary<Ordering> {
  if (table.columns.length === 0) {
    return fc.constant([['id', 'asc' as const]] as unknown as Ordering);
  }
  const single = fc
    .constantFrom(...table.columns)
    .chain(col =>
      fc.constantFrom('asc' as const, 'desc' as const).map(
        dir => [[col.name, dir]] as unknown as Ordering,
      ),
    );
  // Two-element ordering — col1 first, then PK as tiebreaker (Zero semantics).
  const dual = fc.constantFrom(...table.columns).chain(col =>
    fc.constantFrom('asc' as const, 'desc' as const).map(dir => {
      const pk = table.pk[0] ?? col.name;
      const tiebreaker: [string, 'asc' | 'desc'] = [pk, dir];
      const lead: [string, 'asc' | 'desc'] = [col.name, dir];
      // Avoid duplicate column.
      if (col.name === pk) {
        return [lead] as unknown as Ordering;
      }
      return [lead, tiebreaker] as unknown as Ordering;
    }),
  );
  return fc.oneof({weight: 3, arbitrary: single}, {weight: 1, arbitrary: dual});
}

/**
 * arbStart(table, ordering) — record with `row` (object whose keys are exactly
 * the ordering columns and values drawn from each column's domain) and
 * `exclusive` (boolean). Skip semantics require the row to cover the orderBy.
 */
function arbStart(
  table: TableDef,
  ordering: Ordering,
): fc.Arbitrary<NonNullable<AST['start']>> {
  const cols = ordering.map(([name]) => name);
  // Build a record-of-arbitraries keyed by ordering column.
  const rowArbs: Record<string, fc.Arbitrary<unknown>> = {};
  for (const colName of cols) {
    const def = table.columns.find(c => c.name === colName);
    if (!def) {
      rowArbs[colName] = fc.constant(null);
      continue;
    }
    if (def.type === 'number' || def.type === 'number?') {
      rowArbs[colName] = fc.integer({min: 0, max: 10000});
    } else if (def.type === 'boolean') {
      rowArbs[colName] = fc.boolean();
    } else {
      rowArbs[colName] = fc.constantFrom('u1', 'u2', 'ch-pub-1', 'standup-monday');
    }
  }
  return fc.record({
    row: fc.record(rowArbs as Record<string, fc.Arbitrary<unknown>>),
    exclusive: fc.boolean(),
  }) as fc.Arbitrary<NonNullable<AST['start']>>;
}

/**
 * arbLimit — integer arbitrary with min 1, max 10. Per RESEARCH per-operator
 * arbitraries table: limit values 1, 3, 5, 10 are the production-relevant set.
 */
const arbLimit: fc.Arbitrary<number> = fc.integer({min: 1, max: 10});

/**
 * arbRelated(table, schema, depth) — bounded recursion via `depth`. Emits a
 * 1-element related[] array (junction edges flattened to single hop in
 * adaptSchema). At MAX_RELATED_DEPTH we stop recursing into the subquery's
 * own related[].
 */
function arbRelated(
  table: TableDef,
  schema: AdaptedSchema,
  depth: number,
): fc.Arbitrary<readonly CorrelatedSubquery[]> | undefined {
  if (depth >= MAX_RELATED_DEPTH) return undefined;
  if (table.relationships.length === 0) return undefined;
  return fc.constantFrom(...table.relationships).chain(rel => {
    const childTable = schema[rel.childTable];
    // Optionally nest another related[] under the child if depth budget allows
    // and the child table itself has relationships.
    const childRelArb =
      childTable && depth + 1 < MAX_RELATED_DEPTH
        ? arbRelated(childTable, schema, depth + 1)
        : undefined;
    if (!childRelArb) {
      return fc.constant([
        {
          correlation: {
            parentField: [...rel.parentField],
            childField: [...rel.childField],
          },
          subquery: {
            table: rel.childTable,
            alias: `arb_${table.name}_${rel.name}_d${depth}`,
          } as AST,
        },
      ] as readonly CorrelatedSubquery[]);
    }
    return fc.option(childRelArb, {nil: undefined, freq: 2}).map(
      grand =>
        [
          {
            correlation: {
              parentField: [...rel.parentField],
              childField: [...rel.childField],
            },
            subquery: {
              table: rel.childTable,
              alias: `arb_${table.name}_${rel.name}_d${depth}`,
              ...(grand !== undefined ? {related: grand} : {}),
            } as AST,
          },
        ] as readonly CorrelatedSubquery[],
    );
  });
}

// ---------------------------------------------------------------------------
// Top-level builder.
// ---------------------------------------------------------------------------

export interface BuiltArbitraries {
  /** Random ASTs across the full operator surface (filter, EXISTS, And/Or,
   *  Take, Skip, related[]). */
  arbAst: fc.Arbitrary<AST>;
  /** B1-targeted: Skip + EXISTS combined with orderBy. Triggers `ast_to_config.rs:226-238`
   *  Skip-after-conditions ordering bug. */
  arbB1Targeted: fc.Arbitrary<AST>;
  /** B2-targeted: OR(simple, EXISTS). Triggers `exists_op.rs:144` parent_sizes
   *  cache poison. */
  arbB2Targeted: fc.Arbitrary<AST>;
  /** B3-targeted: related[] with limit. Triggers `take_op.rs` partition_key
   *  state-key inconsistency on push. */
  arbB3Targeted: fc.Arbitrary<AST>;
  /** Composition: 7/1/1/1 weights of base/B1/B2/B3. With FUZZ_NUM_RUNS=1000
   *  this gives ~100 iterations per targeted shape. Default arbitrary used by
   *  `random-ast-fuzz.ts`. */
  arbAstWithTargeted: fc.Arbitrary<AST>;
  /** Tables present in the schema (handy for callers and test assertions). */
  tables: TableDef[];
}

export function buildArbitraries(zeroSchema: {
  tables: Record<
    string,
    {
      name: string;
      columns: Record<string, {type: string; optional: boolean}>;
      primaryKey: readonly string[];
    }
  >;
  relationships: Record<
    string,
    Record<
      string,
      readonly {
        sourceField: readonly string[];
        destField: readonly string[];
        destSchema: string;
        cardinality: 'one' | 'many';
      }[]
    >
  >;
}): BuiltArbitraries {
  const adapted = adaptSchema(zeroSchema);
  const tables = Object.values(adapted);
  if (tables.length === 0) {
    throw new Error('arb-ast: schema has zero tables — refusing to build arbitraries.');
  }
  const arbTable = fc.constantFrom(...tables);

  // --- Recursive condition arbitrary via fc.letrec --------------------------
  // Each draw of `cond` happens at depth controlled by fc.letrec's max depth
  // (default 5). Combined with min/max-Length 2-3 on And/Or branches, this
  // bounds the AST size. MAX_BRANCHES_PER_NODE = 3.
  //
  // The simple+csq arbitraries are table-LOCAL — each draw chains the table
  // first, then draws columns/rels from THAT table. This fixes Wave 0's
  // cross-tab bug.
  type RecMap = {
    cond: Condition;
    simple: SimpleCondition;
    csq: CorrelatedSubquery;
  };
  const recArb = fc.letrec<RecMap>(tie => ({
    simple: arbTable.chain(t => arbSimpleConditionForTable(t)),
    csq: arbTable.chain(t => {
      const csqArb = arbCorrelatedSubqueryForTable(t, adapted);
      if (!csqArb) {
        // Fallback: pick any table with relationships.
        const withRels = tables.filter(x => x.relationships.length > 0);
        if (withRels.length === 0) {
          // Degenerate: no relationships at all — emit self-referential CSQ.
          // fc.pre in arbAst may discard but at least we don't throw.
          return fc.constant({
            correlation: {parentField: [...t.pk], childField: [...t.pk]},
            subquery: {table: t.name, alias: 'arb_self'} as AST,
          });
        }
        // Pick deterministically via fc.constantFrom rather than Math.random
        // so seeding stays reproducible (Wave 0 used Math.random — bug).
        return fc
          .constantFrom(...withRels)
          .chain(t2 => arbCorrelatedSubqueryForTable(t2, adapted)!);
      }
      return csqArb;
    }),
    cond: fc.oneof(
      {weight: 5, arbitrary: tie('simple')},
      {
        weight: 2,
        arbitrary: fc.record({
          type: fc.constant('and' as const),
          conditions: fc.array(tie('cond'), {
            minLength: 2,
            maxLength: MAX_BRANCHES_PER_NODE,
          }),
        }),
      },
      {
        weight: 2,
        arbitrary: fc.record({
          type: fc.constant('or' as const),
          conditions: fc.array(tie('cond'), {
            minLength: 2,
            maxLength: MAX_BRANCHES_PER_NODE,
          }),
        }),
      },
      {weight: 3, arbitrary: arbCorrelatedSubqueryCondition(tie('csq'))},
    ),
  }));

  // --- Top-level arbAst: full operator surface -----------------------------
  // Per-table chain so column/rel arbitraries are local to the chosen table.
  // fc.pre rejects schema-rejected shapes (start without orderBy, IN with
  // mismatched array element types, etc.) — keeps the fuzzer signal-rich.
  const arbAst: fc.Arbitrary<AST> = arbTable.chain(table => {
    // Per-table arbs.
    const orderByArb = arbOrderBy(table);
    return fc
      .record({
        where: fc.option(recArb.cond, {nil: undefined, freq: 2}),
        limit: fc.option(arbLimit, {nil: undefined, freq: 3}),
        orderBy: fc.option(orderByArb, {nil: undefined, freq: 3}),
        // start is conditional on orderBy being present — generate optimistically
        // then drop in the .filter() below if mismatched.
        startSeed: fc.option(fc.boolean(), {nil: undefined, freq: 4}),
        relatedDepth0: arbTable.chain(t2 => {
          const r = arbRelated(t2 === table ? table : t2, adapted, 0);
          if (!r) return fc.constant(undefined);
          return fc.option(r, {nil: undefined, freq: 3});
        }),
      })
      .chain(opts => {
        // Construct start ONLY when orderBy is present (Zero rejects otherwise).
        const startArb =
          opts.orderBy !== undefined && opts.startSeed !== undefined
            ? fc.option(arbStart(table, opts.orderBy as Ordering), {
                nil: undefined,
                freq: 2,
              })
            : fc.constant(undefined);
        return startArb.map(start => ({
          opts,
          start,
        }));
      })
      .map(({opts, start}) => {
        const ast: AST = {table: table.name};
        if (opts.where !== undefined) ast.where = opts.where;
        if (opts.limit !== undefined) ast.limit = opts.limit;
        if (opts.orderBy !== undefined) ast.orderBy = opts.orderBy as Ordering;
        if (start !== undefined) ast.start = start;
        if (opts.relatedDepth0 !== undefined && opts.relatedDepth0.length > 0) {
          ast.related = [...opts.relatedDepth0];
        }
        return ast;
      })
      // fc.pre-style filter: drop ASTs where start was set without orderBy
      // (already constructively avoided above, but defensive).
      .filter(ast => !ast.start || !!ast.orderBy);
  });

  // ------------------------------------------------------------------------
  // Targeted arbitraries (D-08) — guarantee B1/B2/B3 trigger shapes.
  // ------------------------------------------------------------------------
  const tablesWithRels = tables.filter(t => t.relationships.length > 0);
  const arbTableWithRels =
    tablesWithRels.length > 0 ? fc.constantFrom(...tablesWithRels) : arbTable;

  // arbB1Targeted: AST with EXISTS-where + orderBy + start.
  // Triggers Skip-after-EXISTS ordering bug per IVM-PORT-AUDIT-DEEP §B1.
  const arbB1Targeted: fc.Arbitrary<AST> = arbTableWithRels.chain(table => {
    const csqArb = arbCorrelatedSubqueryForTable(table, adapted);
    if (!csqArb) {
      // Degenerate: emit a where-less B1 so we always produce something.
      return fc.constant({table: table.name});
    }
    // Fixed orderBy: PK ascending (Skip semantics require row to cover orderBy).
    const orderBy: Ordering = [[table.pk[0] ?? 'id', 'asc' as const]] as unknown as Ordering;
    return fc
      .record({
        csq: csqArb,
        start: arbStart(table, orderBy),
      })
      .map(({csq, start}) => ({
        table: table.name,
        where: {
          type: 'correlatedSubquery' as const,
          related: csq,
          op: 'EXISTS' as const,
        } as CorrelatedSubqueryCondition,
        orderBy,
        start,
      }));
  });

  // arbB2Targeted: AST with where = OR(simple, EXISTS).
  // Triggers parent_sizes cache poison per IVM-PORT-AUDIT-DEEP §B2.
  const arbB2Targeted: fc.Arbitrary<AST> = arbTableWithRels.chain(table => {
    const csqArb = arbCorrelatedSubqueryForTable(table, adapted);
    const simpleArb = arbSimpleConditionForTable(table);
    if (!csqArb) {
      return fc.constant({table: table.name});
    }
    return fc
      .record({simple: simpleArb, csq: csqArb})
      .map(({simple, csq}) => ({
        table: table.name,
        where: {
          type: 'or' as const,
          conditions: [
            simple,
            {
              type: 'correlatedSubquery' as const,
              related: csq,
              op: 'EXISTS' as const,
            } as CorrelatedSubqueryCondition,
          ],
        },
      }));
  });

  // arbB3Targeted: AST with related[] containing a subquery with explicit limit.
  // The headline B3 trigger shape — every related-subquery with limit silently
  // mishandles child-table edits/adds/removes per IVM-PORT-AUDIT-DEEP §B3.
  const arbB3Targeted: fc.Arbitrary<AST> = arbTableWithRels.chain(table => {
    // Force limit=5 (matches Category C in ast-fuzz.ts:620) plus a vary path.
    return fc
      .integer({min: 1, max: 5})
      .chain(lim => {
        const csqArb = arbCorrelatedSubqueryForTable(table, adapted, {
          withLimit: lim,
        });
        if (!csqArb) {
          return fc.constant({table: table.name});
        }
        return csqArb.map(csq => ({
          table: table.name,
          related: [csq],
        }));
      });
  });

  // Composition: arbAstWithTargeted = oneof(7×base, 1×B1, 1×B2, 1×B3).
  // With FUZZ_NUM_RUNS=1000 expect ~100 hits per targeted shape — sufficient
  // to catch regressions if Track 2 fixes break (D-08 verification).
  const arbAstWithTargeted: fc.Arbitrary<AST> = fc.oneof(
    {weight: 7, arbitrary: arbAst},
    {weight: 1, arbitrary: arbB1Targeted},
    {weight: 1, arbitrary: arbB2Targeted},
    {weight: 1, arbitrary: arbB3Targeted},
  );

  return {
    arbAst,
    arbB1Targeted,
    arbB2Targeted,
    arbB3Targeted,
    arbAstWithTargeted,
    tables,
  };
}

// ---------------------------------------------------------------------------
// Smoke entry point — when run directly, draws a few ASTs and prints them.
// ---------------------------------------------------------------------------
if (import.meta.url === `file://${process.argv[1]}`) {
  // Lazy import: only when run directly, not when imported as a library.
  const {schema} = await import('./zero-schema.ts');
  const built = buildArbitraries(schema as never);
  // Use a deterministic seed so the smoke output is stable.
  const baseSamples = fc.sample(built.arbAst, {numRuns: 2, seed: 42});
  const b1Samples = fc.sample(built.arbB1Targeted, {numRuns: 1, seed: 42});
  const b2Samples = fc.sample(built.arbB2Targeted, {numRuns: 1, seed: 42});
  const b3Samples = fc.sample(built.arbB3Targeted, {numRuns: 1, seed: 42});
  for (const [label, samples] of [
    ['base', baseSamples],
    ['B1-targeted', b1Samples],
    ['B2-targeted', b2Samples],
    ['B3-targeted', b3Samples],
  ] as const) {
    for (const [i, ast] of samples.entries()) {
      process.stdout.write(`# arb-ast smoke draw [${label}] ${i + 1}\n`);
      process.stdout.write(JSON.stringify(ast, null, 2));
      process.stdout.write('\n\n');
    }
  }
  process.stdout.write(
    `arb-ast: ${built.tables.length} tables loaded; default shrinker active. ` +
      `Keys exported: arbAst, arbB1Targeted, arbB2Targeted, arbB3Targeted, ` +
      `arbAstWithTargeted, tables.\n`,
  );
}
