# Plan 06-01 Summary: Initial-Sync napi Overhead Benchmark

## Result

**D-19 Decision: SKIP Phase 6** — napi overhead is 0.4% of flush time (threshold was 5%).

## Benchmark Results

```
10000 rows, 10 cols, batch=50 → 200 flush calls
Rust Statement.run() ×200: 15.636 ms (median of 5)
better-sqlite3 ×200:      10.960 ms (median of 5)
Rust/BS ratio:            0.70x
Per-call napi overhead:   0.3 µs
Estimated napi total:     0.062 ms
napi overhead:            0.4% of flush time
```

## Analysis

- The napi crossing overhead per call is negligible (0.3µs)
- With 200 batch INSERTs (10K rows / 50 per batch), total napi overhead is 0.062ms
- This is 0.4% of the 15.6ms total flush time — well below the 5% threshold
- The Rust INSERT path is slower than better-sqlite3 (0.70x) due to napi object construction overhead per call, but a bulk method would not help since the overhead is in param extraction, not the crossing itself
- **Conclusion:** Building a Rust `bulkInsert` method would save <0.1ms — not worth the complexity

## Files Modified

- `packages/zqlite-rs/bench/rust-vs-ts.cjs` — Added Section 6: Initial-Sync INSERT Pattern

## Commit

`f2e663e92` — bench(06): add initial-sync INSERT pattern benchmark
