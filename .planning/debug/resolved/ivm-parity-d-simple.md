---
status: resolved
trigger: "ivm-parity-d-simple — Last remaining IVM parity divergence in the 6-shape catalog. Bucket: D-simple-OR-with-EXISTS. Shape: OR(EXISTS, always-true-simple) — pre-existing bug in RS's OrExists operator path (not the FanOut path introduced for B5). 5/6 passing on hydrate; this is the 1/6."
created: 2026-05-11T00:00:00Z
updated: 2026-05-11T05:58:00Z
resolved: 2026-05-11T05:58:00Z
---

## Current Focus

reasoning_checkpoint:
hypothesis: |
RS `condition_to_predicate_json` short-circuits `NOT IN []` to
`{"and": []}` (always-true), ignoring SQL NULL semantics. TS
`createPredicate` (filter.ts:87-93) has an early NULL check: for
`processedAt NOT IN []` where `processedAt = null`, TS returns false
(NULL early-check); for non-null processedAt, returns true (empty set).
Net TS semantics: `NOT IN []` ≡ `processedAt IS NOT NULL`. RS emits
`{"and":[]}` which evaluates true even for NULL → over-permissive →
rows with NULL processedAt pass the OR via the simple branch → fill
limit=6 slots, displacing legitimate matches.
confirming_evidence: - "Diag run: TS=4 events all with processedAt!=null. RS=6 events including 3 with processedAt=null (ev-jsonb-empty-test, ev-no-ticket-test, ev-null-test-1)." - "RS missing ev-pure-or-test-1 (has tags AND non-null processedAt) — its limit-6 slot was stolen by a NULL-processedAt row sorted earlier alphabetically." - "TS createPredicate filter.ts:87-93: `if (lhs === null || lhs === undefined) return false;` — explicit NULL gate." - "RS ast_to_config.rs:1063-1068: returns `{\"and\":[]}` for NOT IN [], no field reference, no NULL gate." - "Predicate evaluator filter.rs:232: `Predicate::And(empty) → true` for any row."
falsification_test: |
Inject a unit test in ast_to_config.rs: feed `Condition::Simple { op:"NOT IN",
    left: Column("processedAt"), right: Literal([]) }` → assert emitted JSON is
`{"field":"processedAt", "isNotNull": true}` (or equivalent NULL-aware form).
Live-test: after fix, regression-runner for D-simple bucket should report
`ok=1`.
fix_rationale: |
Replace `{"and": []}` (always-true) with `{"field": <name>, "isNotNull": true}`
in the NOT IN [] short-circuit. This exactly matches TS semantics: `lhs NOT
    IN []` returns true iff lhs is non-null (TS early NULL check + empty-set
result). For the literal-LHS literal-RHS case (line 1031-1035) the
short-circuit stays `{"and":[]}` — no field to be null there.
For `IN []`, leave `{"or":[]}` (always-false) — matches TS (NULL gate
returns false, empty set also returns false).
blind_spots: - "If `field` is a column that doesn't exist on the row at runtime, RS evaluator filter.rs:226-228 IsNull returns true (treats missing as NULL) — TS behavior is the same (row[name] is undefined → early null check returns false for the outer predicate). Equivalent." - "If the column reference is wrapped in a subquery resolution path (not in our catalog), the short-circuit may be reached without a proper field name. Mitigated: the surrounding code at lines 1015-1038 already filters to ConditionValue::Column for non-literal LHS, and the literal-literal case has its own short-circuit before this." - "Multi-column NOT IN: TS handles tuple NOT IN as a Set, but Rust doesn't construct tuples either way — the catalog only has single-column NOT IN. Out of scope for D-simple."

hypothesis: "RS NOT IN [] short-circuit emits {and:[]} (always true), missing TS's NULL early-check semantics."
test: "Run regression-runner BUCKET=D-simple-OR-with-EXISTS after fix; expect ok=1."
expecting: "After fix, RS row set matches TS exactly (4 events, all non-null processedAt)."
next_action: "Apply fix in ast_to_config.rs:1063-1068 — emit isNotNull for NOT IN [] (column LHS), rebuild napi binary, restart RS, redeploy permissions, re-run regression."

## Symptoms

expected: |
Catalog shape with bucket D-simple-OR-with-EXISTS produces identical row
sets between TS and RS on hydrate. regression-runner reports
`D-simple-OR-with-EXISTS: ok=1 pass=100%`.

actual: |
0/1 in that bucket. Total catalog ok=5 diverge=1 error=0 after the B5
fixes landed (commits 0f2361b52, 353088651, 3ec659b5a). Silent wrong-row-set
divergence.

errors: No runtime errors. Silent wrong-row-set divergence.

reproduction: |
cd /Users/kartik.parsoya/Documents/Zero/mono-rs/tools/ivm-parity
BUCKET=D-simple-OR-with-EXISTS npx tsx regression-runner.ts
Expect: D-simple-OR-with-EXISTS: ok=0 diverge=1 error=0

started: Pre-existing prior to B5 fix landing 2026-04-30. Documented in
resolved/ivm-parity-divergences.md as out-of-scope follow-up.

## Eliminated

(none yet)

## Evidence

- timestamp: 2026-05-11T00:00:00Z
  checked: Knowledge base + resolved/ivm-parity-divergences.md
  found: D-simple is shape with always-true `NOT IN []` simple branch in OR
  alongside a CSQ. Pre-existing OrExists-path ordering bug. The B5 fix
  explicitly preserved this branch unchanged.
  implication: Route is `OrExists`/`Exists+or_condition` at ast_to_config.rs:518-541,
  NOT FanOut.

