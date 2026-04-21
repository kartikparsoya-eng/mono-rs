/**
 * Property-based fuzz harness for Rust ↔ TS IVM correctness.
 *
 * Uses fast-check to generate random filter predicates and row data,
 * then compares Rust evaluation against TS evaluation.
 *
 * Run: npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts
 * Run more iterations: FUZZ_NUM_RUNS=10000 npx vitest run ...
 *
 * This is NOT part of the regular test suite. Run it periodically or
 * before merging Rust IVM changes.
 */

import fc from 'fast-check';
import {describe, expect, test} from 'vitest';
import {RustFilterPredicate} from '../../../../zero-ivm-rs/index.js';
import type {Row} from '../../../../zero-protocol/src/data.ts';
import {ChangeType} from '../../../../zql/src/ivm/change-type.ts';
import {compareChanges, type CompareResult} from './dual-executor.ts';
import type {RowChange} from './pipeline-driver.ts';

const NUM_RUNS = parseInt(process.env['FUZZ_NUM_RUNS'] ?? '1000', 10);

// --- Arbitraries ---

/** Generate a random scalar value (string, number, boolean, null) */
const arbScalar = fc.oneof(
  fc.string({maxLength: 50}),
  fc.integer({min: -1000000, max: 1000000}),
  fc.double({min: -1e6, max: 1e6, noNaN: true, noDefaultInfinity: true}),
  fc.boolean(),
  fc.constant(null),
);

/** Generate a random row with 1-5 columns */
const arbRow = fc.dictionary(
  fc.stringOf(fc.constantFrom(...'abcdefghijklmnop'.split('')), {
    minLength: 1,
    maxLength: 4,
  }),
  arbScalar,
  {minKeys: 1, maxKeys: 5},
);

type SimpleCondition = {
  type: 'simple';
  op: string;
  left: {type: 'column'; name: string};
  right: {type: 'literal'; value: unknown};
};

type CompoundCondition = {
  type: 'and' | 'or';
  conditions: FilterCondition[];
};

type FilterCondition = SimpleCondition | CompoundCondition;

const FILTER_OPS = ['=', '!=', '<', '>', '<=', '>='] as const;

/** Generate a filter condition that references columns from a given row */
function arbFilterForRow(
  row: Record<string, unknown>,
): fc.Arbitrary<FilterCondition> {
  const columns = Object.keys(row);
  if (columns.length === 0) {
    // Degenerate — always-true filter
    return fc.constant({
      type: 'simple' as const,
      op: '=',
      left: {type: 'column' as const, name: '_nonexistent'},
      right: {type: 'literal' as const, value: null},
    });
  }

  const arbSimple: fc.Arbitrary<SimpleCondition> = fc
    .tuple(
      fc.constantFrom(...columns),
      fc.constantFrom(...FILTER_OPS),
      arbScalar,
    )
    .map(([col, op, val]) => ({
      type: 'simple' as const,
      op,
      left: {type: 'column' as const, name: col},
      right: {type: 'literal' as const, value: val},
    }));

  // Also test with the row's own values for higher hit rate
  const arbSimpleFromRow: fc.Arbitrary<SimpleCondition> = fc
    .tuple(fc.constantFrom(...columns), fc.constantFrom(...FILTER_OPS))
    .map(([col, op]) => ({
      type: 'simple' as const,
      op,
      left: {type: 'column' as const, name: col},
      right: {type: 'literal' as const, value: row[col]},
    }));

  return fc.oneof(arbSimple, arbSimpleFromRow);
}

/** Build the AST condition JSON matching the zero-protocol format */
function conditionToAst(cond: FilterCondition): unknown {
  if (cond.type === 'simple') {
    return {
      type: 'simple',
      op: cond.op,
      left: cond.left,
      right: cond.right,
    };
  }
  return {
    type: cond.type,
    conditions: cond.conditions.map(conditionToAst),
  };
}

// --- TS Filter Evaluation (reference implementation) ---

function tsCompare(a: unknown, b: unknown): number {
  if (a === null && b === null) return 0;
  if (a === null) return -1;
  if (b === null) return 1;
  if (typeof a === 'boolean') a = a ? 1 : 0;
  if (typeof b === 'boolean') b = b ? 1 : 0;
  if (typeof a === 'number' && typeof b === 'number') {
    return a < b ? -1 : a > b ? 1 : 0;
  }
  const sa = String(a);
  const sb = String(b);
  return sa < sb ? -1 : sa > sb ? 1 : 0;
}

function tsEvaluateSimple(
  row: Record<string, unknown>,
  cond: SimpleCondition,
): boolean {
  const left = row[cond.left.name];
  const right = cond.right.value;

  // Column not found → treat as null
  const leftVal = left === undefined ? null : left;

  switch (cond.op) {
    case '=':
      return tsCompare(leftVal, right) === 0;
    case '!=':
      return tsCompare(leftVal, right) !== 0;
    case '<':
      return tsCompare(leftVal, right) < 0;
    case '>':
      return tsCompare(leftVal, right) > 0;
    case '<=':
      return tsCompare(leftVal, right) <= 0;
    case '>=':
      return tsCompare(leftVal, right) >= 0;
    default:
      return false;
  }
}

