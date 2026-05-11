---
status: resolved-partial
trigger: 'Families B + C combined: `flip` and `scalar` flag handling in CSQ causes RS to over-include rows. 4 hydrate divergences, all showing the same `co-1, co-2` over-include pattern → likely one root cause.'
created: 2026-05-11T13:30:00Z
updated: 2026-05-11T15:30:00Z
resolved_commit: c6477e7b9
deferred_followups:
  - "Family B (fuzz_00132, fuzz_00133) — flipped-CSQ-in-OR semantic mismatch. RS's `Exists{or_condition}` attaches CSQ children when the simple gate matches; TS's `UnionFanIn.mergeFetches` keeps the simple-branch's relationship-less node first, dropping the FlippedJoin's relationship payload. Deferred to Phase 36 (requires implementing FlippedJoin + UnionFanOut + UnionFanIn operators in zero-ivm-rs to mirror TS `applyFilterWithFlips` topology — Phase 36 stub exists at packages/zero-ivm-rs/src/flipped_join_op.rs (Wave 0 / unwired))."
---

## Current Focus

reasoning_checkpoint:
hypothesis: "Two distinct bugs explain the 4 hydrate divergences (NOT one root cause as the trigger hypothesized). Bug C: `SchemaCache::ensure_table_info` overwrites pre-seeded primary_keys with empty PRAGMA result on replica tables lacking PRIMARY KEY constraints — causes second-and-later EXISTS subqueries on the same table to build their Take with sort=[], which then returns ALL rows instead of `limit`. Bug B: For `OR(simple, csq{flip:true})`, RS emits `Exists{or_condition}` that attaches CSQ children when the simple gate matches, but TS's UnionFanIn dedup discards that relationship payload."
confirming_evidence: - "eprintln trace shows `primary_key=[\"id\"]` on first conversations subquery call, then `primary_key=[]` on second — confirming the overwrite." - "eprintln trace in Take.fetch shows result_size=3 for first Exists (sort=[(id,asc)]) and result_size=5 for second (sort=[]) — confirming the empty-sort cascade." - "Standalone fuzz_00132 control 'baseline OR(simple, EXISTS flip=false)' PASSED while only the flip variant DIVERGES — confirming Bug B is flip-specific and orthogonal to the AND/OR-of-two-CSQs shape." - "Post-fix corpus run 1185/5/8 vs pre-fix 1183/7/8 — exactly 2 cases (fuzz_00139, fuzz_00140) flipped from diverge→ok, while fuzz_00132/fuzz_00133 still diverge — confirming Bug C resolves Family C only, not Family B."
falsification_test: "If the post-fix corpus did NOT flip fuzz_00139/fuzz_00140 from diverge→ok, the hypothesis would be wrong. (Verified: it did.)"
fix_rationale: "Bug C fix preserves the Zero-schema-derived seeded primary_keys. TS reference: TS pipeline-driver reads PKs from `delegate.getSource(table).getSchema().primaryKey` (the Zero schema), never from SQLite PRAGMA. The seed in Rust mirrors this design intent; the overwrite was a latent bug masked by tests that constructed test tables WITH `PRIMARY KEY` constraints in their `CREATE TABLE`. Production replica tables lack those constraints, exposing the bug."
blind_spots: - "Have not run the advance (push) sweep to verify no advance-side regressions from the fix. Bug C fix only affects how primary_keys is populated, not how it's consumed by push code paths — should be a no-op for push, but unverified." - "Have not checked seed_22, seed_23, fuzz_00673 — these are other remaining diverges that may or may not be affected. The full corpus +2 OK matches expectation, so no regressions; but adjacent symptom changes haven't been audited."

