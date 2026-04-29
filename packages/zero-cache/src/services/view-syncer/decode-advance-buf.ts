/**
 * Decode a binary buffer from rust_fan_out_buf() / rust_advance_full_buf()
 * into an AdvanceResult.
 *
 * Binary format (little-endian):
 *   [u32] change_count
 *   [u8]  flags (bit 0 = has_error)
 *   If has_error:
 *     [u16 + bytes] error message
 *     [u16 + bytes] error type
 *   Per RowChange:
 *     [u8]  change_type (0=add, 1=remove, 2=edit, 3=other)
 *     [u16 + bytes] query_id
 *     [u16 + bytes] table
 *     [json_value]  row_key
 *     [u8]  has_row
 *     If has_row:
 *       [u16] col_count
 *       Per column:
 *         [u16 + bytes] col_name
 *         [json_value]  col_value
 *
 * json_value tags:
 *   0=null, 1=i64(8B LE), 2=f64(8B LE), 3=text(u32 len + bytes),
 *   4=blob(u32 len + bytes), 5=bool(u8), 6=json fallback(u32 len + JSON bytes)
 */

const textDecoder = new TextDecoder();

export const CHANGE_TYPES = ['add', 'remove', 'edit', 'other'] as const;

export type DecodedRowChange = {
  queryID: string;
  table: string;
  row_key: unknown;
  row: Record<string, unknown> | null;
  type: string;
};

export type PipelineTimings = {
  queryID: string;
  buildUs: number;
  warmupUs: number;
  rewindUs: number;
  pushUs: number;
  dedupFilterUs: number;
  totalUs: number;
};

export type AdvanceTimings = {
  totalUs: number;
  pipelineCount: number;
  perPipeline: PipelineTimings[];
};

export type DecodedAdvanceResult = {
  changes: DecodedRowChange[];
  error?: string | undefined;
  error_type?: string | undefined;
  timings?: AdvanceTimings | undefined;
  /** If set, companion scalar subquery value changed — caller must reset pipelines. */
  reset_signal?: string | undefined;
};

export function decodeAdvanceResultBuf(buf: Buffer): DecodedAdvanceResult {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  let offset = 0;

  const changeCount = view.getUint32(offset, true);
  offset += 4;

  const flags = buf[offset++];
  const hasError = (flags & 1) !== 0;

  let error: string | undefined;
  let errorType: string | undefined;
  if (hasError) {
    [error, offset] = readStr(buf, view, offset);
    [errorType, offset] = readStr(buf, view, offset);
  }

  const changes: DecodedRowChange[] = Array.from({length: changeCount});
  for (let i = 0; i < changeCount; i++) {
    const ct = buf[offset++];
    let queryID: string;
    [queryID, offset] = readStr(buf, view, offset);
    let table: string;
    [table, offset] = readStr(buf, view, offset);
    let rowKey: unknown;
    [rowKey, offset] = readJsonValue(buf, view, offset);

    const hasRow = buf[offset++];
    let row: Record<string, unknown> | null = null;
    if (hasRow) {
      const colCount = view.getUint16(offset, true);
      offset += 2;
      row = {};
      for (let c = 0; c < colCount; c++) {
        let colName: string;
        [colName, offset] = readStr(buf, view, offset);
        let colValue: unknown;
        [colValue, offset] = readJsonValue(buf, view, offset);
        row[colName] = colValue;
      }
    }

    changes[i] = {
      queryID,
      table,
      row_key: rowKey,
      row,
      type: CHANGE_TYPES[ct] ?? 'other',
    };
  }

  // Check for timings trailer (flag bit 1)
  const hasTimings = (flags & 2) !== 0;
  let timings: AdvanceTimings | undefined;
  if (hasTimings && offset < buf.byteLength) {
    const totalUs = readU64(view, offset);
    offset += 8;
    const pipelineCount = view.getUint32(offset, true);
    offset += 4;
    const perPipeline: PipelineTimings[] = [];
    for (let p = 0; p < pipelineCount; p++) {
      let queryID: string;
      [queryID, offset] = readStr(buf, view, offset);
      const buildUs = readU64(view, offset);
      offset += 8;
      const warmupUs = readU64(view, offset);
      offset += 8;
      const rewindUs = readU64(view, offset);
      offset += 8;
      const pushUs = readU64(view, offset);
      offset += 8;
      const dedupFilterUs = readU64(view, offset);
      offset += 8;
      const pipelineTotalUs = readU64(view, offset);
      offset += 8;
      perPipeline.push({
        queryID,
        buildUs,
        warmupUs,
        rewindUs,
        pushUs,
        dedupFilterUs,
        totalUs: pipelineTotalUs,
      });
    }
    timings = {totalUs, pipelineCount, perPipeline};
  }

  // Check for reset_signal (flag bit 2)
  const hasResetSignal = (flags & 4) !== 0;
  let reset_signal: string | undefined;
  if (hasResetSignal && offset < buf.byteLength) {
    [reset_signal, offset] = readStr(buf, view, offset);
  }

  return {changes, error, error_type: errorType, timings, reset_signal};
}

function readStr(
  buf: Buffer,
  view: DataView,
  offset: number,
): [string, number] {
  const len = view.getUint16(offset, true);
  offset += 2;
  const s = textDecoder.decode(buf.subarray(offset, offset + len));
  return [s, offset + len];
}

