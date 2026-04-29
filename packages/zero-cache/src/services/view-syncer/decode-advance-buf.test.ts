import {describe, expect, test} from 'vitest';
import {
  decodeAdvanceChunkBuf,
  decodeAdvanceResultBuf,
} from './decode-advance-buf.ts';

describe('decodeAdvanceResultBuf', () => {
  test('decodes empty result with no changes, no error, no reset', () => {
    // 4 bytes change_count=0, 1 byte flags=0
    const buf = Buffer.alloc(5);
    buf.writeUInt32LE(0, 0); // change_count
    buf[4] = 0; // flags

    const result = decodeAdvanceResultBuf(buf);
    expect(result.changes).toEqual([]);
    expect(result.error).toBeUndefined();
    expect(result.reset_signal).toBeUndefined();
  });

  test('decodes reset_signal from flag bit 2', () => {
    // Build a buffer: change_count=0, flags=0x04 (reset_signal), then u16 len + string
    const reason = 'companion scalar changed for query q1';
    const reasonBytes = Buffer.from(reason, 'utf-8');

    const buf = Buffer.alloc(4 + 1 + 2 + reasonBytes.length);
    let offset = 0;

    buf.writeUInt32LE(0, offset); // change_count
    offset += 4;
    buf[offset++] = 0x04; // flags: bit 2 = reset_signal
    buf.writeUInt16LE(reasonBytes.length, offset); // string length
    offset += 2;
    reasonBytes.copy(buf, offset);

    const result = decodeAdvanceResultBuf(buf);
    expect(result.changes).toEqual([]);
    expect(result.error).toBeUndefined();
    expect(result.reset_signal).toBe(reason);
  });

  test('decodes error + reset_signal together', () => {
    // flags = 0x01 (error) | 0x04 (reset_signal) = 0x05
    const errMsg = 'some error';
    const errType = 'TestError';
    const reason = 'scalar changed';
    const errMsgBuf = Buffer.from(errMsg, 'utf-8');
    const errTypeBuf = Buffer.from(errType, 'utf-8');
    const reasonBuf = Buffer.from(reason, 'utf-8');

    const totalLen =
      4 +
      1 +
      2 +
      errMsgBuf.length +
      2 +
      errTypeBuf.length +
      2 +
      reasonBuf.length;
    const buf = Buffer.alloc(totalLen);
    let offset = 0;

    buf.writeUInt32LE(0, offset);
    offset += 4;
    buf[offset++] = 0x05; // flags: error + reset_signal

    buf.writeUInt16LE(errMsgBuf.length, offset);
    offset += 2;
    errMsgBuf.copy(buf, offset);
    offset += errMsgBuf.length;

    buf.writeUInt16LE(errTypeBuf.length, offset);
    offset += 2;
    errTypeBuf.copy(buf, offset);
    offset += errTypeBuf.length;

    buf.writeUInt16LE(reasonBuf.length, offset);
    offset += 2;
    reasonBuf.copy(buf, offset);

    const result = decodeAdvanceResultBuf(buf);
    expect(result.error).toBe(errMsg);
    expect(result.error_type).toBe(errType);
    expect(result.reset_signal).toBe(reason);
  });

  test('decodes single row change with no reset_signal', () => {
    // Build: change_count=1, flags=0, then one row change
    const parts: Buffer[] = [];

    // Header
    const header = Buffer.alloc(5);
    header.writeUInt32LE(1, 0); // 1 change
    header[4] = 0; // no flags
    parts.push(header);

    // change_type = 0 (add)
    parts.push(Buffer.from([0]));

    // query_id = "q1"
    const qid = Buffer.from('q1', 'utf-8');
    const qidLen = Buffer.alloc(2);
    qidLen.writeUInt16LE(qid.length, 0);
    parts.push(qidLen, qid);

    // table = "issues"
    const tbl = Buffer.from('issues', 'utf-8');
    const tblLen = Buffer.alloc(2);
    tblLen.writeUInt16LE(tbl.length, 0);
    parts.push(tblLen, tbl);

    // row_key: json value tag=6 (json fallback), u32 len, json bytes
    const rowKeyJson = Buffer.from('{"id":"1"}', 'utf-8');
    const rowKeyHeader = Buffer.alloc(5);
    rowKeyHeader[0] = 6; // json fallback tag
    rowKeyHeader.writeUInt32LE(rowKeyJson.length, 1);
    parts.push(rowKeyHeader, rowKeyJson);

    // has_row = 1
    parts.push(Buffer.from([1]));

    // col_count = 1
    const colCount = Buffer.alloc(2);
    colCount.writeUInt16LE(1, 0);
    parts.push(colCount);

    // col: "id" = "1" (text: tag=3, u32 len, bytes)
    const colName = Buffer.from('id', 'utf-8');
    const colNameLen = Buffer.alloc(2);
    colNameLen.writeUInt16LE(colName.length, 0);
    parts.push(colNameLen, colName);

    const colVal = Buffer.from('1', 'utf-8');
    const colValHeader = Buffer.alloc(5);
    colValHeader[0] = 3; // text tag
    colValHeader.writeUInt32LE(colVal.length, 1);
    parts.push(colValHeader, colVal);

    const buf = Buffer.concat(parts);
    const result = decodeAdvanceResultBuf(buf);
    expect(result.changes).toHaveLength(1);
    expect(result.changes[0]).toEqual({
      queryID: 'q1',
      table: 'issues',
      row_key: {id: '1'},
      row: {id: '1'},
      type: 'add',
    });
    expect(result.reset_signal).toBeUndefined();
  });
});