hypothesis: Two distinct bugs identified — Bug C (PK overwrite) fixed; Bug B (flip-in-OR) is a separate root cause needing its own session.
test: Live diag against the 4 cases via tools/ivm-parity/\_diag_bc.ts (now removed) + full corpus sweep + cargo unit tests + catalog regression-runner.
expecting: fuzz_00139 and fuzz_00140 flip from diverge→ok; catalog stays 6/6; existing unit tests stay green; full corpus moves 1183→1185 OK.
next_action: Commit Bug C fix; report findings to user; ask scope decision for Bug B (flip-in-OR semantic mismatch — separate session or fold into this one?).

## Symptoms

expected: AST shapes using `flip:true` and/or `scalar:true` flags on a CorrelatedSubquery condition produce identical row sets between TS and RS.

actual: 4 hydrate cases over-include 2 rows in RS (`co-1`, `co-2` on `conversations`):

- fuzz_00132: channels.where(or(cmp('id','ch-pub-1'), exists('conversations', {flip:true}))) TS=12 RS=14
- fuzz_00133: same with {flip:true, scalar:true} TS=12 RS=14
- fuzz_00139: channels.where(and(exists('conversations'), exists('conversations', {scalar:true}))) TS=12 RS=14
- fuzz_00140: same with or(...) TS=12 RS=14

The over-include is identical (`co-1, co-2` on `conversations`) across all 4. AND vs OR variants fuzz_00139/00140 produce IDENTICAL RS output — hints at AND/OR collapse or scalar semantics drop.

errors: No runtime errors. Silent over-include.

reproduction: TS cache running on :4858, RS cache running on :4868. PG :6434. Diagnostic harness in tools/ivm-parity. Full re-test: `MAX_PARALLEL=8 npx tsx harness-coverage.ts`. Pre-Family-A baseline 1183 ok / 7 diverge. Goal: 4 of those 7 → ok → 1187 ok / 3 diverge.

started: Always broken since these shapes first added to fuzz corpus. Family A fix in `e07b6634b` did not touch this code path.

## Eliminated

(none yet)

## Evidence

- timestamp: 2026-05-11T13:35
  checked: tools/ivm-parity/debug-fuzz-00132.ts isolating fuzz_00132 + 3 control variants.
  found: Only the combination `OR(simple, csq{flip:true})` diverges (RS includes co-1, co-2 extras). All controls pass:
  - baseline OR(simple, EXISTS flip=false) → OK
  - isolated bare EXISTS flip=true (no OR) → OK
  - isolated bare EXISTS flip=false (no OR) → OK
    implication: Bug requires both (a) OR-of-conditions AND (b) flipped CSQ. The flipped EXISTS as a standalone WHERE works correctly; the simple/non-flipped CSQ in OR works correctly. The interaction inside `applyOr` is the root cause.

- timestamp: 2026-05-11T13:30
  checked: TS spec — packages/zql/src/builder/builder.ts:361-449 (applyWhere + applyFilterWithFlips), :514-557 (applyOr), :649-678 (applyCorrelatedSubqueryCondition).
  found: TS routes any flipped CSQ branch (at any nesting depth) to `applyFilterWithFlips` via `conditionIncludesFlippedSubqueryAtAnyLevel` at builder.ts:367. Inside flipped-or, TS partitions branches: `withoutFlipped` go through `applyOr` (FanOut/FanIn/Exists path); `withFlipped` go through `applyFilterWithFlips` recursively. Both are children of a `UnionFanOut` that fans into a `UnionFanIn` (builder.ts:417-446). The flipped branch terminates at a `FlippedJoin` node (builder.ts:459-477), not `Exists`. Critically: `applyCorrelatedSubqueryCondition` (line 649) NEVER reads `condition.flip` — the flipped path is handled exclusively by the higher-level `applyFilterWithFlips` routing.
  implication: If RS routes a flipped CSQ through `applyCorrelatedSubqueryCondition`-equivalent (i.e., normal EXISTS branch) it loses the FlippedJoin/UnionFanOut topology. The flipped branch should be a `FlippedJoin` reading the child source and joining UP to the parent table, with output unioned through `UnionFanIn`.