function readU64(view: DataView, offset: number): number {
  const lo = view.getUint32(offset, true);
  const hi = view.getUint32(offset + 4, true);
  return hi * 0x100000000 + lo;
}

/**
 * Decode a per-pipeline chunk buffer from `RustPipelineManager.advanceStreaming`
 * / `hydrateStreaming` / `hydrateQueryStreaming` (Phase 31 streaming surface).
 *
 * Binary format (little-endian) — emitted by `crate::chunk_encoder::encode_chunk_buf`:
 *
 *   [u32] change_count
 *   [u8]  flags  ── Reserved flags byte. MUST be 0 in v1; future versions may
 *                   set bits for per-pipeline timing telemetry. The decoder
 *                   throws on non-zero to force a decoder upgrade rather than
 *                   silently misinterpret subsequent bytes.
 *                   See `.planning/IVM-STREAMING-PLAN.md` §10 Phase D and
 *                   31-CONTEXT.md D-03..D-05.
 *   Per RowChange: identical encoding to `decodeAdvanceResultBuf` — see
 *                  that function for the per-row layout (change_type byte,
 *                  query_id, table, row_key, has_row, optional column map).
 *
 * NOTE: The chunk format does NOT carry error flags or reset_signal trailers.
 * Errors and resets travel as separate `StreamItem` variants
 * (`StreamItem::Error`, `StreamItem::ResetSignal`) outside the chunk encoding.
 * The TS streaming wrapper in `pipeline-driver.ts::#streamChanges` maps those
 * variants to `RustStreamError` / `ResetPipelinesSignal` throws.
 */
export function decodeAdvanceChunkBuf(buf: Buffer): DecodedRowChange[] {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  let offset = 0;

  const changeCount = view.getUint32(offset, true);
  offset += 4;

  const flags = buf[offset++];
  if (flags !== 0) {
    // D-04: fail loud on unknown flags rather than silently misinterpret.
    throw new Error(
      'decodeAdvanceChunkBuf: unexpected non-zero flags byte ' +
        `(got 0x${flags.toString(16)}; need decoder upgrade for new format bits)`,
    );
  }

  const changes: DecodedRowChange[] = Array.from({length: changeCount});
  for (let i = 0; i < changeCount; i++) {
    const ct = buf[offset++];
    let queryID: string;
    [queryID, offset] = readStr(buf, view, offset);
    let table: string;
    [table, offset] = readStr(buf, view, offset);
    let rowKey: unknown;
    [rowKey, offset] = readJsonValue(buf, view, offset);

    const hasRow = buf[offset++];
    let row: Record<string, unknown> | null = null;
    if (hasRow) {
      const colCount = view.getUint16(offset, true);
      offset += 2;
      row = {};
      for (let c = 0; c < colCount; c++) {
        let colName: string;
        [colName, offset] = readStr(buf, view, offset);
        let colValue: unknown;
        [colValue, offset] = readJsonValue(buf, view, offset);
        row[colName] = colValue;
      }
    }

    changes[i] = {
      queryID,
      table,
      row_key: rowKey,
      row,
      type: CHANGE_TYPES[ct] ?? 'other',
    };
  }
  return changes;
}

function readJsonValue(
  buf: Buffer,
  view: DataView,
  offset: number,
): [unknown, number] {
  const tag = buf[offset++];
  switch (tag) {
    case 0: // null
      return [null, offset];
    case 1: {
      // i64. Gate on hi magnitude BEFORE Number arithmetic (CR-01 fix per
      // CONTEXT D-12..D-13 + RESEARCH §3 P-06/P-07 + §4). 2^53 = 2^21 * 2^32,
      // so |hi| >= 2^21 (= 0x200000) means the value exceeds MAX_SAFE_INTEGER
      // and JS Number arithmetic loses precision. Construct BigInt directly
      // in that case so the safe-integer guard never operates on a lossy value.
      // Use `<= -0x200000` (inclusive) for symmetry with `>= 0x200000` so that
      // MIN_SAFE_INTEGER - 1 lands in the BigInt branch (mirror of case 2).
      const lo = view.getUint32(offset, true);
      const hi = view.getInt32(offset + 4, true);
      offset += 8;
      if (hi >= 0x200000 || hi <= -0x200000) {
        return [BigInt(hi) * 0x100000000n + BigInt(lo), offset];
      }
      return [hi * 0x100000000 + lo, offset];
    }
    case 2: // f64
      return [view.getFloat64(offset, true), offset + 8];
    case 3: {
      // text
      const len = view.getUint32(offset, true);
      offset += 4;
      return [
        textDecoder.decode(buf.subarray(offset, offset + len)),
        offset + len,
      ];
    }
    case 4: {
      // blob
      const len = view.getUint32(offset, true);
      offset += 4;
      return [
        Buffer.from(buf.buffer, buf.byteOffset + offset, len),
        offset + len,
      ];
    }
    case 5: // bool
      return [buf[offset++] !== 0, offset];
    case 6: {
      // json fallback
      const len = view.getUint32(offset, true);
      offset += 4;
      const jsonStr = textDecoder.decode(buf.subarray(offset, offset + len));
      return [JSON.parse(jsonStr), offset + len];
    }
    default:
      throw new Error(`Unknown binary tag: ${tag} at offset ${offset - 1}`);
  }
}
