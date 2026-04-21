import type {AST} from '../../../../zero-protocol/src/ast.ts';
import {createSchema} from '../../../../zero-schema/src/builder/schema-builder.ts';
import {
  number,
  string,
  table,
} from '../../../../zero-schema/src/builder/table-builder.ts';

// --- Table Schemas -----------------------------------------------------------

export const jsonItems = table('json_items')
  .columns({
    id: string(),
    payload: string(),
  })
  .primaryKey('id');

export const nullItems = table('null_items')
  .columns({
    id: string(),
    val: string(),
    sortKey: string(),
  })
  .primaryKey('id');

export const nullJoinParents = table('null_join_parents')
  .columns({
    id: string(),
    joinKey: string(),
  })
  .primaryKey('id');

export const nullJoinChildren = table('null_join_children')
  .columns({
    id: string(),
    parentKey: string(),
    label: string(),
  })
  .primaryKey('id');

export const unicodeItems = table('unicode_items')
  .columns({
    id: string(),
    text: string(),
    category: string(),
  })
  .primaryKey('id');

export const numberItems = table('number_items')
  .columns({
    id: string(),
    val: number(),
    label: string(),
  })
  .primaryKey('id');

// --- Client Schemas ----------------------------------------------------------

export const jsonClientSchema = createSchema({tables: [jsonItems]});
export const nullClientSchema = createSchema({
  tables: [nullItems, nullJoinParents, nullJoinChildren],
});
export const unicodeClientSchema = createSchema({tables: [unicodeItems]});
export const numberClientSchema = createSchema({tables: [numberItems]});

// --- JSON Fixture Data -------------------------------------------------------

export const JSON_ROWS = {
  nestedObject: {
    id: 'j1',
    payload: JSON.stringify({a: {b: {c: 42}}, arr: [1, 2, 3]}),
  },
  arrayTop: {
    id: 'j2',
    payload: JSON.stringify([{x: 1}, {x: 2}, {x: 3}]),
  },
  emptyObject: {
    id: 'j3',
    payload: JSON.stringify({}),
  },
  emptyArray: {
    id: 'j4',
    payload: JSON.stringify([]),
  },
  stringEscaped: {
    id: 'j5',
    payload: JSON.stringify({key: 'value with "quotes" and \\backslash'}),
  },
} as const;

// --- NULL Fixture Data -------------------------------------------------------

export const NULL_FILTER_ROWS = [
  {id: 'n1', val: 'alpha', sortKey: 'a'},
  {id: 'n2', val: null, sortKey: null},
  {id: 'n3', val: 'gamma', sortKey: 'c'},
  {id: 'n4', val: null, sortKey: 'b'},
  {id: 'n5', val: 'epsilon', sortKey: null},
] as const;

export const NULL_JOIN_PARENTS_DATA = [
  {id: 'np1', joinKey: 'k1'},
  {id: 'np2', joinKey: null},
  {id: 'np3', joinKey: 'k2'},
] as const;

export const NULL_JOIN_CHILDREN_DATA = [
  {id: 'nc1', parentKey: 'k1', label: 'child-a'},
  {id: 'nc2', parentKey: null, label: 'child-b'},
  {id: 'nc3', parentKey: 'k2', label: 'child-c'},
] as const;

// --- Unicode Fixture Data ----------------------------------------------------

export const UNICODE_ROWS = [
  {id: 'u1', text: '\u{1F600}\u{1F680}\u{1F30D}', category: 'emoji'},
  {id: 'u2', text: '\u4F60\u597D\u4E16\u754C', category: 'cjk'},
  {id: 'u3', text: '\u0645\u0631\u062D\u0628\u0627', category: 'rtl'},
  {id: 'u4', text: 'e\u0301', category: 'combining'},
  {id: 'u5', text: '\u00E9', category: 'precomposed'},
  {
    id: 'u6',
    text: '\u{1F468}\u200D\u{1F469}\u200D\u{1F467}\u200D\u{1F466}',
    category: 'zwj',
  },
  {id: 'u7', text: 'caf\u00E9', category: 'mixed'},
] as const;

// --- Number Fixture Data -----------------------------------------------------

export const NUMBER_ROWS = [
  {id: 'num1', val: Number.MAX_SAFE_INTEGER, label: 'max-safe'},
  {id: 'num2', val: -Number.MAX_SAFE_INTEGER, label: 'neg-max-safe'},
  {id: 'num3', val: 0.1 + 0.2, label: 'float-precision'},
  {id: 'num4', val: -0, label: 'neg-zero'},
  {id: 'num5', val: 1e308, label: 'large-float'},
  {id: 'num6', val: 5e-324, label: 'min-positive'},
  {id: 'num7', val: 42, label: 'normal'},
] as const;

// --- AST Queries -------------------------------------------------------------

export const JSON_ITEMS_QUERY: AST = {
  table: 'json_items',
  orderBy: [['id', 'asc']],
};

export const JSON_ITEMS_FILTER_QUERY: AST = {
  table: 'json_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '=',
    left: {type: 'column', name: 'payload'},
    right: {
      type: 'literal',
      value: JSON.stringify({a: {b: {c: 42}}, arr: [1, 2, 3]}),
    },
  },
};

export const NULL_ITEMS_QUERY: AST = {
  table: 'null_items',
  orderBy: [['sortKey', 'asc']],
};

export const NULL_ITEMS_IS_NULL_QUERY: AST = {
  table: 'null_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: 'IS',
    left: {type: 'column', name: 'val'},
    right: {type: 'literal', value: null},
  },
};

export const NULL_JOIN_QUERY: AST = {
  table: 'null_join_parents',
  orderBy: [['id', 'asc']],
  related: [
    {
      system: 'client',
      correlation: {
        parentField: ['joinKey'],
        childField: ['parentKey'],
      },
      subquery: {
        table: 'null_join_children',
        alias: 'children',
        orderBy: [['id', 'asc']],
      },
    },
  ],
};

export const UNICODE_ITEMS_QUERY: AST = {
  table: 'unicode_items',
  orderBy: [['id', 'asc']],
};

export const UNICODE_FILTER_EMOJI_QUERY: AST = {
  table: 'unicode_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '=',
    left: {type: 'column', name: 'category'},
    right: {type: 'literal', value: 'emoji'},
  },
};

export const UNICODE_LIKE_QUERY: AST = {
  table: 'unicode_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: 'LIKE',
    left: {type: 'column', name: 'text'},
    right: {type: 'literal', value: 'caf%'},
  },
};

export const NUMBER_ITEMS_QUERY: AST = {
  table: 'number_items',
  orderBy: [['val', 'asc']],
};

export const NUMBER_FILTER_QUERY: AST = {
  table: 'number_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '=',
    left: {type: 'column', name: 'val'},
    right: {type: 'literal', value: Number.MAX_SAFE_INTEGER},
  },
};

export const NUMBER_GT_QUERY: AST = {
  table: 'number_items',
  orderBy: [['id', 'asc']],
  where: {
    type: 'simple',
    op: '>',
    left: {type: 'column', name: 'val'},
    right: {type: 'literal', value: 0},
  },
};