- timestamp: 2026-05-11T13:50
  checked: TS UnionFanIn.fetch + mergeFetches at packages/zql/src/ivm/union-fan-in.ts:103-108 + 222-296. ExistsOperator (Rust) at packages/zero-ivm-rs/src/exists_op.rs:148-199.
  found: TS UnionFanIn dedups by primary-key sort key — `mergeFetches` keeps only the FIRST yielded node per row (line 264-269 `comparator(lastNodeYielded, minNode) === 0` → continue). For fuzz_00132: the simple-branch (gate `id="ch-pub-1"`) yields `ch-pub-1` as a bare Filter result with NO `conv` relationship. The flipped-branch (FlippedJoin) ALSO yields `ch-pub-1` with a `conv` relationship attached. Because simple-branch yields first and they dedup, the FlippedJoin's relationship payload is DROPPED. Result: TS emits `ch-pub-1` without `conv` children (co-1, co-2 don't make it to client).
  RS path for fuzz_00132 (ast_to_config.rs:518-541 OR-flat branch): `csq_conds.len()==1`, `simple_conds=[simple_id_eq]`, `any_compound_branch` false. Routes to `append_csq_as_exists(..., or_cond_json=Some(predicate(simple_id_eq)))` → emits `OperatorConfig::Exists { or_condition: Some(...) }`. At runtime exists_op.rs:155-184: when `or_condition_matches(row)` is true (i.e., `ch-pub-1` matches the gate), the operator STILL calls `fetch_children` and inserts them into `node.relationships` at line 178-181. Result: RS emits `ch-pub-1` WITH `conv` relationship containing co-1, co-2 — these get propagated to client as conversation rows.
  implication: Bug confirmed at exists_op.rs:178-181. When or_condition matches the simple gate, RS unconditionally attaches the CSQ's children as a relationship payload. TS would never attach them in this case because the simple-branch in UnionFanIn has no relationship to begin with (it's a Filter, not a Join).

- timestamp: 2026-05-11T14:10
  checked: TS Exists semantics (packages/zql/src/ivm/exists.ts:21-99) and builder.ts:308-329 (non-flipped EXISTS pre-Join).
  found: TS Exists is a FilterOperator — it does NOT attach the `relationship_name` to the node; the relationship is attached UPSTREAM by `applyCorrelatedSubQuery` Join at builder.ts:308-329 (for non-flipped CSQs ONLY). Flipped CSQs are explicitly skipped at line 310 `if (!csqCondition.flip)`. So:
  - Non-flipped CSQ in OR(simple, csq): Join pre-attaches the relationship before FanOut/FanIn. Parent matching either branch passes with relationship attached. Both TS and RS produce the same result.
  - Flipped CSQ in OR(simple, csq{flip}): NO pre-Join. The simple-branch in UnionFanOut filters parent without any relationship. FlippedJoin branch joins and attaches relationship. UnionFanIn dedups (mergeFetches first-wins). Simple-branch yields first → simple-only nodes carry no relationship. Result: TS emits parent without csq children.
    implication: For flipped-CSQ-in-OR, RS's `Exists{or_condition}` (which attaches children when gate matches) does NOT match TS. RS needs to either (a) implement UnionFanIn/FlippedJoin topology, or (b) emit a "filter-only" Exists for flipped CSQ that doesn't attach relationship when or_condition matches, or (c) at hydrate time, skip attaching the relationship when or_condition matches.

