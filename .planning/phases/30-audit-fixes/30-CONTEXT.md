# Phase 30: Audit Fixes - Context

**Gathered:** 2026-04-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Ship the 4 fixes from the IVM port audit (`.planning/IVM-PORT-AUDIT.md`) so the streaming work in Phase 31 builds on a known-correct operator baseline. Strictly bug fixes — no new features, no refactors beyond the minimum needed for each fix.

**In scope:**

- AUDIT-01: `LIKE` case sensitivity in `parse_predicate_json` (real bug, one-line fix)
- AUDIT-02: `EXISTS` parent_field missing from `collect_split_edit_keys` (real bug, ~10-line fix)
- AUDIT-03: Promote framework-invariant `debug_assert!` to `assert!` in 4 operators (latent risk → loud failure)
- AUDIT-04: Fix `ExistsOperator` / `OrExistsOperator` Edit handling with `or_predicate` (latent bug)

**Out of scope:**

- Any streaming work (that's Phase 31)
- Any change to `encode_advance_result_buf` / `decodeAdvanceResultBuf` formats
- Any signature change to existing buffered Rust napi methods or TS pipeline-driver methods
- The `Cap` operator divergence noted in the audit (Cap is dead code, deletion is a separate decision deferred to v6.0)

</domain>

<decisions>
## Implementation Decisions

### AUDIT-01: LIKE case sensitivity

- **D-01:** Change `packages/zqlite-rs/src/hydrate.rs:769` third arg of `Predicate::Like` from `true` to `false` for the `like` key. Leave the `ilike` key arm at `true`. This matches `pipeline.rs::parse_predicate` (already correct) and TS `getLikePredicate(rhs, '')` semantics.
- **D-02:** Add a Rust unit test in `packages/zqlite-rs/src/hydrate.rs` (or wherever `parse_predicate_json` lives) that:
  - Parses `{"field": "name", "like": "Foo%"}` → asserts the resulting `Predicate::Like(_, _, false)`.
  - Parses `{"field": "name", "ilike": "foo%"}` → asserts `Predicate::Like(_, _, true)`.
  - Evaluates each predicate against rows `{name: "Foo Bar"}`, `{name: "foo bar"}`, `{name: "FOO"}` and asserts correct case-sensitive vs case-insensitive matching.
- **D-03:** Add a TS integration test that exercises `LIKE` through the full ast_to_config → parse_predicate_json → FilterOperator path with a real query, asserting the case distinction holds end-to-end.

### AUDIT-02: EXISTS parent_field in split_edit_keys

- **D-04:** Add a `Condition::CorrelatedSubquery { related, .. }` arm to the inner `collect_from_cond` function inside `collect_split_edit_keys` (`packages/zqlite-rs/src/advance.rs`). The arm must:
  - Insert each field in `related.correlation.parent_field` into the keys set.
  - Recurse into `related.subquery.where_cond` (since EXISTS subqueries can themselves contain nested AND/OR/CSQ conditions).
- **D-05:** Add a Rust unit test that constructs an AST with EXISTS in the where clause, calls `collect_split_edit_keys`, and asserts the parent_field columns appear in the result.
- **D-06:** Add an integration test that:
  - Sets up a TableSource for table `parent` with a row whose `child_id` column is referenced by an `EXISTS` parent_field.
  - Edits the row's `child_id` from value A (matching children) to value B (no children).
  - Asserts the source emits `Remove + Add` (not `Edit`) and the downstream `ExistsOperator` produces a `Remove` for the parent in the output.
  - Tests the inverse case (B → A) and asserts `Add`.
- **D-07:** Verify the fix doesn't change behavior for queries without EXISTS (existing tests must still pass unchanged).

### AUDIT-03: debug_assert! → assert!

- **D-08:** Promote `debug_assert!` to `assert!` for these specific framework-invariant violations:
  - `packages/zero-ivm-rs/src/join_op.rs:219-221` — `Parent edit must not change relationship.`
  - `packages/zero-ivm-rs/src/join_op.rs:263-266` — `Child edit must not change relationship.`
  - `packages/zero-ivm-rs/src/exists_op.rs:170` — `Unexpected re-entrancy` (in_push)
  - `packages/zero-ivm-rs/src/take_op.rs:200, 246` — `Invalid state. Row has duplicate primary key`
  - `packages/zero-ivm-rs/src/cap_op.rs:166-169` — `Cap: partition key must not change on edit`
- **D-09:** Do NOT promote `debug_assert!` in non-framework contexts (e.g., test helpers, internal sanity checks that assume well-formed input where the validation cost would be excessive). Audit each `debug_assert!` occurrence in the listed files; promote only those that protect framework invariants where silent miscalculation is the alternative.
- **D-10:** Add a Rust unit test per promoted assertion that constructs the violating input and uses `#[should_panic(expected = "...")]` to confirm the panic message. This catches future regressions where someone might revert to `debug_assert!`.
- **D-11:** Run `cargo test --release` to confirm assertions fire in release builds, not just dev.

### AUDIT-04: ExistsOperator / OrExistsOperator Edit with or_predicate

- **D-12:** In `ExistsOperator::push_impl` and `OrExistsOperator` (the equivalent), update the `Change::Edit` branch:
  - Evaluate `or_predicate` on BOTH `old_node.row` and `node.row` (currently only checks new).
  - Combined with the size check (`passes_filter(count)`), determine old-row state and new-row state independently:
    - old_passed = `or_condition_matches(old)` || `passes_filter(count_for_old_pk)` (note: count uses old_pk)
    - new_passed = `or_condition_matches(new)` || `passes_filter(count_for_new_pk)`
    - Both pass → emit `Edit` (current behavior)
    - old_passed only → emit `Remove(old_node)`
    - new_passed only → emit `Add(node)`
    - Neither → emit nothing
- **D-13:** For now, use the cached `parent_sizes` for both old and new. If old_pk == new_pk (which it should be, since EXISTS parent_field is split via Bug #2 fix → parent_field doesn't change in Edit), this is straightforward. If old_pk != new_pk somehow leaks through, fetch both counts on demand.
- **D-14:** Add 4 Rust unit tests covering the 4 transitions (both pass / old only / new only / neither) for both `ExistsOperator` and `OrExistsOperator`.
- **D-15:** Confirm fix does NOT regress the existing Edit behavior for Exists without `or_predicate` set. The change only takes effect when `or_predicate.is_some()`.

### Claude's Discretion

- Test file naming and exact module placement of new unit tests (Claude picks the most idiomatic Rust location — typically `#[cfg(test)] mod tests` block in the same file, or co-located `_test.rs` if convention demands).
- Exact wording of panic messages in promoted asserts (preserve current message text unless ambiguous).
- Whether to add property-based fuzz coverage for AUDIT-04 transitions (likely yes, but not blocking — defer to planner).
- Order of fixes within the phase (independent fixes, can be one plan or four plans — planner decides based on commit hygiene preferences).

</decisions>

<canonical_refs>

## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Audit + Plan

- `.planning/IVM-PORT-AUDIT.md` — full audit findings, exact file:line locations for each bug, cross-checks against TS implementation
- `.planning/IVM-STREAMING-PLAN.md` — milestone context (why these fixes ship before streaming)

### Rust source files to modify

- `packages/zqlite-rs/src/hydrate.rs` (line 769 — AUDIT-01 fix site; rest of `parse_predicate_json` stays unchanged)
- `packages/zqlite-rs/src/advance.rs` (`collect_split_edit_keys` function — AUDIT-02 fix site)
- `packages/zero-ivm-rs/src/join_op.rs` (lines 219-221, 263-266 — AUDIT-03 promote sites)
- `packages/zero-ivm-rs/src/exists_op.rs` (line 170 — AUDIT-03 promote site; `push_impl::Edit` branch — AUDIT-04 fix site)
- `packages/zero-ivm-rs/src/or_exists_op.rs` (Edit branch — AUDIT-04 fix site)
- `packages/zero-ivm-rs/src/take_op.rs` (lines 200, 246 — AUDIT-03 promote sites)
- `packages/zero-ivm-rs/src/cap_op.rs` (lines 166-169 — AUDIT-03 promote site)

### Rust source for cross-reference (correct counterpart for AUDIT-01)

- `packages/zero-ivm-rs/src/pipeline.rs` (lines 128-135 — `parse_predicate` shows the correct `Like(_, _, false)` shape)

### TS source for behavioral parity reference

- `packages/zql/src/builder/builder.ts` (lines 273-290 — TS `splitEditKeys` collection, the parity target for AUDIT-02)
- `packages/zql/src/builder/filter.ts` (lines 113-150 — TS `createPredicateImpl`, the parity target for AUDIT-01 LIKE semantics)
- `packages/zql/src/ivm/join.ts` (Join.#pushParent / #pushChild Edit assertions — parity target for AUDIT-03 join asserts)
- `packages/zql/src/ivm/exists.ts` (Exists.#push Edit handling — parity reference; note TS doesn't have or_predicate, so AUDIT-04 has no direct TS counterpart)

### Test patterns to follow

- `packages/zero-ivm-rs/src/filter_op.rs` `#[cfg(test)] mod tests` block — example of co-located unit tests
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.edit-semantics.test.ts` — example integration-style edit semantics tests in TS

</canonical_refs>

<code_context>

## Existing Code Insights

### Reusable Assets

- `parse_predicate` in `pipeline.rs` is the correct reference implementation for `parse_predicate_json` LIKE flag — same shape, copy the third-arg semantics.
- `MockInput` and `make_node` test helpers in `join_op.rs`/`exists_op.rs` `mod tests` are reusable for the new unit tests.
- The existing edit-semantics integration tests in `pipeline-driver.edit-semantics.test.ts` provide a tested template for the AUDIT-02 source-split regression test.

### Established Patterns

- Rust unit tests live in `#[cfg(test)] mod tests` blocks in the same `.rs` file as the code under test.
- Integration-style edit-semantics tests live in `packages/zero-cache/src/services/view-syncer/pipeline-driver.edit-semantics.test.ts`.
- Panic-expecting tests use `#[should_panic(expected = "...")]`.
- The audit fixes touch operator semantics — all fixes need both unit tests (in Rust) AND integration tests (in TS via pipeline-driver) to verify end-to-end behavior.

### Integration Points

- `parse_predicate_json` is called from `build_next_operator` (hydrate.rs) and `build_push_next_operator` (hydrate.rs:1083) — both are in the production push/fetch paths. Fix is at the parse function so both call sites benefit automatically.
- `collect_split_edit_keys` output flows into `RustTableSource::connect(..., split_keys)` via `build_pipeline_state` (advance.rs). Fix at the collector so all consumers benefit.
- `ExistsOperator::push_impl::Edit` is reached via the persistent pipeline's `push_through_ptrs` and via `push_child` for child-table changes. AUDIT-04 fix is in the same function for both code paths.

</code_context>

<specifics>
## Specific Ideas

- **Order of work:** Plans should ship in dependency order — AUDIT-02 (split_edit_keys) before AUDIT-04 (Exists Edit handling) because AUDIT-04 assumes the parent_field doesn't change in Edit (which AUDIT-02 enforces at the source level).
- **Atomicity:** Each AUDIT-XX fix is independent enough to be its own plan, OR all 4 can be a single plan. Defer to planner; small atomic commits are easier to review/revert.
- **Verification gate (from ROADMAP.md):** every plan must run `npx vitest run packages/zero-cache/src/services/view-syncer/pipeline-driver.*.test.ts`, `npx vitest run packages/zero-cache/src/services/view-syncer/fuzz-ivm.test.ts`, and `cargo test` in both Rust crates before commit. None of these can fail.
- **Hard constraint:** no signature change to any existing buffered Rust napi method or TS pipeline-driver method, no change to `encode_advance_result_buf` / `decodeAdvanceResultBuf` formats. Phase 30 only changes operator semantics inside the existing surface.

</specifics>

<deferred>
## Deferred Ideas

- **Cap operator semantic divergence** (audit Risk #4): Cap is dead code (`ast_to_config` never emits `OperatorConfig::Cap`). Either delete the operator entirely or rewrite its fetch to match TS PK-lookup semantics — defer to v6.0 cleanup milestone.
- **Property-based fuzz coverage for AUDIT-04 transitions:** The 4-case Edit transition matrix would benefit from fast-check fuzzing similar to `fuzz-ivm.test.ts`. Mark as a follow-up if not added in this phase.
- **TS-side `parse_predicate_json` parity tests in dual-exec harness:** The dual-exec harness is dead in production but still imported by tests. Adding a LIKE-specific dual-exec test would catch this class of bug structurally — defer to a "test infrastructure" phase if dual-exec is ever re-enabled.

</deferred>

---

_Phase: 30-audit-fixes_
_Context gathered: 2026-04-29_
