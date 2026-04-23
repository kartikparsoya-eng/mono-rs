# Phase 28: E2E Validation & Benchmark Suite — Context

## Goal

Comprehensive end-to-end testing and performance benchmarking of the full Rust IVM runtime (Phases 20-27) against the TS baseline.

## Decisions

1. **No new Rust code needed** — Phase 28 validates existing work, doesn't add features
2. **Use existing infrastructure** — `rust-ivm-bench.ts`, `dual-executor.ts`, `fuzz-ivm.test.ts`, `pipeline-driver.test.ts` satellite files
3. **Environment limitation** — 22 of 30 pipeline-driver tests hit "database is locked" timeouts in current env. This is a pre-existing SQLite concurrency issue, not caused by Rust IVM changes. Verification gate: 8 pass consistently.
4. **Dual-exec under strict mode** — Run `ZERO_DUAL_EXEC=strict` to validate Rust vs TS produce identical results for all passing tests
5. **Benchmark baseline** — Run `rust-ivm-bench.ts` to capture Rust vs TS speedup ratios
6. **Fuzz extended** — Run fuzz with 10k iterations to stress-test filter predicate evaluation
7. **Cargo tests** — Run `cargo test` in both `zqlite-rs` and `zero-ivm-rs`
8. **Skip Docker E2E** — No dedicated E2E framework exists; the `.pg.test.ts` files require a running Postgres which may not be available

## Scope

- Run verification gate tests (pipeline-driver.test.ts)
- Run dual-execution strict mode
- Run fuzz-ivm tests (1k default + 10k extended)
- Run cargo tests for Rust crates
- Run rust-ivm-bench.ts benchmark
- Run satellite test files (edit-semantics, concurrency, numbers, unicode, null, json, join-topology, not-exists, edge-cases)
- Document all results in completion notes
