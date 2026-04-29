//! Per-chunk binary encoder for streaming advance/hydrate.
//!
//! Chunk format (little-endian, per IVM-STREAMING-PLAN.md §10 Phase D and
//! 31-CONTEXT.md D-03..D-05):
//!   [u32] change_count
//!   [u8]  flags  ── Reserved flags byte. Must be 0 in v1.
//!                  Future versions (Phase 33) may set bits for per-pipeline
//!                  timing telemetry; see `IVM-STREAMING-PLAN.md §10 Phase D`.
//!   Per RowChange: identical encoding to encode_advance_result_buf —
//!                  see crate::advance::encode_advance_result_buf for the
//!                  per-row layout (change_type byte, query_id, table,
//!                  row_key, has_row, optional column map).
//!
//! NOTE: chunk format does NOT include error flags or reset_signal trailers.
//! Errors and resets travel as separate StreamItem variants outside the
//! chunk encoding.

use crate::advance::RowChange;

/// One item in the streaming channel (Rust → JS).
/// Matches the napi NextChunkValue {kind} discriminator on the TS side.
pub enum StreamItem {
    /// An encoded chunk buffer ready to ship to JS.
    Chunk(Vec<u8>),
    /// A companion scalar value changed; downstream must reset all pipelines.
    /// String is the human-readable reason (mirrors AdvanceResult::reset_signal).
    ResetSignal(String),
    /// A failure inside the rayon scope. (message, kind).
    /// kind ∈ {"panic", "rayon_error", "channel_closed"} (matches D-10/D-11).
    Error(String, String),
}

#[cfg(test)]
impl std::fmt::Debug for StreamItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamItem::Chunk(b) => write!(f, "Chunk(len={})", b.len()),
            StreamItem::ResetSignal(r) => write!(f, "ResetSignal({})", r),
            StreamItem::Error(m, k) => write!(f, "Error(\"{}\", \"{}\")", m, k),
        }
    }
}

/// Encode a per-pipeline chunk buffer (header + per-row encoding).
/// REUSES per-row encoders from `crate::advance` to guarantee byte-for-byte
/// row-encoding parity with `encode_advance_result_buf` (COMPAT-03 — only
/// the framing differs).
pub fn encode_chunk_buf(_rows: &[RowChange]) -> Vec<u8> {
    unimplemented!("Wave 1 Task 2 (GREEN) implements this");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_header_format() {
        // Zero rows → exactly 5 bytes: [u32 count=0 LE][u8 flags=0]
        let buf = encode_chunk_buf(&[]);
        assert_eq!(
            buf,
            vec![0, 0, 0, 0, 0],
            "Expected [u32 count=0 LE][u8 flags=0] = 5 bytes; got {:?}",
            buf
        );
    }

    #[test]
    fn chunk_header_flags_byte_is_reserved_zero() {
        // Any non-empty encoding must keep buf[4] (flags) == 0 in v1.
        let row = RowChange {
            change_type: "add".to_string(),
            query_id: "q1".to_string(),
            table: "t1".to_string(),
            row_key: serde_json::json!({"id":1}),
            row: Some(std::collections::HashMap::new()),
        };
        let buf = encode_chunk_buf(&[row]);
        assert!(buf.len() >= 5, "Buf too short: {}", buf.len());
        assert_eq!(
            buf[4], 0u8,
            "Reserved flags byte must be 0 in v1, got 0x{:x}",
            buf[4]
        );
        // count u32 LE must be 1
        assert_eq!(
            &buf[0..4],
            &[1u8, 0, 0, 0],
            "count u32 LE must be 1, got {:?}",
            &buf[0..4]
        );
    }
}
