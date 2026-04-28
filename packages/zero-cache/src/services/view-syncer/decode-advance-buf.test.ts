import {describe, expect, test} from 'vitest';
import {decodeAdvanceResultBuf} from './decode-advance-buf.ts';

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