- timestamp: 2026-05-11T14:20
  checked: Ran live diag with `RS_DEBUG_TAKE` + `RS_DEBUG_EXISTS` + `RS_DEBUG_AST_TO_OPS` + `RS_DEBUG_APPLY_EXISTS_LIMIT` + `RS_DEBUG_BUILD_NEXT_TAKE` eprintln traces. Inspected runtime values of `OperatorConfig::Take.sort` for both Exists operators inside `AND(EXISTS conv1, EXISTS conv2)`.
  found: **Bug C root cause:** `SchemaCache::ensure_table_info` at ast_to_config.rs:164-165 unconditionally inserts the PRAGMA-derived primary_key into `self.primary_keys`, overwriting any pre-seeded entry from `seed_primary_keys`. When the replica's SQLite CREATE TABLE lacks a `PRIMARY KEY` constraint (production replica vs test setup), PRAGMA returns an empty pk_cols vector. Sequence:
  1. `build_pipeline_state` seeds primary_keys[conversations] = ["id"] from query.all_primary_keys.
  2. First Exists's recursion calls `get_primary_key("conversations")` → ["id"] (from seed). ✓
  3. Then `ast_to_operator_configs` calls `get_columns("conversations")` → `ensure_table_info` → PRAGMA returns empty pk → INSERTS [] OVERWRITING seed.
  4. Second Exists's recursion calls `get_primary_key("conversations")` → [] (from PRAGMA-overwritten state). ✗
  5. `ast_to_operator_configs` builds Source with sort=[] (the for-loop at line 217 adds nothing because primary_key is empty).
  6. `apply_exists_limit` reads Source.sort = [] → appends Take with sort=[].
  7. At runtime `compare_rows` with empty sort returns `Equal` for every pair → state_with_bound branch in TakeOperator::fetch never hits `cmp == Less` → returns ALL input rows instead of 3.
     Verified by eprintln trace lines 257 (primary_key=["id"]) vs 260 (primary_key=[]) for back-to-back conversations subqueries.

  **Two distinct bugs identified — Family B+C is NOT one root cause as initially hypothesized:**
  - Family C (fuzz_00139, fuzz_00140): PK-overwrite bug in `SchemaCache::ensure_table_info`. Fix: don't overwrite a pre-seeded entry with an empty PRAGMA result.
  - Family B (fuzz_00132, fuzz_00133): Flipped-CSQ-in-OR-with-simple-gate semantic bug. RS's `Exists{or_condition}` attaches children when gate matches; TS's UnionFanIn dedup keeps the simple-branch (relationship-less) node, dropping the FlippedJoin's relationship payload.

## Resolution

root_cause: |
Two distinct bugs, both manifesting as RS over-include on `conversations` table when the query is `channels.where(...)` with EXISTS in the WHERE.

**Bug C (Family C — fuzz_00139, fuzz_00140):** `SchemaCache::ensure_table_info` at `packages/zqlite-rs/src/ast_to_config.rs:164-165` unconditionally inserts the PRAGMA-derived primary_key, overwriting any pre-seeded entry. On the production replica, SQLite tables lack `PRIMARY KEY` constraints (TS replicator does not propagate them), so PRAGMA returns empty pk. The first `get_columns` call on a child table thus clobbers the seed PK. The next `get_primary_key` for the same table returns [] → Source built with sort=[] → Take built with sort=[] → `compare_rows` with empty sort returns Equal everywhere → state_with_bound loop in `TakeOperator::fetch` never breaks → returns all input rows instead of `limit`. Family C's symptom (`x-co-4, x-co-5` over-include for `x-ch-deep`) is the second Exists's Take returning 5 instead of 3.

**Bug B (Family B — fuzz_00132, fuzz_00133):** For `OR(simple_gate, csq{flip:true})`, RS emits a single `OperatorConfig::Exists { or_condition: Some(simple_gate_pred) }`. At runtime in `packages/zero-ivm-rs/src/exists_op.rs:155-184`, when `or_condition_matches(row)` is true (the simple gate matches), the operator STILL fetches CSQ children and inserts them into `node.relationships`. But TS routes flipped CSQs via `applyFilterWithFlips` (builder.ts:376-449) using `UnionFanOut` → `[Filter(simple), FlippedJoin]` → `UnionFanIn`. The simple-branch yields the gate-matched parent with NO relationship; the FlippedJoin yields the same parent WITH the csq relationship. `UnionFanIn.mergeFetches` (union-fan-in.ts:222-296) dedups by primary key — keeps FIRST yielded node. Simple-branch wins (lowest index → yielded first by reduce min at line 247) → the FlippedJoin's relationship payload is discarded. Result: TS emits ch-pub-1 without `conv` children; RS includes co-1, co-2 of ch-pub-1 because Exists+or_condition attaches them.