// Phase 31-02: per-chunk decoder tests. Mirrors the chunk byte-format
// emitted by `crate::chunk_encoder::encode_chunk_buf` (Rust 31-01):
//
//   [u32 count LE][u8 flags=0]
//   Per RowChange: [u8 ct][u16 qid_len][qid][u16 tbl_len][tbl]
//                  [json_value row_key]
//                  [u8 has_row][optional u16 col_count + per-col [name, value]]
//
// json_value tags (see decode-advance-buf.ts:23-25 + chunk_encoder.rs comment):
//   0=null, 1=i64(8B LE), 2=f64(8B LE), 3=text(u32 len + bytes),
//   4=blob(u32 len + bytes), 5=bool(u8), 6=json fallback(u32 len + JSON bytes)
//
// We hand-roll the bytes to keep the test independent of the napi binary
// (the parity fuzz test in `streaming-vs-buffered-parity.fuzz.test.ts`
// covers end-to-end Rust→TS roundtrip).
describe('decodeAdvanceChunkBuf', () => {
  test('decodes empty chunk', () => {
    // Header only: [u32 count=0][u8 flags=0]
    const buf = Buffer.from([0, 0, 0, 0, 0]);
    expect(decodeAdvanceChunkBuf(buf)).toEqual([]);
  });

  test('decodes single-row chunk with byte-format identical to buffered per-row encoding', () => {
    // Build {queryID:"q1", table:"t1", row_key:{id:"1"}, row:{id:"1"}, type:"add"}
    // using json-fallback (tag=6) for row_key and text (tag=3) for the column
    // value — exactly the shape `decodeAdvanceResultBuf`'s "single row change"
    // test exercises so the per-row layout is verified by mirror.
    const parts: Buffer[] = [];

    // Header: count=1, flags=0
    const header = Buffer.alloc(5);
    header.writeUInt32LE(1, 0);
    header[4] = 0;
    parts.push(header);

    // change_type = 0 (add)
    parts.push(Buffer.from([0]));

    // query_id = "q1" (u16 LE len + bytes)
    const qid = Buffer.from('q1', 'utf-8');
    const qidLen = Buffer.alloc(2);
    qidLen.writeUInt16LE(qid.length, 0);
    parts.push(qidLen, qid);

    // table = "t1"
    const tbl = Buffer.from('t1', 'utf-8');
    const tblLen = Buffer.alloc(2);
    tblLen.writeUInt16LE(tbl.length, 0);
    parts.push(tblLen, tbl);

    // row_key json fallback (tag=6) {"id":"1"}
    const rowKeyJson = Buffer.from('{"id":"1"}', 'utf-8');
    const rowKeyHeader = Buffer.alloc(5);
    rowKeyHeader[0] = 6;
    rowKeyHeader.writeUInt32LE(rowKeyJson.length, 1);
    parts.push(rowKeyHeader, rowKeyJson);

    // has_row = 1, col_count = 1
    parts.push(Buffer.from([1]));
    const colCount = Buffer.alloc(2);
    colCount.writeUInt16LE(1, 0);
    parts.push(colCount);

    // col "id" = "1" (text tag=3)
    const colName = Buffer.from('id', 'utf-8');
    const colNameLen = Buffer.alloc(2);
    colNameLen.writeUInt16LE(colName.length, 0);
    parts.push(colNameLen, colName);

    const colVal = Buffer.from('1', 'utf-8');
    const colValHeader = Buffer.alloc(5);
    colValHeader[0] = 3;
    colValHeader.writeUInt32LE(colVal.length, 1);
    parts.push(colValHeader, colVal);

    const buf = Buffer.concat(parts);
    const result = decodeAdvanceChunkBuf(buf);
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({
      queryID: 'q1',
      table: 't1',
      type: 'add',
    });
    expect(result[0].row_key).toEqual({id: '1'});
    expect(result[0].row).toEqual({id: '1'});
  });

  test('throws on non-zero flags byte (D-04 forward-compat guard)', () => {
    // Header with flags=0x01 — any non-zero value triggers the throw.
    // Phase 33 telemetry bits will repurpose this byte; v1 decoders must
    // fail loudly rather than silently misinterpret subsequent bytes.
    const buf = Buffer.from([0, 0, 0, 0, 0x01]);
    expect(() => decodeAdvanceChunkBuf(buf)).toThrow(
      /decodeAdvanceChunkBuf: unexpected non-zero flags byte/,
    );
  });
});

