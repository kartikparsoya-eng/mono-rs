---
phase: 31-streaming-primitives-and-wrappers
reviewed: 2026-04-29T18:00:00Z
depth: standard
files_reviewed: 9
files_reviewed_list:
  - packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts
  - packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.ts
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.streaming.test.ts
  - packages/zero-cache/src/services/view-syncer/streaming-vs-buffered-parity.fuzz.test.ts
  - packages/zqlite-rs/src/lib.rs
  - packages/zqlite-rs/src/chunk_encoder.rs
  - packages/zqlite-rs/src/pipeline_manager.rs
  - packages/zqlite-rs/src/advance.rs
findings:
  critical: 1
  warning: 6
  info: 5
  total: 12
status: issues_found
---

# Phase 31: Code Review Report

**Reviewed:** 2026-04-29T18:00:00Z
**Depth:** standard
**Files Reviewed:** 9
**Status:** issues_found

## Summary

The Phase 31 streaming surface is largely well-designed and meets the documented contracts (D-03..D-17, COMPAT-02/03, RESEARCH Open Q #1–#4). The chunk format, error class shape, decoder strict-zero validation, `panic::catch_unwind` per-task wrapping, channel sizing rationale (`pipeline_count + 1`), and TS `try/finally` cancel propagation are all correctly implemented.

The single Critical issue is a real correctness bug in `decode-advance-buf.ts`'s `i64` decoder (pre-existing, but exercised by the new chunk decoder path) that returns wrong values for negative `i64`s near the safe-integer boundary. Several Warnings concern subtleties around cancel propagation (the cancel check happens AFTER the per-pipeline lock — meaning a cancel arriving while pipelines are queued may not short-circuit before lock acquisition), the `numChanges = 0` early-return path (companions silently skipped), and a few defensive-programming issues in the TS wrapper (companion fallback ordering, `addQueriesStreaming` registers Rust queries even when caller may want to drain TS-only first).

The cancel-aware path itself is well-engineered. The wall-time-based TEST-01 substitution is reasonable given the rayon completion-vs-cancel race. Forward-compat doc comments and reserved-flags-byte handling are correct.

## Critical Issues

### CR-01: Negative `i64` decoded incorrectly near safe-integer boundary

**File:** `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts:266-275`
**Issue:** The `i64` branch in `readJsonValue` reconstructs the value as `hi * 0x100000000 + lo` where `hi` is read as a signed `Int32` and `lo` as unsigned. But the boundary check `n >= MAX_SAFE_INTEGER || n <= MIN_SAFE_INTEGER` is wrong in two ways:

1. The check uses strict `>=` / `<=`. For `n === Number.MAX_SAFE_INTEGER` (a representable safe integer, value `2^53 - 1`), the code falls into the BigInt branch and returns `BigInt(MAX_SAFE_INTEGER)`. That works for positive values but the returned BigInt path uses `BigInt(hi) * BigInt(0x100000000) + BigInt(lo >>> 0)` — for negative `hi` this is the correct two's complement reconstruction, but contradicts the Number branch above (which just used `hi * 0x100000000 + lo` as a JS Number with `hi` already negative). For values exactly at the boundary, the two branches return values that are conceptually the same but typed differently (Number vs BigInt) — downstream code comparing strict types or doing arithmetic will misbehave.

2. More importantly: for any `i64` whose absolute value exceeds `MAX_SAFE_INTEGER`, the BigInt fallback is `BigInt(hi) * BigInt(0x100000000) + BigInt(lo >>> 0)`. This is correct for negative `hi` (since `hi` is read as signed Int32), but the JS Number reconstruction `hi * 0x100000000 + lo` already coerced `lo` (unsigned) to a Number. For a negative `i64` like `-1` (which encodes as `lo=0xFFFFFFFF, hi=0xFFFFFFFF` in two's complement), the JS reconstruction gives `(-1) * 0x100000000 + 0xFFFFFFFF = -1` (correct), but values like `-2^32` (`lo=0, hi=-1`) give `(-1) * 0x100000000 + 0 = -0x100000000` (correct).

After re-tracing more carefully: the `lo` value is read as `view.getUint32(offset, true)` (unsigned). The BigInt fallback path uses `BigInt(lo >>> 0)` which is redundant (already unsigned). The Number path uses `hi * 0x100000000 + lo` where `lo` is already a non-negative JS Number ≤ 2^32-1. For negative `i64` the math is `(neg_hi) * 2^32 + pos_lo = correct_negative_value` only when `lo` is the low 32 bits of two's complement. This is correct.

**However** the threshold check is still off-by-one and can mis-route values: `n >= MAX_SAFE_INTEGER` rejects `2^53 - 1` itself even though it's representable. More critically, a value like `-2^53 + 1` (= MIN_SAFE_INTEGER + 1) is exactly representable but `n <= MIN_SAFE_INTEGER` triggers BigInt only for `n <= -(2^53 - 1)`. For values in `(-2^53, MIN_SAFE_INTEGER]` the threshold matches but the underlying Number arithmetic has already lost precision. For `n` in approximately `(-2^53, -2^53 + something)`, the JS Number `hi * 0x100000000 + lo` may have already rounded due to f64 precision before the comparison is made. The comparison-after-loss pattern is unsafe.

**Fix:** Compare against the Number boundaries using bitwise reasoning before constructing the Number, or always use BigInt and then narrow:

```typescript
case 1: {
  const lo = view.getUint32(offset, true);
  const hi = view.getInt32(offset + 4, true);
  offset += 8;
  // Check whether the i64 fits in safe integer range based on hi alone.
  // hi must be in [-(2^21), 2^21 - 1) for the value to be < 2^53 - 1
  // (since 2^53 = 2^21 * 2^32). Conservatively: if |hi| >= 2^21, use BigInt.
  if (hi >= 0x200000 || hi < -0x200000) {
    return [
      (BigInt(hi) << 32n) | BigInt(lo),
      offset,
    ];
  }
  // Safe range — Number is precise.
  const n = hi * 0x100000000 + lo;
  return [n, offset];
}
```

Note: this is a **pre-existing bug in `decodeAdvanceResultBuf`** (the function has been stable since Phase 25), but Phase 31's `decodeAdvanceChunkBuf` shares the same `readJsonValue` helper. Since the chunk decoder is now on a hot path for every advance batch, the surface area of the bug grows. Fix should be batched as a single bug-fix (both decoders benefit since they share the helper).

If treated as out-of-scope for Phase 31 (because it's pre-existing in the buffered path), at minimum file a follow-up issue and add a comment noting the precision constraint.

## Warnings

### WR-01: `numChanges = 0` early-return drops companion handling

**File:** `packages/zqlite-rs/src/pipeline_manager.rs:406-415`
**Issue:** The early-return path for empty changes drops the channel immediately and never spawns the coordinator. This is correct for _per-pipeline_ work (no changes → no IVM push needed). But it also bypasses the post-scope companion check at lines 488-514. The buffered `advance_instance` (line 691-737) also early-returns at line 692 with `if changes.is_empty()`, so semantics match — but the comment at line 692-694 simply returns an empty `AdvanceResult`, equivalent behavior.

This is a behavioral parity match with the buffered path, but worth verifying explicitly: companion scalar values can change in a transaction even when `changes.is_empty()` — actually no, because companion checks only inspect tables present in `changes`. So this is correct.

**Fix:** Add a code comment confirming the parity rationale at the early-return:

```rust
if changes.is_empty() {
    // Parity with advance_instance (pipeline_manager.rs:692): no changes
    // means no companion checks possible (changed_tables would be empty).
    let (_tx, rx) = mpsc::sync_channel::<StreamItem>(1);
    ...
}
```

### WR-02: Cancel flag check happens AFTER per-pipeline mutex acquisition contention

**File:** `packages/zqlite-rs/src/pipeline_manager.rs:452-475` (advance_streaming) and `565-584` (hydrate_streaming)
**Issue:** Each rayon task body starts with `if cancel.load(Ordering::Relaxed) { return; }` BEFORE entering `panic::catch_unwind`. Inside the catch_unwind, the first thing it does is `pm.lock().unwrap()`. If many pipelines contend on the same `Mutex<PipelineState>` (they don't — each pipeline has its own Mutex), this would be fine.

However, the cancel observation cadence inside `advance_persistent_pipeline_with_cancel` is once-per-change (line 1288). For a pipeline with no operators, the function returns immediately via `changes_to_row_changes_direct` on line 1248-1262 — that path **does not check `cancel` at all**. A cancel arriving during a `changes_to_row_changes_direct` call would not short-circuit even after entry. Per the documented contract (line 1219-1221), this is acceptable since the function is "byte-identical to advance_persistent_pipeline" except for the per-change loop check, and the no-operators path is fast. But the wall-time test (TEST-01) may not exercise this case.

**Fix:** Add a single cancel check at the top of `advance_persistent_pipeline_with_cancel` before the no-operators branch, OR document explicitly in a code comment that the no-operators path is intentionally non-cancellable for parity:

```rust
pub(crate) fn advance_persistent_pipeline_with_cancel(
    pipeline: &mut PipelineState,
    changes: &[Change],
    db_path: &str,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Vec<RowChange> {
    // Early cancel check — covers the no-operators fast path which doesn't
    // visit the per-change loop below.
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Vec::new();
    }
    ...
}
```

### WR-03: `addQueriesStreaming` registers Rust-eligible queries even when fall-through to TS-only would be acceptable

**File:** `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:1257-1262`
**Issue:** The Phase 2 block unconditionally calls `this.#manager.addQuery(...)` for every rust-eligible query, even though the actual `hydrateStreaming` call happens later in the async generator. If the caller breaks out of the generator before reaching Phase 3b (e.g., an exception in Phase 3a's TS-hydrate loop), the queries have already been registered on the Rust side without being drained. They will still be drained on the next `addQueriesStreaming` invocation, but the resulting state is inconsistent with the buffered `addQueries` path which calls `addQuery` + `hydrate` atomically.

This is a leak risk if errors interrupt Phase 3a, since the `RustPipelineManager` will accumulate queries that never have results consumed. Since pipelines are added to the manager but not to `this.#pipelines` (which only happens in `finalizeQuery`), a subsequent `removeQuery` or `init()` call would still clean them up. But the manager state diverges from the driver state in the interim.

**Fix:** Either:

1. Defer `addQuery` registration to inside the `#streamAddQueries` generator (just before the `hydrateStreaming` call), or
2. Wrap the registration in a try/catch that calls `this.#manager.removeQuery(...)` on each registered query if a downstream throw occurs.

### WR-04: `hydrate_query_streaming` re-locks instance lock multiple times; `pipeline_idx` path drops the lock guard before use

**File:** `packages/zqlite-rs/src/pipeline_manager.rs:633-665`
**Issue:** The `position()` call iterates pipelines while holding `instance` lock guard. It then accesses `&instance.pipelines[i]` to obtain `pm`. The lifetime of `pm` is tied to `instance` (which is valid throughout the closure). But the `pm.lock().map(|g| g.query_id == query_id).unwrap_or(false)` inside `position()` ACQUIRES the per-pipeline mutex once per pipeline checked — and immediately drops it. Then `pm.lock().unwrap()` inside `panic::catch_unwind` re-acquires it.

This is functionally correct but inefficient. More importantly: the `unwrap_or(false)` swallows poisoned-mutex errors silently, meaning if a previous panic poisoned a pipeline mutex, that pipeline is invisible to `position()` and the user gets a confusing "No pipeline for query: X" error rather than the actual poison reason.

**Fix:** Surface the poison error rather than swallowing it:

```rust
let pipeline_idx = instance.pipelines.iter().position(|p| {
    match p.lock() {
        Ok(g) => g.query_id == query_id,
        Err(_) => {
            // poisoned — skip but log
            false
        }
    }
});
```

Or at least add a comment explaining the silent swallow.

### WR-05: `extract_pk_as_json` may return `Object` with **non-deterministic key ordering** when `pk` is empty

**File:** `packages/zqlite-rs/src/pipeline_manager.rs:1336-1338`
**Issue:** When `pk` is empty, the function returns `serde_json::Value::Object(row.clone().into_iter().collect())`. `row` is a `HashMap<String, serde_json::Value>` — iteration order is non-deterministic. The resulting JSON object's key order varies per-call, which downstream may use for change deduplication keying. This is a latent non-determinism risk.

The pk-empty case is unusual (every syncable table has a primary key per Zero's invariants), but the defensive path could mask bugs.

**Fix:** Either assert pk is non-empty or use `BTreeMap` for deterministic ordering:

```rust
if pk.is_empty() {
    debug_assert!(false, "extract_pk_as_json called with empty pk");
    let mut sorted: std::collections::BTreeMap<String, serde_json::Value> =
        row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    return serde_json::Value::Object(sorted.into_iter().collect());
}
```

### WR-06: Debug log file write is in production path of `advance_persistent_pipeline`

**File:** `packages/zqlite-rs/src/advance.rs:1487-1494`
**Issue:** Every call to `advance_persistent_pipeline` (the buffered, non-cancel variant) opens `/tmp/rust_ivm_debug.log` in append mode and writes a debug line. This is a hot path — it runs once per pipeline per advance call. The `OpenOptions::new().create(true).append(true).open(...)` call costs a syscall every invocation. In production, this generates an unbounded growing log file at `/tmp/rust_ivm_debug.log` and adds latency.

This is **not new in Phase 31** but is in scope because the cancel-aware variant `advance_persistent_pipeline_with_cancel` was created as a near-duplicate. Notice that the cancel-aware variant (line 1232+) does NOT include this debug write — so the streaming path is correctly clean. Only the buffered path leaks.

**Fix:** Gate the debug write behind `#[cfg(debug_assertions)]` or remove entirely:

```rust
#[cfg(debug_assertions)]
if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/rust_ivm_debug.log") {
    use std::io::Write;
    let _ = writeln!(f, "[advance_persistent] ...");
}
```

## Info

### IN-01: Test-only static `PIPELINE_INVOCATION_COUNT` is process-wide and can race across tests

**File:** `packages/zqlite-rs/src/advance.rs:1213-1215` and `packages/zqlite-rs/src/pipeline_manager.rs:1601-1604`
**Issue:** `PIPELINE_INVOCATION_COUNT` is a process-wide `AtomicUsize`. The TEST-01 wall-time test (line 1503) and TEST-03 panic test (line 1597) both manipulate this counter. The test comment at line 1601-1602 acknowledges the race: "the counter is process-wide, so this test must not race with other streaming tests... use --test-threads 1 if flaky."

This is documented but worth flagging: future tests using these statics need to coordinate. Consider using `serial_test` crate to enforce serialization, or scope the counter to a single-test wrapper.

**Fix:** Add `#[serial_test::serial]` to both tests using the static, or document the requirement in a module-level comment.

### IN-02: Magic number `pipeline_count.max(1) + 1` could be a named constant

**File:** `packages/zqlite-rs/src/pipeline_manager.rs:428, 544, 614`
**Issue:** The channel capacity formula is repeated three times. The "+1" represents the post-scope ResetSignal/companion-Chunk slot per RESEARCH Open Q #1. While the inline comments explain this, a small helper would prevent drift if the rationale changes:

```rust
fn streaming_channel_capacity(pipeline_count: usize) -> usize {
    // Per RESEARCH Open Q #1: one slot per pipeline + one for post-scope items.
    pipeline_count.max(1) + 1
}
```

### IN-03: `Vec<RowChange>` allocation strategy is generous but unmeasured

**File:** `packages/zqlite-rs/src/chunk_encoder.rs:52`
**Issue:** `Vec::with_capacity(5 + rows.len() * 64)` uses a 64-byte-per-row heuristic. This is documented as "rough heuristic" but unmeasured. For typical Zero rows (few short string columns), 64 bytes underestimates and triggers re-allocation; for narrow tables with single-int columns, it overestimates significantly. Worth profiling once Phase 32's view-syncer integration lands.

**Fix:** No action needed for v1; add to backlog or comment for Phase 33 telemetry.

### IN-04: Forward-compat doc comment is correct but the test invariant could be tightened

**File:** `packages/zqlite-rs/src/chunk_encoder.rs:115-138` (test `chunk_header_flags_byte_is_reserved_zero`)
**Issue:** The test verifies `buf[4] == 0` for a single non-empty encoding. It does NOT test that flags byte stays 0 for chunks with multiple rows, or for chunks with various `change_type` values. The encoder doesn't write to `buf[4]` after the initial push, so the test should be sufficient — but a property test (e.g., quickcheck-style) over arbitrary `Vec<RowChange>` would be more robust.

**Fix:** Optional — defer to Phase 33 hardening pass.

### IN-05: TS `decodeAdvanceChunkBuf` test doesn't exercise companion-style row encoding (no edits/removes)

**File:** `packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts:167-248`
**Issue:** The chunk decoder tests cover empty chunk, single-row add, and the non-zero-flags throw. They don't exercise:

- Multi-row chunks (verifies the loop correctly advances offset between rows)
- `remove` change type (`hasRow=0` path)
- `edit` change type
- Multiple columns in a single row
- All json_value tags (only tags 3 and 6 are exercised)

The TEST-04 parity fuzz at 1000 iterations covers most of these end-to-end, but a focused unit test would localize any bugs to the decoder itself.

**Fix:** Add unit tests for `remove` and `edit` rows. Optional but recommended.

---

_Reviewed: 2026-04-29T18:00:00Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
