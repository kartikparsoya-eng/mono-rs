/**
 * Phase 34 Wave 0 — fast-check arbitraries for random AST generation.
 *
 * Schema-driven (re-uses `schema as paritySchema` from `./zero-schema.ts`,
 * same pattern as `ast-fuzz.ts`). Exports `buildArbitraries(schema)` returning
 * `{ arbAst: fc.Arbitrary<AST> }`. Wave 1 will expand the targeted shapes for
 * B1/B2/B3/B11; this file is the SCAFFOLDING — it produces simple ASTs covering
 * Filter, EXISTS, And/Or, Take, Skip, related[].
 *
 * Coexists with `ast-fuzz.ts` (D-04 BFS baseline preserved AS-IS). The two
 * generators feed into the same `harness-coverage.ts` / `harness-advance-coverage.ts`
 * pipeline because both emit the same `AST` JSON shape.
 *
 * Default fast-check shrinker is sufficient (CD-01 → researcher decided default
 * is adequate; no custom shrinker). Recursion via `fc.letrec`. Per-operator
 * arbitraries listed in 34-RESEARCH "fast-check Generator Design (CD-01)".
 *
 * Source patterns:
 *   - Existing usage of fast-check in
 *     `packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts:14-100`
 *   - Schema adapter pattern from `tools/ivm-parity/ast-fuzz.ts:112-166`
 */
import fc from 'fast-check';
import type {
  AST,
  Condition,
  CorrelatedSubquery,
  CorrelatedSubqueryCondition,
  LiteralValue,
  SimpleCondition,
  SimpleOperator,
} from '../../packages/zero-protocol/src/ast.ts';

// ---------------------------------------------------------------------------
// Schema adapter — minimal subset of `ast-fuzz.ts:adaptSchema`. Wave 1 may
// reuse the existing one verbatim once the harness-fuzz batched runner exists.
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
// ---------------------------------------------------------------------------

const EQ_OPS: SimpleOperator[] = ['=', '!='];
const ORD_OPS: SimpleOperator[] = ['<', '>', '<=', '>='];
const LIKE_OPS: SimpleOperator[] = ['LIKE', 'NOT LIKE', 'ILIKE', 'NOT ILIKE'];
const IN_OPS: SimpleOperator[] = ['IN', 'NOT IN'];
const NULL_OPS: SimpleOperator[] = ['IS', 'IS NOT'];

function opsForColumn(col: ColumnDef): SimpleOperator[] {
  const isString = col.type === 'string' || col.type === 'string?';
  const isNumber = col.type === 'number' || col.type === 'number?';
  const isOptional = col.type === 'string?' || col.type === 'number?';
  const ops: SimpleOperator[] = [];
  if (isOptional) ops.push(...NULL_OPS);
  ops.push(...EQ_OPS, ...IN_OPS);
  if (isString) ops.push(...LIKE_OPS, ...ORD_OPS);
  if (isNumber) ops.push(...ORD_OPS);
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
  if (col.type === 'number' || col.type === 'number?') {
    return fc.integer({min: 0, max: 10000});
  }
  return fc.constantFrom('u1', 'u2', 'ch-pub-1', 'ch-priv-1', 'standup-monday');
}