// Phase 32-01 (CR-01): i64 decoder safe-integer boundary regression tests.
//
// The pre-fix `readJsonValue` case 1 (i64) constructs `n = hi * 0x100000000 + lo`
// BEFORE comparing against MAX_SAFE_INTEGER / MIN_SAFE_INTEGER — when |hi| >= 2^21
// (= 0x200000) the JS Number arithmetic is already lossy, defeating the guard.
//
// Per CONTEXT D-12..D-14 + RESEARCH §3 P-06/P-07 + §4, the post-fix gate is:
//   if (hi >= 0x200000 || hi <= -0x200000) return BigInt branch
//   else return Number branch (safe range)
//
// These tests pin the type+value contract for both decoders. They PASS on the
// current code today (the BigInt fallback already produces correct values even
// when entered after lossy arithmetic — see RESEARCH §3 P-06). Task 2 (GREEN)
// fortifies the implementation by moving the gate before the lossy multiplication.
function buildSingleI64ChunkBuf(lo: number, hi: number): Buffer {
  // Builds a single-row chunk buffer with one column 'value' carrying an i64.
  // Layout matches `decodeAdvanceChunkBuf` per-chunk encoding:
  //   [u32 count=1][u8 flags=0]
  //   [u8 ct=0 add][u16 qid_len][qid="q1"][u16 tbl_len][tbl="t1"]
  //   [json_value row_key tag=6 + JSON]
  //   [u8 has_row=1][u16 col_count=1]
  //   [u16 col_name_len][col_name="value"]
  //   [u8 tag=1][u32 lo LE][i32 hi LE]
  const parts: Buffer[] = [];

  // Header: count=1, flags=0
  const header = Buffer.alloc(5);
  header.writeUInt32LE(1, 0);
  header[4] = 0;
  parts.push(header);

  // change_type = 0 (add)
  parts.push(Buffer.from([0]));

  // query_id = "q1"
  const qid = Buffer.from('q1', 'utf-8');
  const qidLen = Buffer.alloc(2);
  qidLen.writeUInt16LE(qid.length, 0);
  parts.push(qidLen, qid);

  // table = "t1"
  const tbl = Buffer.from('t1', 'utf-8');
  const tblLen = Buffer.alloc(2);
  tblLen.writeUInt16LE(tbl.length, 0);
  parts.push(tblLen, tbl);

  // row_key json fallback (tag=6) {"id":"1"}
  const rowKeyJson = Buffer.from('{"id":"1"}', 'utf-8');
  const rowKeyHeader = Buffer.alloc(5);
  rowKeyHeader[0] = 6;
  rowKeyHeader.writeUInt32LE(rowKeyJson.length, 1);
  parts.push(rowKeyHeader, rowKeyJson);

  // has_row = 1, col_count = 1
  parts.push(Buffer.from([1]));
  const colCount = Buffer.alloc(2);
  colCount.writeUInt16LE(1, 0);
  parts.push(colCount);

  // col "value" with i64 tag=1
  const colName = Buffer.from('value', 'utf-8');
  const colNameLen = Buffer.alloc(2);
  colNameLen.writeUInt16LE(colName.length, 0);
  parts.push(colNameLen, colName);

  // i64 tag + 8 bytes (lo, hi) little-endian
  const valBuf = Buffer.alloc(9);
  valBuf[0] = 1; // i64 tag
  valBuf.writeUInt32LE(lo >>> 0, 1);
  valBuf.writeInt32LE(hi | 0, 5);
  parts.push(valBuf);

  return Buffer.concat(parts);
}