- timestamp: 2026-05-11T01:00:00Z
  checked: ast_to_config.rs:1063-1068 NOT IN [] short-circuit.
  found: RS emits `{"and": []}` for NOT IN [] (always-true predicate, no
  field reference). The empty-AND evaluates true for any row in
  filter.rs:232.
  implication: This short-circuit ignores SQL NULL semantics. TS retains
  NULL-on-LHS = false (filter.ts:87-93 early null check).

- timestamp: 2026-05-11T01:05:00Z
  checked: Live diag run via \_diag_d_simple.ts against parity caches on
  :4858/:4868.
  found: |
  AST is `events WHERE OR(EXISTS(event_tags WHERE eventId=events.id),
processedAt NOT IN []) LIMIT 6 RELATED conversations`.
  TS returned 4 events: all have processedAt!=null: - ev-dst-fall-test (no tags, processedAt non-null) - ev-dst-spring-test (no tags, processedAt non-null) - ev-jsonb-test-1 (2 tags, processedAt non-null) - ev-pure-or-test-1 (2 tags, processedAt non-null)
  RS returned 6 events: - ev-dst-fall-test (processedAt non-null) - ev-dst-spring-test (processedAt non-null) - ev-jsonb-empty-test (processedAt NULL — wrongly included) - ev-jsonb-test-1 (processedAt non-null) - ev-no-ticket-test (processedAt NULL — wrongly included) - ev-null-test-1 (processedAt NULL — wrongly included)
  DB total: 8 events, 4 with processedAt!=null, 4 NULL.
  implication: RS includes 3 rows with NULL processedAt that TS correctly
  rejects. These fill the limit=6, displacing ev-pure-or-test-1
  (alphabetically sorted later than the included NULL rows). Diagnosis
  of root cause confirmed.

- timestamp: 2026-05-11T01:10:00Z
  checked: filter.ts:62-94 (TS createPredicate) and filter.rs:226-262 (RS
  predicate evaluator + is_null_for_row).
  found: |
  TS createPredicate at line 87-93 has explicit early NULL check before
  calling impl: returns false when lhs is null/undefined. RS's
  Predicate::Not + is_null_for_row implements NULL semantics for
  non-empty NOT IN ([1,2,3]) correctly — but the empty-array short-
  circuit at ast_to_config.rs:1063-1068 bypasses this entirely by
  emitting {"and":[]} which has no field reference, so the NULL gate
  never runs.
  implication: The fix is in ast_to_config.rs: emit a NULL-aware
  short-circuit for NOT IN []. Equivalent expression: `field IS NOT
NULL` → `{"field": <name>, "isNotNull": true}` matches TS exactly
  (TS NULL gate returns false; NOT IN empty set returns true otherwise →
  field IS NOT NULL semantics).

## Resolution

root_cause: |
packages/zqlite-rs/src/ast_to_config.rs::condition_to_predicate_json (lines
1061-1068) short-circuited `x NOT IN ()` to `{"and": []}` — an
unconditional-true predicate with no field reference. The empty-AND
evaluates true for any row at packages/zero-ivm-rs/src/filter.rs:232.

TS reference (packages/zql/src/builder/filter.ts:87-93 createPredicate)
has an explicit early NULL check: when the LHS column value is null or
undefined, the predicate returns false before evaluating the operator.
For `processedAt NOT IN []`: TS returns false for NULL rows, true for
non-NULL rows — semantically `processedAt IS NOT NULL`.

Rust's short-circuit bypassed this NULL gate because the emitted
`{"and": []}` has no field reference — `is_null_for_row` (filter.rs:245)
never runs against it. Result: rows with NULL processedAt wrongly
passed the OR via the simple branch, filling limit=6 slots and
displacing legitimate matches (e.g. ev-pure-or-test-1).

fix: |
packages/zqlite-rs/src/ast_to_config.rs:1075-1078 — replace the
`{"and": []}` always-true short-circuit for `NOT IN []` with
`{"field": <column-name>, "isNotNull": true}`. This exactly mirrors
TS semantics: NULL-on-LHS returns false (Predicate::IsNotNull at
filter.rs:229-230 maps None/Null → false); non-NULL value returns true
(NOT IN empty set is always true for non-NULL). The `IN []` case keeps
its `{"or": []}` always-false short-circuit (matches TS: NULL gate
returns false; non-NULL value not in empty set also returns false).

Three unit tests added in ast_to_config.rs::tests: - test_condition_to_predicate_in_empty_array_is_always_false - test_condition_to_predicate_not_in_empty_array_is_is_not_null

verification: |
Unit: cargo test --lib for zqlite-rs → 151 passed; 0 failed (was 148 + 3 new tests). zero-ivm-rs unchanged: 234/234.
Live: ./regression-runner.ts after napi rebuild + RS restart →
overall ok=6 diverge=0 error=0 (was ok=5 diverge=1).
A-nested-OR-with-EXISTS: 5/5 pass=100% (unchanged from B5 fix).
D-simple-OR-with-EXISTS: 1/1 pass=100% (was 0/1).
\_diag_d_simple.ts post-fix: STATUS === ok. TS=RS=4 events
{ev-dst-fall-test, ev-dst-spring-test, ev-jsonb-test-1,
ev-pure-or-test-1} all with processedAt!=null; TS=RS=4 event_tags
(the tags for ev-jsonb-test-1 and ev-pure-or-test-1).

files_changed:

- packages/zqlite-rs/src/ast_to_config.rs
- tools/ivm-parity/regression-runner-last.json
- tools/ivm-parity/\_diag_d_simple.ts (new diag helper)
