# Requirements — v4.0 Parallel IVM Runtime

## Operator Runtime

- [x] **OPR-01**: Unified Rust `Operator` trait with `fetch()` and `push()` methods matching TS IVM semantics
- [x] **OPR-02**: All IVM operators (Filter, Join, Take, Exists, Skip, Cap) ported to Rust with full fetch + push support
- [x] **OPR-03**: Pipeline builder constructs Rust operator tree from ZQL AST, matching TS `buildPipeline()` output

## Source & Connection

- [ ] **SRC-01**: Rust TableSource with SQLite connection pool (N read-only connections at same WAL snapshot)
- [ ] **SRC-02**: Filter/sort pushdown into SQL query (matching TS `source.connect()` behavior)
- [ ] **SRC-03**: Overlay system in Rust for self-join correctness during push propagation

## Hydration Parallelism

- [ ] **HYD-01**: N pipelines hydrate in parallel via Rayon, each on own thread with own SQLite connection
- [ ] **HYD-02**: Scalar subquery resolution works within each pipeline thread
- [ ] **HYD-03**: Row deduplication after parallel hydration produces identical output to sequential TS dedup

## Join Fan-Out

- [ ] **JFO-01**: Child relationship fetches within Join.fetch() run in parallel per parent row
- [ ] **JFO-02**: Child parallelism preserves correct ordering (parent order + per-relationship results)

## Advance Parallelism

- [ ] **ADV-01**: Full operator tree evaluation during advance runs on Rayon threads (not just filter-only fan-out)
- [ ] **ADV-02**: Pipeline state (Take bounds, overlay) correctly maintained across parallel push propagation
- [ ] **ADV-03**: ResetPipelinesSignal equivalent in Rust — schema/permission changes return error to TS

## Serialization

- [ ] **SER-01**: Binary format for Rust -> TS row transfer (avoids napi JS object creation on Rayon threads)
- [ ] **SER-02**: Encoding + decoding overhead < 10% of total operation time

## TS Integration

- [ ] **INT-01**: pipeline-driver.ts `addQuery()` delegates to Rust hydration (build + fetch in Rust)
- [ ] **INT-02**: pipeline-driver.ts `#advance()` delegates to Rust advance (full operator tree push in Rust)
- [ ] **INT-03**: Feature flag `ZERO_DISABLE_RUST_HYDRATION` for fallback to TS path

## Cross-ViewSyncer

- [ ] **XVS-01**: Single Rust call processes change across ALL affected ViewSyncer pipelines in parallel
- [ ] **XVS-02**: Global timeSliceQueue unnecessary for advance path (CPU work moved to Rust threads)

## Validation

- [ ] **E2E-01**: All existing vitest suites pass with Rust IVM runtime enabled
- [ ] **E2E-02**: Docker E2E against xyne-spaces: identical pass/fail pattern as stock zero
- [ ] **BEN-01**: Documented benchmarks showing measurable improvement for hydration and advance paths

## Traceability

| REQ-ID | Phase |
| ------ | ----- |
| OPR-01 | 20    |
| OPR-02 | 20    |
| OPR-03 | 20    |
| SRC-01 | 21    |
| SRC-02 | 21    |
| SRC-03 | 21    |
| HYD-01 | 22    |
| HYD-02 | 22    |
| HYD-03 | 22    |
| JFO-01 | 23    |
| JFO-02 | 23    |
| ADV-01 | 24    |
| ADV-02 | 24    |
| ADV-03 | 24    |
| SER-01 | 25    |
| SER-02 | 25    |
| INT-01 | 26    |
| INT-02 | 26    |
| INT-03 | 26    |
| XVS-01 | 27    |
| XVS-02 | 27    |
| E2E-01 | 28    |
| E2E-02 | 28    |
| BEN-01 | 28    |

## Out of Scope

- Client-side code (zero-client, zero-react)
- WebSocket/HTTP server (Fastify) — stays TS
- Auth/JWT/permissions transformation — stays TS
- CVR Postgres operations — stays TS
- Poke protocol / wire format — stays TS
- Change streamer — stays TS
- Rewriting pipeline-driver.ts or view-syncer.ts entirely in Rust (they are orchestration/I/O glue)
