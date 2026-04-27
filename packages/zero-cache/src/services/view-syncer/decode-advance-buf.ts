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
const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;
const MIN_SAFE_INTEGER = Number.MIN_SAFE_INTEGER;

const CHANGE_TYPES = ['add', 'remove', 'edit', 'other'] as const;

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

  return {changes, error, error_type: errorType, timings};
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
      // i64
      const lo = view.getUint32(offset, true);
      const hi = view.getInt32(offset + 4, true);
      const n = hi * 0x100000000 + lo;
      offset += 8;
      if (n >= MAX_SAFE_INTEGER || n <= MIN_SAFE_INTEGER) {
        return [BigInt(hi) * BigInt(0x100000000) + BigInt(lo >>> 0), offset];
      }
      return [n, offset];
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