fix: |
**Bug C fix (to be applied):** `packages/zqlite-rs/src/ast_to_config.rs::SchemaCache::ensure_table_info` — guard the `primary_keys.insert` at line 165 so it does not overwrite a pre-seeded entry. The seed (from `query.all_primary_keys`) is authoritative (Zero schema source); PRAGMA is a fallback. Code change:
`     self.columns.insert(table_name.to_string(), cols);
    if !self.primary_keys.contains_key(table_name) || self.primary_keys.get(table_name).map(|v| v.is_empty()).unwrap_or(true) {
        self.primary_keys.insert(table_name.to_string(), pk);
    }
    `
TS reference: TS has no equivalent because the TS `getTableSchema` reads PKs from the supplied Zero schema (zero-cache/src/services/view-syncer/pipeline-driver.ts ~line 350 — `delegate.getSource(table).getSchema().primaryKey`), never from SQLite PRAGMA. The Rust port preserves the seed-from-schema design intent; the bug is the PRAGMA fallback path clobbering it.

**Bug B fix (deferred for this session):** Will require either implementing the `FlippedJoinOperator`+`UnionFanOut`+`UnionFanIn` topology, or adding a `flipped_in_or` flag on `OperatorConfig::Exists` that suppresses relationship attachment when `or_condition` matches. Scope decision in checkpoint below.

verification:

- "Bug C fix landed in `packages/zqlite-rs/src/ast_to_config.rs::SchemaCache::ensure_table_info` — only insert PRAGMA-derived primary_keys when there's no seeded entry or the seeded entry is empty."
- "fuzz_00139 (`AND(EXISTS, EXISTS{scalar})`) — RS conversations 10 → 8 (matches TS=8) — STATUS=OK ✓"
- "fuzz_00140 (`OR(EXISTS, EXISTS{scalar})`) — RS conversations 10 → 8 (matches TS=8) — STATUS=OK ✓"
- "C3 control (`AND(EXISTS, EXISTS)` both non-scalar, no scalar/flip flags) — RS 10 → 8 (matches TS) — STATUS=OK ✓"
- "C1 control (single `EXISTS{scalar}`) — still OK (no regression) ✓"
- "C2 control (single bare EXISTS) — still OK (no regression) ✓"
- "Catalog regression-runner: 6/6 OK (5 A-nested-OR-with-EXISTS + 1 D-simple-OR-with-EXISTS) — no regressions ✓"
- "Cargo unit tests: ast_to_config tests 17/17 pass — no regressions ✓"
- "Full 1198-AST hydrate corpus: pre-fix 1183/7/8 → post-fix 1185/5/8 (+2 OK, -2 diverge). fuzz_00139 and fuzz_00140 flipped from diverge→ok. Remaining 5 diverges: fuzz_00132, fuzz_00133 (Family B — flip-in-OR, separate root cause), fuzz_00673 (Family D), seed_22 (Family E), seed_23 (Family A residual)."
  files_changed:
- packages/zqlite-rs/src/ast_to_config.rs (SchemaCache::ensure_table_info — preserve seeded primary_keys)
  deferred_followups:
- "Family B (fuzz_00132 + fuzz_00133) remains diverging post-fix. Root cause is the flipped-CSQ-in-OR-with-simple-gate semantic mismatch (TS's UnionFanIn dedup keeps the simple-branch's relationship-less node; RS's `Exists{or_condition}` always attaches relationship payload when the gate matches). Two distinct fix strategies possible: (a) implement `FlippedJoinOperator` + `UnionFanOut/FanIn` topology for full TS-spec parity (Phase 36 stub at `packages/zero-ivm-rs/src/flipped_join_op.rs` is currently Wave 0 / unwired); (b) add a per-Exists `flipped_in_or` flag that suppresses relationship attachment when or_condition matches. Scope decision deferred to next session."
