# Phase 16: NOT EXISTS & Join Topology Tests - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Close operator coverage gaps: make NOT EXISTS work through the Rust exists path, and test complex join topologies (multi-level, sibling, exists-on-child) end-to-end through the pipeline driver.

</domain>

<decisions>
## Implementation Decisions

### NOT EXISTS Fix

- **D-75:** Detect exists type by inspecting the operator instance property in `pipeline-driver.ts`, NOT by modifying the name string in `builder.ts` (D-44 blocks zql changes).
- **D-76:** The regex `RUST_EXISTS_NAME_RE` stays as-is. Instead, the `decorateFilterInput` callback inspects the operator's `existsType` property (or equivalent) to determine EXISTS vs NOT EXISTS.
- **D-77:** `rust-exists.ts` wrapper already supports `notExists` boolean — no changes needed in Rust code or wrapper. Only the call site in `pipeline-driver.ts` needs updating.

### Join Topology Tests

- **D-78:** Test 3 topologies: parent→child→grandchild (3-level), sibling joins (two children of same parent), parent with exists on child.
- **D-79:** Tests verify end-to-end correctness through the Rust-accelerated path (not just TS fallback).

### Test Organization

- **D-80:** Separate test files: `pipeline-driver.not-exists.test.ts` and `pipeline-driver.join-topology.test.ts`
- **D-81:** D-35 applies — no modifying existing test files.

### Claude's Discretion

- Test data design (table schemas, row counts, assertion granularity)
- Whether to also add Rust unit tests for NOT EXISTS edge cases in `exists.rs`

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Exists Implementation

- `packages/zero-cache/src/services/view-syncer/rust-exists.ts` — Rust exists wrapper, already supports `notExists` boolean
- `packages/zero-ivm-rs/src/exists.rs` — Rust exists decision logic, already handles `not_exists: bool`
- `packages/zql/src/ivm/exists.ts` — TS reference implementation (EXISTS vs NOT EXISTS via `this.#not`)
- `packages/zql/src/builder/builder.ts:669` — Always generates `:exists(rel)` name (D-44: do not modify)

### Pipeline Integration

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:73` — `RUST_EXISTS_NAME_RE` regex
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:496-507` — `decorateFilterInput` hardcodes `'EXISTS'`

### Join Topology

- `packages/zql/src/ivm/join.ts` — TS Join operator (parent/child binary structure)
- `packages/zero-cache/src/services/view-syncer/rust-join.ts` — Rust join wrapper

### Existing Tests (reference, DO NOT MODIFY)

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — 30 integration tests
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.edge-cases.test.ts` — 6 edge case tests
- `packages/zql/src/ivm/exists.fetch.test.ts` — Has NOT EXISTS test cases (lines 863+)

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `createRustExistsWrapper` in `rust-exists.ts` — already takes `existsType` param, just needs correct value passed
- `rustExistsPushBatch` in `exists.rs` — already takes `not_exists: bool`, no Rust changes needed
- Pipeline-driver test infrastructure — `makeSource`, `makeOperator` helpers in existing test files

### Established Patterns

- Delegate pattern for Rust integration (D-44 compliance)
- Lazy import with graceful fallback (`isRustExistsAvailable()`)
- Edge case tests in separate files from main integration tests

### Integration Points

- `pipeline-driver.ts:decorateFilterInput` — the single call site that needs the exists type fix
- New test files alongside existing test files in `packages/zero-cache/src/services/view-syncer/`

</code_context>

<specifics>
## Specific Ideas

- The fix is small (propagate existsType in pipeline-driver.ts) but the test coverage is the main deliverable
- 3 join topologies: parent→child→grandchild, sibling joins, parent with exists on child
- Verify Rust path produces identical results to TS path for all NOT EXISTS scenarios

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

_Phase: 16-not-exists-join-topology-tests_
_Context gathered: 2026-04-21_
