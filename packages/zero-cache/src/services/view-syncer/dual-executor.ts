/**
 * Dual-Execution Comparator for Rust ↔ TS IVM correctness verification.
 *
 * When enabled (ZERO_DUAL_EXEC=1), both TS and Rust paths run on every
 * advance/hydration. Results are normalized and deep-compared. Mismatches
 * are logged with full detail; in strict mode (ZERO_DUAL_EXEC=strict)
 * they throw.
 *
 * This is the primary safety net for v4.0 Rust porting.
 */

import type {LogContext} from '@rocicorp/logger';
import {type JSONValue} from '../../../../shared/src/json.ts';
import type {Row} from '../../../../zero-protocol/src/data.ts';
import {ChangeType} from '../../../../zql/src/ivm/change-type.ts';
import {getOrCreateCounter} from '../../observability/metrics.ts';
import type {RowChange} from './pipeline-driver.ts';

// --- Configuration ---

function getDualExecEnv(): string {
  return process.env['ZERO_DUAL_EXEC'] ?? '';
}

/** Whether dual execution is enabled at all. */
export function isDualExecEnabled(): boolean {
  const env = getDualExecEnv();
  return env === '1' || env === 'strict' || env === 'log';
}

/** Whether mismatches throw (strict) or just log (log/1). */
export function isDualExecStrict(): boolean {
  return getDualExecEnv() === 'strict';
}

// --- Normalized change format for comparison ---

interface NormalizedChange {
  type: 'add' | 'edit' | 'remove';
  queryID: string;
  table: string;
  /** Sorted JSON of rowKey for stable comparison */
  rowKeyJson: string;
  /** Sorted JSON of full row for stable comparison */
  rowJson: string;
}

// --- Stats ---

const dualExecComparisons = getOrCreateCounter(
  'sync',
  'ivm.dual-exec-comparisons',
  'Number of dual-exec Rust vs TS comparisons',
);

const dualExecMismatches = getOrCreateCounter(
  'sync',
  'ivm.dual-exec-mismatches',
  'Number of dual-exec Rust vs TS mismatches',
);

export interface DualExecStats {
  comparisons: number;
  mismatches: number;
  lastMismatchDetail: string | null;
}

const stats: DualExecStats = {
  comparisons: 0,
  mismatches: 0,
  lastMismatchDetail: null,
};

export function getDualExecStats(): Readonly<DualExecStats> {
  return {...stats};
}

export function resetDualExecStats(): void {
  stats.comparisons = 0;
  stats.mismatches = 0;
  stats.lastMismatchDetail = null;
}

// --- Normalization ---

function changeTypeToString(type: ChangeType): 'add' | 'edit' | 'remove' {
  switch (type) {
    case ChangeType.ADD:
      return 'add';
    case ChangeType.EDIT:
      return 'edit';
    case ChangeType.REMOVE:
      return 'remove';
    default:
      return 'remove';
  }
}

function sortedJsonStringify(obj: Row | JSONValue): string {
  if (obj === null || obj === undefined) return 'null';
  if (typeof obj !== 'object') return JSON.stringify(obj);
  if (Array.isArray(obj)) {
    return '[' + obj.map(v => sortedJsonStringify(v)).join(',') + ']';
  }
  const keys = Object.keys(obj).sort();
  return (
    '{' +
    keys
      .map(
        k =>
          JSON.stringify(k) +
          ':' +
          sortedJsonStringify((obj as Record<string, JSONValue>)[k]),
      )
      .join(',') +
    '}'
  );
}

function normalizeChange(change: RowChange): NormalizedChange {
  return {
    type: changeTypeToString(change.type),
    queryID: change.queryID,
    table: change.table,
    rowKeyJson: sortedJsonStringify(change.rowKey as JSONValue),
    rowJson: sortedJsonStringify(change.row as JSONValue),
  };
}

/** Sort key for deterministic ordering of changes. */
function sortKey(c: NormalizedChange): string {
  return `${c.queryID}\0${c.table}\0${c.type}\0${c.rowKeyJson}`;
}

function normalizeAndSort(changes: RowChange[]): NormalizedChange[] {
  return changes.map(normalizeChange).sort((a, b) => {
    const ka = sortKey(a);
    const kb = sortKey(b);
    return ka < kb ? -1 : ka > kb ? 1 : 0;
  });
}

// --- Comparison ---

export interface CompareResult {
  match: boolean;
  tsCount: number;
  rustCount: number;
  mismatches: MismatchDetail[];
}

interface MismatchDetail {
  kind: 'ts_only' | 'rust_only' | 'row_diff';
  queryID: string;
  table: string;
  type: string;
  rowKey: string;
  tsRow?: string;
  rustRow?: string;
}

/**
 * Compare TS and Rust RowChange arrays for equivalence.
 * Both are normalized (sorted by queryID+table+type+rowKey) before comparison.
 */