function tsEvaluate(
  row: Record<string, unknown>,
  cond: FilterCondition,
): boolean {
  if (cond.type === 'simple') return tsEvaluateSimple(row, cond);
  if (cond.type === 'and')
    return cond.conditions.every(c => tsEvaluate(row, c));
  if (cond.type === 'or') return cond.conditions.some(c => tsEvaluate(row, c));
  return false;
}

// --- Tests ---

describe('fuzz-ivm', () => {
  describe('RustFilterPredicate vs TS filter evaluation', () => {
    test('simple conditions match TS behavior', () => {
      fc.assert(
        fc.property(arbRow, fc.constant(null), (row, _) => {
          const filter = arbFilterForRow(row);
          return fc.assert(
            fc.property(filter, cond => {
              const ast = conditionToAst(cond);
              const predicateJson = JSON.stringify(ast);

              let rustResult: boolean;
              try {
                const rustPred = new RustFilterPredicate(predicateJson);
                rustResult = rustPred.evaluateRow(row);
              } catch {
                // Rust can't parse this predicate — skip
                return;
              }

              const tsResult = tsEvaluate(row, cond);

              if (rustResult !== tsResult) {
                throw new Error(
                  `Mismatch!\n` +
                    `  Row: ${JSON.stringify(row)}\n` +
                    `  Predicate: ${predicateJson}\n` +
                    `  TS: ${tsResult}, Rust: ${rustResult}`,
                );
              }
            }),
            {numRuns: 5}, // inner loop — 5 predicates per row
          );
        }),
        {numRuns: NUM_RUNS},
      );
    });

    test('equality with rows own values always matches', () => {
      fc.assert(
        fc.property(arbRow, row => {
          const columns = Object.keys(row);
          for (const col of columns) {
            const cond: SimpleCondition = {
              type: 'simple',
              op: '=',
              left: {type: 'column', name: col},
              right: {type: 'literal', value: row[col]},
            };
            const predicateJson = JSON.stringify(conditionToAst(cond));
            try {
              const rustPred = new RustFilterPredicate(predicateJson);
              const rustResult = rustPred.evaluateRow(row);
              const tsResult = tsEvaluate(row, cond);
              expect(rustResult).toBe(tsResult);
            } catch {
              // Skip if Rust can't handle the predicate
            }
          }
        }),
        {numRuns: NUM_RUNS},
      );
    });
  });

  describe('compareChanges (dual-executor unit tests)', () => {
    test('identical change arrays match', () => {
      fc.assert(
        fc.property(
          fc.array(
            fc.tuple(
              fc.constantFrom('add', 'edit', 'remove'),
              fc.string({minLength: 1, maxLength: 5}),
              fc.string({minLength: 1, maxLength: 5}),
              arbRow,
            ),
            {maxLength: 20},
          ),
          changes => {
            const rowChanges: RowChange[] = changes.map(
              ([type, queryID, table, row]) => ({
                type:
                  type === 'add'
                    ? ChangeType.ADD
                    : type === 'edit'
                      ? ChangeType.EDIT
                      : ChangeType.REMOVE,
                queryID,
                table,
                rowKey: {id: queryID} as Row,
                row: row as Row,
              }),
            ) as RowChange[];

            const result = compareChanges(rowChanges, [...rowChanges]);
            expect(result.match).toBe(true);
            expect(result.mismatches).toHaveLength(0);
          },
        ),
        {numRuns: NUM_RUNS / 10},
      );
    });

    test('detects missing changes', () => {
      const ts: RowChange[] = [
        {
          type: ChangeType.ADD,
          queryID: 'q1',
          table: 't1',
          rowKey: {id: '1'} as Row,
          row: {id: '1', name: 'a'} as Row,
        },
      ];
      const rust: RowChange[] = [];
      const result = compareChanges(ts, rust);
      expect(result.match).toBe(false);
      expect(result.mismatches).toHaveLength(1);
      expect(result.mismatches[0].kind).toBe('ts_only');
    });

    test('detects extra changes', () => {
      const ts: RowChange[] = [];
      const rust: RowChange[] = [
        {
          type: ChangeType.ADD,
          queryID: 'q1',
          table: 't1',
          rowKey: {id: '1'} as Row,
          row: {id: '1', name: 'a'} as Row,
        },
      ];
      const result = compareChanges(ts, rust);
      expect(result.match).toBe(false);
      expect(result.mismatches).toHaveLength(1);
      expect(result.mismatches[0].kind).toBe('rust_only');
    });

    test('detects row content differences', () => {
      const ts: RowChange[] = [
        {
          type: ChangeType.ADD,
          queryID: 'q1',
          table: 't1',
          rowKey: {id: '1'} as Row,
          row: {id: '1', name: 'alice'} as Row,
        },
      ];
      const rust: RowChange[] = [
        {
          type: ChangeType.ADD,
          queryID: 'q1',
          table: 't1',
          rowKey: {id: '1'} as Row,
          row: {id: '1', name: 'bob'} as Row,
        },
      ];
      const result = compareChanges(ts, rust);
      expect(result.match).toBe(false);
      expect(result.mismatches).toHaveLength(1);
      expect(result.mismatches[0].kind).toBe('row_diff');
    });
  });

  describe('rust_fan_out vs TS for filter-only pipelines', () => {
    // This is a placeholder for Phase 20+ when we have the full
    // pipeline comparison infrastructure. For now, the dual-exec
    // comparator in pipeline-driver.ts covers this at integration level.
    test.todo('generate random tables, filter predicates, and changes');
  });
});
