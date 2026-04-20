# Requirements — v3.0 Test Coverage & Correctness Hardening

## Operator Coverage

- [ ] **OPC-01**: NOT EXISTS path exercised end-to-end — delegate passes `existsType`, Rust handles negation correctly
- [ ] **OPC-02**: Complex join topologies (multi-level, sibling joins) tested through `rust_advance()` pipeline

## Data Diversity

- [ ] **DAT-01**: JSON column values pass through Rust path correctly (nested objects, arrays)
- [ ] **DAT-02**: NULL-heavy datasets produce identical output in Rust vs TS
- [ ] **DAT-03**: Unicode strings (emoji, CJK, RTL) handled correctly in filter predicates and join keys
- [ ] **DAT-04**: Boundary numbers (MAX_SAFE_INTEGER, floats with precision loss, negative zero) pass correctly

## Concurrency

- [ ] **CON-01**: Multiple concurrent pushes against `rust_advance()` produce deterministic results
- [ ] **CON-02**: Pipeline add/remove during active advance doesn't corrupt state

## Edit Semantics

- [ ] **EDI-01**: Intermediate edit ordering (add before remove, edit splits) verified — not just final output equality
- [ ] **EDI-02**: Edit type transitions (insert→update→delete within same push) produce correct diff stream

## Traceability

| REQ-ID | Phase |
|--------|-------|
| OPC-01 | 16 |
| OPC-02 | 16 |
| DAT-01 | 17 |
| DAT-02 | 17 |
| DAT-03 | 17 |
| DAT-04 | 17 |
| CON-01 | 18 |
| CON-02 | 18 |
| EDI-01 | 19 |
| EDI-02 | 19 |

## Out of Scope

- Performance optimization (v3.0 is correctness-only)
- Extending Rust advance to join/exists/take pipelines (future milestone)
- CI integration (future milestone)
