# Phase 25: Serialization Format & FFI Optimization

## Goal

Replace JSON string serialization (serde_json::to_string + JSON.parse) with a binary buffer
protocol for AdvanceResult, eliminating serialization overhead on the hot path.

## Implementation

### Rust (advance.rs)

1. **`encode_json_value()`** -- encodes serde_json::Value with type tags matching allBuf protocol
   - Tags: 0=null, 1=i64(8B LE), 2=f64(8B LE), 3=text(u32+bytes), 4=blob, 5=bool, 6=json fallback
2. **`encode_str()`** -- encodes strings as [u16 LE len][UTF-8 bytes]
3. **`encode_advance_result_buf()`** -- encodes full AdvanceResult to binary buffer
4. **`rust_fan_out_buf()`** -- NAPI entry point returning Buffer (replaces JSON String path)
5. **`rust_advance_full_buf()`** -- NAPI entry point for full operator tree, returning Buffer

### TypeScript (decode-advance-buf.ts)

1. **`decodeAdvanceResultBuf()`** -- decodes binary buffer back to DecodedAdvanceResult
2. **`readStr()`** / **`readJsonValue()`** -- helpers for binary protocol parsing
3. Supports all value types including BigInt for large integers

## Binary Format

```
[u32 LE] change_count
[u8]     flags (bit 0 = has_error)
If has_error:
  [u16 LE + bytes] error message
  [u16 LE + bytes] error type
Per RowChange:
  [u8]  change_type (0=add, 1=remove, 2=edit)
  [u16 LE + bytes] query_id
  [u16 LE + bytes] table
  [json_value]     row_key
  [u8]  has_row
  If has_row:
    [u16 LE] col_count
    Per column:
      [u16 LE + bytes] col_name
      [json_value]     col_value
```

## Verification

- cargo test: 141 passed, 0 failed (2 new binary encoder tests)
- pipeline-driver.test.ts: 28 passed, 2 pre-existing failures
- ZERO_DUAL_EXEC=strict: 28 passed, 2 pre-existing (no mismatches)
- fuzz-ivm.test.ts: 6 passed
