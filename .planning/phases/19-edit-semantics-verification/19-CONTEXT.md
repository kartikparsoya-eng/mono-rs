# Phase 19: Edit Semantics Verification - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Verify the Rust `rust_advance()` path produces correct intermediate edit stream output - not just final snapshot equality. Tests edit type transitions, filter-boundary splits, and insert/delete cancellation.

</domain>

<decisions>
## Implementation Decisions

### Edit Split Verification

- **D-90:** Assert both exact `change_type` strings AND final row set equality. When a row crosses a filter boundary (was visible -> now hidden), Rust must emit "remove" not "edit". When a row becomes visible via edit, Rust must emit "add" not "edit".

### Ordering Semantics

- **D-91:** Compare changes as unordered set (order-insensitive). Cross-pipeline and within-pipeline order is not guaranteed due to Rayon. Real consumers don't depend on order.

### Insert->Delete Cancellation

- **D-92:** Test at Rust unit level only (in `advance.rs`). When changelog contains insert+delete of same PK within one push, `process_change_for_pipeline` should produce no output for that row.

### Carried Forward

- **D-35:** No test file modifications
- **D-78:** Separate test files per concern
- **D-80:** Dual-path comparison via `ZERO_DISABLE_RUST_IVM` toggle

### Claude's Discretion

- Test file naming and organization within phase constraints
- Number of test cases per scenario (minimum to cover success criteria)

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### IVM Change Types

- `packages/zql/src/ivm/change.ts` - Defines AddChange, RemoveChange, EditChange, ChildChange types
- `packages/zql/src/ivm/maybe-split-and-push-edit-change.ts` - TS reference for edit split logic (filter boundary)

### Rust Advance Logic

- `packages/zqlite-rs/src/advance.rs` lines 244-334 - `process_change_for_pipeline` edit handling logic
- `packages/zqlite-rs/src/advance.rs` lines 351-368 - `find_prev_by_pk` PK matching

### Existing Test Patterns

- `packages/zero-cache/src/services/view-syncer/pipeline-driver.fixtures.ts` - Shared test helpers
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.edge-cases.test.ts` - Prior edit test patterns

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `pipeline-driver.fixtures.ts`: schemas, AST builders, `runDualPath()` helper for Rust vs TS comparison
- `advance.rs` already has `test_edit_matches_by_primary_key` and `test_edit_no_pk_match_becomes_add_remove` tests

### Established Patterns

- Rust edit logic: `process_change_for_pipeline` checks `passes_filter(old)` x `passes_filter(new)` to decide change_type
- Missing case in Rust: when `!passes_filter(prev)` AND `passes_filter(next)` with PK match - currently emits "edit" but should emit "add" (or TS may also emit "edit" here - needs verification)

### Integration Points

- New Rust unit tests in `advance.rs` `#[cfg(test)]` module
- New vitest file: `pipeline-driver.edit-semantics.test.ts`

</code_context>

<specifics>
## Specific Ideas

- EDI-01 test cases: edit where filter result changes (4 quadrants: old-pass/new-pass, old-pass/new-fail, old-fail/new-pass, old-fail/new-fail)
- EDI-02 test cases: insert->update (same PK, two changelog entries), update->delete, insert->delete
- Cancellation: changelog with insert row {id:1, x:5} then delete row {id:1} -> no output

</specifics>

<deferred>
## Deferred Ideas

None - discussion stayed within phase scope

</deferred>

---

_Phase: 19-edit-semantics-verification_
_Context gathered: 2026-04-21_
