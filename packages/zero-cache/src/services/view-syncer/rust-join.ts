import type {Row} from '../../../../zero-protocol/src/data.ts';
import type {CompoundKey} from '../../../../zero-protocol/src/ast.ts';

// Conditional import — Rust bindings may not be available
let rustBindings: typeof import('zero-ivm-rs') | undefined;
try {
  rustBindings = require('zero-ivm-rs');
} catch {
  rustBindings = undefined;
}

/**
 * Returns true if Rust join acceleration is available.
 */
export function isRustJoinAvailable(): boolean {
  return (
    rustBindings !== undefined &&
    typeof rustBindings.rustJoinPushChildBatch === 'function'
  );
}

/**
 * Rust-accelerated buildJoinConstraint.
 * Returns constraint object or undefined (matching TS signature).
 */
export function rustBuildJoinConstraint(
  sourceRow: Row,
  sourceKey: CompoundKey,
  targetKey: CompoundKey,
): Record<string, unknown> | undefined {
  if (!rustBindings) return undefined;
  const result = rustBindings.rustBuildJoinConstraint(
    JSON.stringify(sourceRow),
    sourceKey as unknown as string[],
    targetKey as unknown as string[],
  );
  if (result === null || result === undefined) return undefined;
  return JSON.parse(result);
}

/**
 * Rust-accelerated batch join for pushChildChange hot path.
 * Takes a child row, keys, and materialized parent rows from fetch().
 * Returns { constraint, matchResults } in a single napi call.
 */
export function rustJoinPushChildBatch(
  childRow: Row,
  childKey: CompoundKey,
  parentKey: CompoundKey,
  parentRows: Row[],
): {constraint: Record<string, unknown> | undefined; matchResults: boolean[]} {
  if (!rustBindings) {
    throw new Error('Rust join bindings not available');
  }
  const resultJson = rustBindings.rustJoinPushChildBatch(
    JSON.stringify(childRow),
    childKey as unknown as string[],
    parentKey as unknown as string[],
    parentRows.map(r => JSON.stringify(r)),
  );
  const parsed = JSON.parse(resultJson);
  return {
    constraint: parsed.constraint ?? undefined,
    matchResults: parsed.matchResults,
  };
}

/**
 * Rust-accelerated isJoinMatch (single-pair, for cases outside the batch path).
 */
export function rustIsJoinMatch(
  parentRow: Row,
  parentKey: CompoundKey,
  childRow: Row,
  childKey: CompoundKey,
): boolean {
  if (!rustBindings) return false;
  return rustBindings.rustIsJoinMatch(
    JSON.stringify(parentRow),
    parentKey as unknown as string[],
    JSON.stringify(childRow),
    childKey as unknown as string[],
  );
}

/**
 * Rust-accelerated rowEqualsForCompoundKey.
 */
export function rustRowEqualsForCompoundKey(
  a: Row,
  b: Row,
  key: CompoundKey,
): boolean {
  if (!rustBindings) return false;
  return rustBindings.rustRowEqualsForCompoundKey(
    JSON.stringify(a),
    JSON.stringify(b),
    key as unknown as string[],
  );
}
