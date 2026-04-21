# Roadmap — v3.0 Test Coverage & Correctness Hardening

## Phase 16: NOT EXISTS & Join Topology Tests

**Goal:** Close operator coverage gaps — NOT EXISTS support and complex join integration tests.

**Requirements:** OPC-01, OPC-02

**Success Criteria:**

1. `existsType` passed through delegate to Rust exists handler
2. NOT EXISTS filter inverts correctly (rows excluded when subquery has matches)
3. Multi-level join (parent → child → grandchild) tested through `rust_advance()`
4. Sibling joins tested through `rust_advance()`
5. All new tests in separate test files (D-35 compliance)

---

## Phase 17: Data Type Diversity Tests

**Goal:** Prove Rust path handles all SQLite/JS value types correctly.

**Requirements:** DAT-01, DAT-02, DAT-03, DAT-04

**Success Criteria:**

1. JSON objects and arrays round-trip through Rust filter/join predicates
2. NULL values in filter comparisons, join keys, and sort keys produce identical results to TS
3. Unicode strings (emoji, CJK, combining characters) work in equality and LIKE comparisons
4. MAX_SAFE_INTEGER, -0, NaN, Infinity handled without silent corruption
5. Mixed-type columns (string vs number in same column across rows) compare correctly

---

## Phase 18: Concurrency Tests

**Goal:** Prove `rust_advance()` is safe under concurrent access.

**Requirements:** CON-01, CON-02

**Success Criteria:**

1. 10 concurrent pushes to same pipeline set produce deterministic output
2. Pipeline added mid-advance receives correct initial state on next cycle
3. Pipeline removed mid-advance doesn't cause panic or undefined behavior
4. Rayon thread pool handles concurrent `rust_advance()` calls without data races
5. No deadlocks under contention (test with timeout assertions)

---

## Phase 19: Edit Semantics Verification

**Goal:** Verify intermediate edit stream correctness, not just final snapshot.

**Requirements:** EDI-01, EDI-02

**Success Criteria:**

1. Edit stream from Rust matches TS edit-by-edit (not just final set equality)
2. Insert→update within same push produces [add, edit] sequence correctly
3. Insert→delete within same push produces no output (cancellation)
4. Update→delete produces [remove] with correct old values
5. Edit splits (row changes from matching to not-matching filter) produce correct remove+add pairs

---

## Summary

| Phase | Name                       | Requirements   | Criteria   |
| ----- | -------------------------- | -------------- | ---------- |
| 16    | NOT EXISTS & Join Topology | OPC-01, OPC-02 | 5          |
| 17    | Data Type Diversity        | DAT-01–04      | 5          |
| 18    | 2/2                        | Complete       | 2026-04-21 |
| 19    | Edit Semantics             | EDI-01, EDI-02 | 5          |

**4 phases** | **10 requirements** | 100% coverage