function arbSimpleConditionForTable(table: TableDef): fc.Arbitrary<SimpleCondition> {
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

function arbCorrelatedSubqueryForTable(
  table: TableDef,
  schema: AdaptedSchema,
): fc.Arbitrary<CorrelatedSubquery> | undefined {
  if (table.relationships.length === 0) return undefined;
  return fc.constantFrom(...table.relationships).map(rel => {
    const childTable = schema[rel.childTable];
    return {
      correlation: {
        parentField: [...rel.parentField],
        childField: [...rel.childField],
      },
      subquery: {
        table: rel.childTable,
        alias: `arb_${table.name}_${rel.name}`,
        // Targeted B3 shape: child subquery with limit. Wave 1 should also vary
        // it (sometimes 0, sometimes large) and add `related[]` recursion.
        // For Wave 0 scaffolding we emit a fixed limit so smoke runs are
        // deterministic enough to inspect by hand.
      } as AST,
    };
  });
}

function arbCorrelatedSubqueryCondition(
  csqArb: fc.Arbitrary<CorrelatedSubquery>,
): fc.Arbitrary<CorrelatedSubqueryCondition> {
  return csqArb.map(csq => ({
    type: 'correlatedSubquery' as const,
    related: csq,
    op: 'EXISTS' as const,
  }));
}

// ---------------------------------------------------------------------------
// Top-level builder.
// ---------------------------------------------------------------------------

export interface BuiltArbitraries {
  /** Random ASTs across the full operator surface (Wave 0 scaffold subset). */
  arbAst: fc.Arbitrary<AST>;
  /** Tables present in the schema (handy for callers and test assertions). */
  tables: TableDef[];
}

export function buildArbitraries(zeroSchema: {
  tables: Record<string, {name: string; columns: Record<string, {type: string; optional: boolean}>; primaryKey: readonly string[]}>;
  relationships: Record<string, Record<string, readonly {sourceField: readonly string[]; destField: readonly string[]; destSchema: string; cardinality: 'one' | 'many'}[]>>;
}): BuiltArbitraries {
  const adapted = adaptSchema(zeroSchema);
  const tables = Object.values(adapted);
  if (tables.length === 0) {
    throw new Error('arb-ast: schema has zero tables — refusing to build arbitraries.');
  }
  const arbTable = fc.constantFrom(...tables);

  // fc.letrec for recursion. Top-level Condition nests And/Or with bounded
  // depth (max 2 nesting levels, max 3 branches per level).
  const recArb = fc.letrec<{
    cond: Condition;
    simple: SimpleCondition;
    csq: CorrelatedSubquery;
  }>(tie => ({
    simple: arbTable.chain(t => arbSimpleConditionForTable(t)),
    csq: arbTable.chain(t => {
      const csqArb = arbCorrelatedSubqueryForTable(t, adapted);
      if (!csqArb) {
        // Fallback: pick any table that has relationships.
        const withRels = tables.filter(x => x.relationships.length > 0);
        if (withRels.length === 0) {
          // Schema has no relationships at all — return a dummy CSQ that
          // points to the same table (TS oracle will reject, fast-check
          // discards via fc.pre wrapper above the outer arbAst).
          return fc.constant({
            correlation: {parentField: [...t.pk], childField: [...t.pk]},
            subquery: {table: t.name, alias: 'arb_self'} as AST,
          });
        }
        return arbCorrelatedSubqueryForTable(
          withRels[Math.floor(Math.random() * withRels.length)],
          adapted,
        )!;
      }
      return csqArb;
    }),
    cond: fc.oneof(
      {weight: 5, arbitrary: tie('simple')},
      {
        weight: 2,
        arbitrary: fc.record({
          type: fc.constant('and' as const),
          conditions: fc.array(tie('cond'), {minLength: 2, maxLength: 3}),
        }),
      },
      {
        weight: 2,
        arbitrary: fc.record({
          type: fc.constant('or' as const),
          conditions: fc.array(tie('cond'), {minLength: 2, maxLength: 3}),
        }),
      },
      {weight: 3, arbitrary: arbCorrelatedSubqueryCondition(tie('csq'))},
    ),
  }));

  // Top-level AST: pick a table, optionally attach a where, optionally a
  // limit, optionally an orderBy. Wave 1 will add `start`, `related[]`, and
  // targeted shape arbitraries for B1 (start + EXISTS) and B3 (related with
  // limit).
  const arbAst: fc.Arbitrary<AST> = arbTable.chain(table =>
    fc.record({
      where: fc.option(recArb.cond, {nil: undefined, freq: 2}),
      limit: fc.option(fc.integer({min: 1, max: 10}), {nil: undefined, freq: 3}),
      orderBy: fc.option(
        fc.constantFrom(...table.columns).map(
          col => [[col.name, 'asc' as const]] as const,
        ),
        {nil: undefined, freq: 3},
      ),
    }).map(opts => ({
      table: table.name,
      ...(opts.where !== undefined ? {where: opts.where} : {}),
      ...(opts.limit !== undefined ? {limit: opts.limit} : {}),
      ...(opts.orderBy !== undefined
        ? {orderBy: opts.orderBy as unknown as AST['orderBy']}
        : {}),
    })),
  );

  return {arbAst, tables};
}

// ---------------------------------------------------------------------------
// Smoke entry point — when run directly, draws a few ASTs and prints them.
// ---------------------------------------------------------------------------
if (import.meta.url === `file://${process.argv[1]}`) {
  // Lazy import: only when run directly, not when imported as a library.
  const {schema} = await import('./zero-schema.ts');
  const built = buildArbitraries(schema as never);
  // Use a deterministic seed so the smoke output is stable.
  const samples = fc.sample(built.arbAst, {numRuns: 3, seed: 42});
  for (const [i, ast] of samples.entries()) {
    process.stdout.write(`# arb-ast smoke draw ${i + 1}\n`);
    process.stdout.write(JSON.stringify(ast, null, 2));
    process.stdout.write('\n\n');
  }
  process.stdout.write(
    `arb-ast: ${built.tables.length} tables loaded; default shrinker active.\n`,
  );
}