describe('decodeAdvanceChunkBuf — i64 boundary cases (CR-01)', () => {
  test('case 1: MAX_SAFE_INTEGER (hi=0x001FFFFF, lo=0xFFFFFFFF) returns Number', () => {
    // 9007199254740991 = 2^53 - 1
    const buf = buildSingleI64ChunkBuf(0xffffffff, 0x001fffff);
    const result = decodeAdvanceChunkBuf(buf);
    expect(result).toHaveLength(1);
    const v = result[0].row?.value;
    expect(typeof v).toBe('number');
    expect(v).toBe(Number.MAX_SAFE_INTEGER);
  });

  test('case 2: MAX_SAFE_INTEGER + 1 = 2^53 (hi=0x00200000, lo=0x00000000) returns BigInt', () => {
    // 9007199254740992 = 2^53 — first integer JS Number cannot represent
    // distinctly from 2^53 + 1, so must be BigInt.
    const buf = buildSingleI64ChunkBuf(0x00000000, 0x00200000);
    const result = decodeAdvanceChunkBuf(buf);
    expect(result).toHaveLength(1);
    const v = result[0].row?.value;
    expect(typeof v).toBe('bigint');
    expect(v).toBe(9007199254740992n);
  });

  test('case 3: i64::MAX (hi=0x7FFFFFFF, lo=0xFFFFFFFF) returns BigInt 9223372036854775807n', () => {
    const buf = buildSingleI64ChunkBuf(0xffffffff, 0x7fffffff);
    const result = decodeAdvanceChunkBuf(buf);
    expect(result).toHaveLength(1);
    const v = result[0].row?.value;
    expect(typeof v).toBe('bigint');
    expect(v).toBe(9223372036854775807n);
  });

  test('case 4: i64::MIN (hi=0x80000000 signed=-2147483648, lo=0) returns BigInt -9223372036854775808n', () => {
    // hi = 0x80000000 as int32 = -2147483648 (sign-extended)
    const buf = buildSingleI64ChunkBuf(0x00000000, -2147483648);
    const result = decodeAdvanceChunkBuf(buf);
    expect(result).toHaveLength(1);
    const v = result[0].row?.value;
    expect(typeof v).toBe('bigint');
    expect(v).toBe(-9223372036854775808n);
  });

  test('case 5: MIN_SAFE_INTEGER (hi=0xFFE00001 signed=-2097151, lo=1) returns Number', () => {
    // -9007199254740991 = -(2^53 - 1)
    // Two's complement i64: 0xFFE0_0000_0000_0001
    //   hi = 0xFFE00001 (as int32 = -2097151)
    //   lo = 0x00000001
    const buf = buildSingleI64ChunkBuf(0x00000001, -2097151);
    const result = decodeAdvanceChunkBuf(buf);
    expect(result).toHaveLength(1);
    const v = result[0].row?.value;
    expect(typeof v).toBe('number');
    expect(v).toBe(Number.MIN_SAFE_INTEGER);
  });

  test('case 6: MIN_SAFE_INTEGER - 1 = -2^53 (hi=0xFFE00000 signed=-2097152, lo=0) returns BigInt', () => {
    // -9007199254740992 = -2^53
    // hi = 0xFFE00000 (as int32 = -2097152 = -0x200000)
    // The post-fix `<= -0x200000` (inclusive) gate routes this to BigInt,
    // symmetric with case 2.
    const buf = buildSingleI64ChunkBuf(0x00000000, -2097152);
    const result = decodeAdvanceChunkBuf(buf);
    expect(result).toHaveLength(1);
    const v = result[0].row?.value;
    expect(typeof v).toBe('bigint');
    expect(v).toBe(-9007199254740992n);
  });
});

