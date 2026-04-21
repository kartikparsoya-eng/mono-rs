/**
 * Decode a binary buffer from rust_dispatch_poke() into per-VS results.
 *
 * Binary format (little-endian):
 *   [u32] vs_count
 *   Per VS:
 *     [u16 + bytes] vs_id
 *     [u8] has_error (0=ok, 1=error)
 *     If has_error:
 *       [u16 + bytes] error message
 *     Else:
 *       [u32] change_count
 *       Per change: same layout as encode_advance_result_buf changes
 */

import type {DecodedRowChange} from './decode-advance-buf.ts';

const textDecoder = new TextDecoder();
const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;
const MIN_SAFE_INTEGER = Number.MIN_SAFE_INTEGER;

const CHANGE_TYPES = ['add', 'remove', 'edit', 'other'] as const;

export interface VSDispatchResult {
  vsId: string;
  changes: DecodedRowChange[];
  error?: string | undefined;
}

export function decodeDispatchPokeBuf(buf: Buffer): VSDispatchResult[] {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  let offset = 0;

  const vsCount = view.getUint32(offset, true);
  offset += 4;

  const results: VSDispatchResult[] = Array.from({length: vsCount});
  for (let i = 0; i < vsCount; i++) {
    let vsId: string;
    [vsId, offset] = readStr(buf, view, offset);

    const hasError = buf[offset++];
    if (hasError) {
      let errorMsg: string;
      [errorMsg, offset] = readStr(buf, view, offset);
      results[i] = {vsId, changes: [], error: errorMsg};
    } else {
      const changeCount = view.getUint32(offset, true);
      offset += 4;

      const changes: DecodedRowChange[] = Array.from({length: changeCount});
      for (let c = 0; c < changeCount; c++) {
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
          for (let col = 0; col < colCount; col++) {
            let colName: string;
            [colName, offset] = readStr(buf, view, offset);
            let colValue: unknown;
            [colValue, offset] = readJsonValue(buf, view, offset);
            row[colName] = colValue;
          }
        }

        changes[c] = {
          queryID,
          table,
          row_key: rowKey,
          row,
          type: CHANGE_TYPES[ct] ?? 'other',
        };
      }
      results[i] = {vsId, changes};
    }
  }

  return results;
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
