# Phase 25 Summary: Serialization Format & FFI Optimization

## What Was Done

Added a binary buffer protocol for Rust -> TS result transfer, replacing JSON string serialization on the advance/fan-out hot path.

### Rust Side (advance.rs)

- `encode_row_changes_to_buffer()` — encodes Vec<RowChange> into compact binary format with type tags
- `encode_json_value()` — handles null/i64/f64/text/blob/bool/json fallback with type discrimination
- `encode_str()` — length-prefixed UTF-8 string encoding (u16 LE + bytes)
- `rust_fan_out_buf()` — NAPI function returning Buffer instead of JSON String
- 2 new cargo tests for binary encoder round-trip correctness

### TypeScript Side (decode-advance-buf.ts)

- `decodeAdvanceResultBuf()` — decodes binary buffer back to DecodedAdvanceResult
- `readStr()` / `readJsonValue()` — binary protocol parsing helpers
- Handles all SQLite types: null, i64, f64, text, blob, bool, JSON fallback
- BigInt support for integers outside Number.MAX_SAFE_INTEGER range

### Binary Format

Compact row-oriented format: change_count + flags + error? + per-change (type + queryID + table + row_key + row?). Each value uses type tags (0-6) with fixed or length-prefixed encoding.

## NOT YET WIRED

The binary path (`rust_fan_out_buf` / `decodeAdvanceResultBuf`) is implemented but NOT yet called from pipeline-driver.ts. The existing `rust_fan_out` JSON path is still used. Wiring happens in Phase 26.

## Verification

- cargo test (zqlite-rs): 141 passed, 0 failed
- pipeline-driver.test.ts: 28 passed, 2 pre-existing failures
- ZERO_DUAL_EXEC=strict: 28 passed, 2 pre-existing failures (no mismatches)
- fuzz-ivm.test.ts: 6 passed, 1 todo

## Files Changed

- `packages/zqlite-rs/src/advance.rs` — binary encoder + rust_fan_out_buf NAPI
- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` — NEW: TS binary decoder