describe('decodeAdvanceResultBuf — i64 parity (CR-01)', () => {
  test('i64::MAX in buffered result returns BigInt 9223372036854775807n (shared readJsonValue)', () => {
    // Build a buffered AdvanceResult with a single row carrying i64::MAX
    // in column 'value'. Mirrors the chunk-buf test for case 3 to guard
    // against regression in the buffered decoder path that shares
    // `readJsonValue` with the streaming chunk decoder.
    const parts: Buffer[] = [];

    // Header: change_count=1, flags=0
    const header = Buffer.alloc(5);
    header.writeUInt32LE(1, 0);
    header[4] = 0;
    parts.push(header);

    // change_type = 0 (add)
    parts.push(Buffer.from([0]));

    // query_id = "q1"
    const qid = Buffer.from('q1', 'utf-8');
    const qidLen = Buffer.alloc(2);
    qidLen.writeUInt16LE(qid.length, 0);
    parts.push(qidLen, qid);

    // table = "t1"
    const tbl = Buffer.from('t1', 'utf-8');
    const tblLen = Buffer.alloc(2);
    tblLen.writeUInt16LE(tbl.length, 0);
    parts.push(tblLen, tbl);

    // row_key json fallback {"id":"1"}
    const rowKeyJson = Buffer.from('{"id":"1"}', 'utf-8');
    const rowKeyHeader = Buffer.alloc(5);
    rowKeyHeader[0] = 6;
    rowKeyHeader.writeUInt32LE(rowKeyJson.length, 1);
    parts.push(rowKeyHeader, rowKeyJson);

    // has_row=1, col_count=1
    parts.push(Buffer.from([1]));
    const colCount = Buffer.alloc(2);
    colCount.writeUInt16LE(1, 0);
    parts.push(colCount);

    // col "value" = i64::MAX (tag=1, lo=0xFFFFFFFF, hi=0x7FFFFFFF)
    const colName = Buffer.from('value', 'utf-8');
    const colNameLen = Buffer.alloc(2);
    colNameLen.writeUInt16LE(colName.length, 0);
    parts.push(colNameLen, colName);

    const valBuf = Buffer.alloc(9);
    valBuf[0] = 1; // i64 tag
    valBuf.writeUInt32LE(0xffffffff, 1);
    valBuf.writeInt32LE(0x7fffffff, 5);
    parts.push(valBuf);

    const buf = Buffer.concat(parts);
    const result = decodeAdvanceResultBuf(buf);
    expect(result.changes).toHaveLength(1);
    const v = result.changes[0].row?.value;
    expect(typeof v).toBe('bigint');
    expect(v).toBe(9223372036854775807n);
  });
});