export function compareChanges(
  tsChanges: RowChange[],
  rustChanges: RowChange[],
): CompareResult {
  const tsNorm = normalizeAndSort(tsChanges);
  const rustNorm = normalizeAndSort(rustChanges);
  const mismatches: MismatchDetail[] = [];

  let ti = 0;
  let ri = 0;

  while (ti < tsNorm.length && ri < rustNorm.length) {
    const ts = tsNorm[ti];
    const rust = rustNorm[ri];
    const tsk = sortKey(ts);
    const rsk = sortKey(rust);

    if (tsk === rsk) {
      // Same change identity — compare row content
      if (ts.rowJson !== rust.rowJson) {
        mismatches.push({
          kind: 'row_diff',
          queryID: ts.queryID,
          table: ts.table,
          type: ts.type,
          rowKey: ts.rowKeyJson,
          tsRow: ts.rowJson,
          rustRow: rust.rowJson,
        });
      }
      ti++;
      ri++;
    } else if (tsk < rsk) {
      mismatches.push({
        kind: 'ts_only',
        queryID: ts.queryID,
        table: ts.table,
        type: ts.type,
        rowKey: ts.rowKeyJson,
        tsRow: ts.rowJson,
      });
      ti++;
    } else {
      mismatches.push({
        kind: 'rust_only',
        queryID: rust.queryID,
        table: rust.table,
        type: rust.type,
        rowKey: rust.rowKeyJson,
        rustRow: rust.rowJson,
      });
      ri++;
    }
  }

  // Remaining TS-only
  while (ti < tsNorm.length) {
    const ts = tsNorm[ti++];
    mismatches.push({
      kind: 'ts_only',
      queryID: ts.queryID,
      table: ts.table,
      type: ts.type,
      rowKey: ts.rowKeyJson,
      tsRow: ts.rowJson,
    });
  }

  // Remaining Rust-only
  while (ri < rustNorm.length) {
    const rust = rustNorm[ri++];
    mismatches.push({
      kind: 'rust_only',
      queryID: rust.queryID,
      table: rust.table,
      type: rust.type,
      rowKey: rust.rowKeyJson,
      rustRow: rust.rowJson,
    });
  }

  return {
    match: mismatches.length === 0,
    tsCount: tsNorm.length,
    rustCount: rustNorm.length,
    mismatches,
  };
}

// --- Integration helper ---

/**
 * Materialize a generator of RowChange | 'yield' into an array of RowChange,
 * filtering out yield sentinels.
 */
export function materializeChanges(
  iterable: Iterable<RowChange | 'yield'>,
): RowChange[] {
  const result: RowChange[] = [];
  for (const item of iterable) {
    if (item !== 'yield') {
      result.push(item);
    }
  }
  return result;
}

/**
 * Run dual-execution comparison and handle results.
 * Called after both TS and Rust paths have produced their changes.
 *
 * @param label - A label for the comparison (e.g. "advance" or "hydrate")
 * @param tsChanges - Changes from TS path (materialized)
 * @param rustChanges - Changes from Rust path (materialized)
 * @param lc - Logger
 * @returns The TS changes (always the source of truth)
 */
export function dualExecCompare(
  label: string,
  tsChanges: RowChange[],
  rustChanges: RowChange[],
  lc: LogContext,
): RowChange[] {
  stats.comparisons++;
  dualExecComparisons.add(1);

  const result = compareChanges(tsChanges, rustChanges);

  if (result.match) {
    lc.debug?.(
      `[dual-exec] ${label}: MATCH (${result.tsCount} ts, ${result.rustCount} rust changes)`,
    );
    return tsChanges;
  }

  stats.mismatches++;
  dualExecMismatches.add(1);

  const detail = formatMismatches(label, result);
  stats.lastMismatchDetail = detail;

  if (isDualExecStrict()) {
    lc.error?.(detail);
    throw new DualExecMismatchError(detail);
  } else {
    lc.warn?.(detail);
  }

  // Always return TS as the source of truth
  return tsChanges;
}

function formatMismatches(label: string, result: CompareResult): string {
  const lines = [
    `[dual-exec] ${label}: MISMATCH — ${result.mismatches.length} difference(s) (${result.tsCount} ts, ${result.rustCount} rust)`,
  ];

  const maxDetail = 20; // Don't flood logs
  for (let i = 0; i < Math.min(result.mismatches.length, maxDetail); i++) {
    const m = result.mismatches[i];
    switch (m.kind) {
      case 'ts_only':
        lines.push(
          `  [${i}] TS-only: ${m.type} ${m.queryID}/${m.table} key=${m.rowKey}`,
        );
        break;
      case 'rust_only':
        lines.push(
          `  [${i}] Rust-only: ${m.type} ${m.queryID}/${m.table} key=${m.rowKey}`,
        );
        break;
      case 'row_diff':
        lines.push(
          `  [${i}] Row diff: ${m.type} ${m.queryID}/${m.table} key=${m.rowKey}`,
        );
        lines.push(`    TS:   ${m.tsRow}`);
        lines.push(`    Rust: ${m.rustRow}`);
        break;
    }
  }
  if (result.mismatches.length > maxDetail) {
    lines.push(`  ... and ${result.mismatches.length - maxDetail} more`);
  }

  return lines.join('\n');
}

export class DualExecMismatchError extends Error {
  constructor(detail: string) {
    super(detail);
    this.name = 'DualExecMismatchError';
  }
}
