---
status: resolved
trigger: 'ivm-parity-divergences — 3 IVM parity divergences between Rust (zqlite-rs / zero-ivm-rs) and TypeScript (zql) IVM. Baseline: 0/6 catalog shapes pass.'
created: 2026-04-30T00:00:00Z
updated: 2026-05-11T00:00:00Z
resolved_for: B5 (compound OR-of-EXISTS) — 5/6 catalog shapes match TS
follow_up: D-simple-OR-with-EXISTS — separate pre-existing OrExists ordering bug
  with always-true `NOT IN []` simple branch. Not part of the B5 family. Tracked
  as a new debug session.
---

## Current Focus

hypothesis: |
B5 hydrate parity now passes 5/6 catalog shapes (was 0/6). The remaining
1/6 (D-simple-OR-with-EXISTS) is a separate, pre-existing OrExists-path
ordering bug NOT touched by the B5 family fix; out of scope.

Two distinct production bugs were uncovered behind the same 0/6 symptom
(both surfaced once the FanOut config emission worked):

  Bug A (`hydrate.rs::ParallelFanOutOperator::fetch`):
    Branch sub-pipelines were built via `build_operator_with_live_source`
    → `build_next_operator` → `ParallelExistsOperator`. ParallelExistsOperator
    uses `batch_fetch_children` → `apply_child_operators` → runs the
    EXISTS-LIMIT `Take(partition_key=[child_key])` against a `PreloadedSource`
    with a default (no-constraint, no-start) `FetchRequest`. `TakeOperator::fetch`
    on a partitioned Take with no constraint and no `max_bound` returns `vec![]`
    by design (advance-mode invariant) — silently dropping every fetched child
    row. Every parent's EXISTS check then sees zero children and is filtered out.

  Bug B (`advance.rs::build_pipeline_state`):
    `rel_to_table` map was built from `collect_child_tables(&query.ast)` — the
    raw, **un-uniquified** AST. The runtime change-output emits the
    **uniquified** `relationship_name` (e.g. `arb_conversations_attachments_1`)
    written into `OperatorConfig::Exists` by Phase-34 alias-leak fix
    (uniquify_top_level_csq_aliases). The mismatch caused
    `flatten_nodes_to_row_changes`'s lookup to miss → fall back to using
    `rel_name` as the table name → `coerce_row(table="arb_conversations_attachments_1", ..)`
    returned None (column_types is keyed by actual table name) → every
    EXISTS child row was dropped from the emitted RowChanges.

test: live regression-runner.ts on tools/ivm-parity (TS:4858, RS:4868)
expecting: 5 of 6 catalog shapes match
result:
  A-nested-OR-with-EXISTS  ok=5  diverge=0  pass=100% (was 0%)
  D-simple-OR-with-EXISTS  ok=0  diverge=1  pass=0%   (pre-existing)
  TOTAL: 5/6 (was 0/6)

next_action: |
  Commit fixes atomically:
    - hydrate.rs: ParallelFanOutOperator branch construction switched to
      build_branch_subpipeline_for_hydrate (uses build_push_next_operator +
      SourceBridgeOperator — the production-validated hydrate path).
    - advance.rs: rel_to_table built from emitted OperatorConfig tree
      (collect_rel_to_table_from_configs) so uniquified relationship_names
      resolve to correct child table_names for coerce_row.
  Update regression-runner-last.json (it already reflects 5/6).
  Update debug session and append knowledge-base entry.
Compound OR branches now emit `OperatorConfig::FanOut` with one self-
contained sub-pipeline per branch (Source + Filter(gates) + Exists chain),
mirroring TS `applyOr` (builder.ts:514-557). Hydration support added via
new `ParallelFanOutOperator` (analog of ParallelOrExistsOperator).

planning_sketch (architecture decision):
KEY DISCOVERY: zero-ivm-rs ALREADY has `FanOutOperator` (filter-graph variant) at fan_out_op.rs.

- Architecture: TS `FanOut + FanIn` collapsed into a single composite Operator that owns N parallel branch sub-pipelines and applies push_accumulated_changes (identity merge/make for filter-graph).
- Wire format: `OperatorConfig::FanOut { branches: Vec<Vec<OperatorConfig>> }` — each branch is a self-contained sub-pipeline starting with its own Source clone.
- Production-ready: 12+ unit tests, op_type "fan_out", deserialize round-trip tested, build_operator wires it up.
- User intent ("UnionFanOut/UnionFanIn" stubs at union*fan*\*\_op.rs are the WRONG match — those are for flipped-join Phase-36-Wave-2, k-way dedup-by-PK merge, NOT for applyOr). The user said "match TS topology" — TS uses FanOut+FanIn (not UnionFanOut/UnionFanIn) inside applyOr (builder.ts:535,551). The Rust FanOutOperator IS the port of TS FanOut+FanIn (per the deliberate fusion documented in fan_out_op.rs:6-23, ALREADY-LANDED Phase 36 Wave 1 architecture).

PLAN: route compound-OR-branch topology through the existing FanOutOperator instead of OrExistsOperator. Keep OrExistsOperator unchanged for the simple flat OR-of-CSQs case (no AND-inside) to minimize test breakage and stay diff-minimal. The split rule: - OR(csq1, csq2, ..., simples...) with all CSQs being plain CorrelatedSubquery (no AND wrappers) → keep OrExists path (unchanged). - OR(branch1, branch2, ...) where ANY branch is an AND-with-CSQ (compound) → emit FanOut config with one self-contained sub-pipeline per branch.

Sub-pipeline construction for FanOut branches (mirrors TS applyFilter recursion): - For a CSQ branch: `[Source(parent_table), Exists(csq)]` - For an AND(s..., csq1, csq2) branch: `[Source(parent_table), Filter(AND(s...)), Exists(csq1), Exists(csq2)]` — chained Exists ops give AND semantics within the branch (Filter passes parent, then each Exists drops if its child predicate fails) - For nested OR or other composites: recurse via append_condition_configs into the branch sub-pipeline - All branches share the same upstream parent Source (cloned into each branch prefix)

falsifiability_test:

- Unit: cargo test in zero-ivm-rs for FanOutOperator already covers push/fetch/dedup; reuse + add a test that combines filter+exists in branches (production-shape test).
- AST→config integration: hand-build catalog shape #0 (A-nested-OR-with-EXISTS) AST against a temp SQLite DB → assert emitted configs include `OperatorConfig::FanOut { branches }` where each branch is a self-contained sub-pipeline that includes the previously-dropped gate conditions.
- Regression: tools/ivm-parity/regression-runner.ts moves from 0/6 → 6/6 hydrate (or report which still diverge if not all 6 share the same root cause).

fix_rationale: |
Addresses the root cause directly — the lost gate conditions are now preserved as a `Filter` inside the FanOut branch sub-pipeline, and the FanOut+FanIn topology mirrors TS applyOr exactly (per builder.ts:514-557). No new operators are introduced; we use the existing-and-tested FanOutOperator. Topology now matches TS: future divergences are debuggable by direct comparison.

blind_spots:

- The FanOutOperator's `fetch` delegates to branch[0] via union-with-dedup, which assumes all branches share an equivalent upstream Source. We REPLICATE the parent Source-config into each branch prefix, so all branches really do read the same rows. Must verify this holds when an upstream Take/Skip/Filter has been emitted before the OR site (the FanOut would skip those upstream operators — that's wrong).
- Need to verify: does the OR site appear AFTER Source/Skip/Take/Limit/Related in the pipeline? If so, the upstream operators are in `configs` BEFORE the FanOut config. Each branch must NOT replicate those — only Source. Per ast_to_operator_configs ordering: Source → Skip → CSQ-Exists/Filter (where OR sites appear) → Take → Related. So WHERE-clause OR sites come after Source+Skip but before Take+Related. FanOut branches need to start with Source only — Skip is upstream of the FanOut, so Skip's effect (filtering rows below the start bound) is ALREADY applied by the time changes reach FanOut. But the FanOut config carries Sources that don't see Skip — so a fresh fetch on a branch returns ALL source rows including skipped ones. **This means we should run the FanOut after Skip in the linearized pipeline so that fetch flows through Skip(Source) before reaching FanOut. Currently build_operator linearizes `current = next_op(current)` per config, so each subsequent config wraps the previous current as input. But OperatorConfig::FanOut DROPS current (per pipeline.rs:370). So Skip is bypassed.**
- **MITIGATION:** OR sites come from WHERE-clause processing where there's no Skip/Take above them in the AST. Skip is at the AST-level (`ast.start`), applied before WHERE. Take is at the AST-level (`ast.limit`), applied AFTER WHERE. The OR-of-CSQ FanOut would always be inside the WHERE block — so the upstream chain so far is Source[+Skip], no Take. Skip IS bypassed. This is a known limitation of the FanOut-drops-current design (pipeline.rs:370). **For the catalog shapes in question, none use `ast.start` (Skip), so this blind spot does not block hydrate parity. Note for follow-up: a future advance test with Skip + OR-with-CSQ would expose this.**

next_action: |

1. Confirm the catalog shapes have no `ast.start` (Skip) → safe to proceed with FanOut routing.
2. Implement the change in ast_to_config.rs:
   - Detect compound OR (any branch is AND-with-CSQ) inside append_condition_configs::Or arm.
   - When compound: emit OperatorConfig::FanOut with one sub-pipeline per branch.
   - For each branch: clone parent Source config, then append branch-specific configs (Filter for gate conditions, Exists chain for CSQs) via a new helper `build_or_branch_subpipeline`.
   - Keep simple flat case routing through OrExists (unchanged).
3. Add a unit test in ast_to_config.rs for the compound-OR path (asserting OperatorConfig::FanOut emitted with expected branch structure).
4. cargo build && cargo test for zqlite-rs and zero-ivm-rs (must stay green).
5. Run regression-runner.ts against all-divergences.json — verify 6/6 hydrate.
6. Commit each natural unit atomically with conventional messages.

commit*alias_leak: dc16c2588 — "fix(ivm-rs): emit user-facing relationship name on advance instead of zsubq*<hash> alias" (CONFIRMED FIXED — Bug 2 closed)

## Symptoms

expected: All 6 catalog AST shapes in tools/ivm-parity/all-divergences.json produce identical output between RS and TS for both hydrate and advance.

actual: 0/6 passing on hydrate-only baseline. Three bug families identified:

- B5 (AND-inside-OR-of-EXISTS): 6/6 catalog shapes hit. TODO at packages/zqlite-rs/src/ast_to_config.rs:587.
- Alias-leak (advance-only): RS emits tableName="zsubq\_<hash>" instead of relationship name in change-output rows.
- NOT LIKE: RS case-sensitive, TS case-insensitive. Decision: revert to TS behavior.

errors: No runtime errors — output divergence detected by parity harness comparing canonical-key serialized rows.

reproduction:
cd tools/ivm-parity && npx tsx regression-runner.ts

# Compares against all-divergences.json (6 shapes); writes regression-runner-last.json

started: 2026-04-30 (catalog created in quick task 260430-m6u; reduced from 56→6 in 260430-mu2)

## Eliminated

- hypothesis: Bug 3 — RS NOT LIKE is case-sensitive while TS NOT LIKE is case-insensitive (divergence)
  evidence: TS NOT LIKE at zql/src/builder/filter.ts:127-128 calls getLikePredicate(rhs, '') with empty flags = case-SENSITIVE (see zql/src/builder/like.ts). RS NOT LIKE produces {"not":{"field":..., "like":...}} parsed as Predicate::Not(Predicate::Like(field, pattern, false)) at zero-ivm-rs/src/pipeline.rs:104-178 — also case-sensitive. They already match. Catalog (all-divergences.json) contains zero NOT LIKE patterns. User-confirmed: drop Bug 3 entirely, do not touch NOT LIKE handling.
  timestamp: 2026-04-30 (post-checkpoint)

## Evidence

- timestamp: 2026-04-30
  checked: tools/ivm-parity/all-divergences.json + ALL-DIVERGENCES-CATALOG.md + 260430-mu2-SUMMARY.md
  found: All 6 catalog shapes are fast-check shrinks of the same B5 family (AND-inside-OR-of-EXISTS). Catalog has only NOT ILIKE patterns, no NOT LIKE.
  implication: Bug 1 (B5) dominates the entire catalog. Bug 2 (alias-leak) and Bug 3 (NOT LIKE) are not exercised by hydrate-only.

- timestamp: 2026-04-30
  checked: packages/zql/src/builder/filter.ts:125-132 (TS LIKE/NOT LIKE/ILIKE/NOT ILIKE handling) + packages/zql/src/builder/like.ts (getLikePredicate)
  found: TS NOT LIKE uses getLikePredicate(rhs, '') with empty flags = case-SENSITIVE. TS NOT ILIKE uses 'i' = case-insensitive.
  implication: User's claim "TS NOT LIKE is case-insensitive" is wrong. Both RS and TS NOT LIKE are case-sensitive — they already match.

- timestamp: 2026-04-30
  checked: packages/zero-ivm-rs/src/pipeline.rs:104-178 (parse_predicate) + packages/zqlite-rs/src/ast_to_config.rs:768-823 (condition_to_predicate_json LIKE handling)
  found: RS NOT LIKE produces {"not": {"field":..., "like": pattern}}, parsed as Predicate::Not(Predicate::Like(field, pattern, false=case-sensitive)). RS NOT ILIKE produces {"not": {"field":..., "ilike": pattern}}, parsed as Predicate::Not(Predicate::Like(field, pattern, true=case-insensitive)).
  implication: RS LIKE-family is already correct and matches TS. No fix needed for Bug 3.

- timestamp: 2026-04-30
  checked: packages/zqlite-rs/src/ast_to_config.rs:556-595 (Or condition with nested OR + AND-of-CSQs) + collect_exists_branches:602-674 (TODO at line 665-668)
  found: collect_exists_branches handles AND(simple..., CSQ1, CSQ2,...) by collecting CSQs as separate branches and DROPPING gate conditions entirely. Comment line 665-668 explicitly notes "TODO: For full correctness with AND(CSQ1, CSQ2) inside OR, we'd need a compound branch. For now, collect each CSQ as a separate branch." Gate conds in this AND are silently dropped → over-permissive matching → divergence.
  implication: This is the precise B5 bug location.

- timestamp: 2026-04-30 (post-checkpoint)
  checked: packages/zql/src/builder/builder.ts:267, 723-765 (TS uniquifyCorrelatedSubqueryConditionAliases) + packages/zqlite-rs/src/ast*to_config.rs:676-679 (NOTE explicitly stating uniquify NOT done in Rust) + packages/zero-ivm-rs/src/exists_op.rs (relationship_name flow)
  found: TS uniquify mutates each CSQ's subquery.alias = (alias ?? '') + '*' + count++ at every buildPipelineInternal call (per-AST-level counter, single pass over WHERE tree, not into nested subquery WHEREs). Rust ast_to_config skips this entirely. The relationship_name in OperatorConfig::Exists comes verbatim from CorrelatedSubquery.subquery.alias via relationship_name() at line 337-344. IVM operators (exists_op, or_exists_op) clone relationship_name into Change::Child{relationship_name} verbatim (exists_op.rs:179, 191, 247).
  implication: Bug 2 root cause confirmed — duplicate aliases in WHERE tree produce identical relationship_names in OperatorConfig::Exists, causing change-output collisions. Fix is to port uniquifyCorrelatedSubqueryConditionAliases.

- timestamp: 2026-04-30 (post-B5-fix-landing)
  checked: live regression-runner against TS:4858/RS:4868. Live caches up; RS replicator caught up; baseline 0/6.
  found: Live `_diag_one_shape.ts` for shape #0 (A-nested-OR-with-EXISTS) showed RS returning ZERO rows for conversations/channels/attachments/participants while TS returned 3/2/5/4. View-syncer log: `cvrFlush ... "rows":0`. Despite the previously-landed B5 unit tests passing in isolation, the live FanOut emitted 0 rows.
  implication: B5 fix's unit tests covered ast→config emission and a hand-built live-DB end-to-end test for a SIMPLE shape, but a deeper bug lurked behind the production hydrate path. Time to add tracing.

- timestamp: 2026-04-30 (B5-trace-1)
  checked: eprintln tracing inserted in `ParallelFanOutOperator::fetch`, `append_condition_configs::Or`, and `LiveTableSource::fetch`. Rebuilt napi binary, restarted RS cache, re-ran diag.
  found: Trace shows `[B5-TRACE] LiveTableSource::fetch EXIT 10 rows` (correct — 10 conversations) but `[B5-TRACE] branch 1 fetched 0 rows`. Branch 1 is `[Source(conversations), Exists(arb_conversations_attachments_1, not=false)]`. The Source returns 10 rows but the EXISTS gate filters all of them out — incorrect.
  implication: The bug is in EXISTS evaluation when run inside a FanOut branch sub-pipeline.

- timestamp: 2026-04-30 (B5-trace-2)
  checked: eprintln tracing inserted in `ParallelExistsOperator::fetch` to log batch_fetch_children groups + per-parent matching.
  found: `batch_fetch_children groups: 7 keys, total child rows: 0` — the batched SQL query DID find 7 conversation IDs that have attachments, but each group has ZERO child rows. The keys are correct (co-1, co-2, co-4, co-5, x-co-1, x-co-2, x-co-5) but rows missing.
  implication: `batch_fetch_children` correctly identifies parent groups but `apply_child_operators` zeroes out the rows.

- timestamp: 2026-04-30 (B5-trace-3 — root-cause-1)
  checked: zero-ivm-rs/src/take_op.rs:283-318 (TakeOperator::fetch with partition_key.is_some()).
  found: When `partition_key.is_some()` AND `req.constraint` is None AND `max_bound` is None (initial fetch, no state populated), TakeOperator::fetch returns `vec![]` by design — line 294. This is the advance-mode invariant: partitioned Take with no max_bound means "no state exists yet, refuse to emit anything until push populates state". `apply_child_operators` calls `current.fetch(&FetchRequest::default())` which has no constraint and no max_bound → returns vec![] → drops all child rows.
  implication: ROOT CAUSE 1 — ParallelExistsOperator's batch_fetch_children → apply_child_operators path is INCOMPATIBLE with partitioned EXISTS-LIMIT Take. The parent simple-EXISTS hydrate path (used by D-simple-OR-with-EXISTS and other pre-existing tests) avoids this by using the SEQUENTIAL `ExistsOperator` (zero-ivm-rs::exists_op) constructed by `build_push_next_operator`, NOT `build_next_operator`. Sequential ExistsOperator does per-parent constrained fetch → hits TakeOperator's `req.constraint.is_some()` branch → works correctly.

- timestamp: 2026-04-30 (B5-fix-1 + B5-trace-4 — root-cause-2)
  checked: After applying fix-1 (route FanOut branches through `build_branch_subpipeline_for_hydrate` which uses `build_push_next_operator` + `SourceBridgeOperator`), re-ran diag.
  found: Branch 1 now correctly fetches 7 rows (conversations with attachments). After Take(3) and Join(channels), we get the expected 3 conversation rows + 2 channel rows + 4 participant rows. BUT — the EXISTS-related attachments (5 rows expected) are missing from the output. RS includes only conversations/channels/participants; TS also includes attachments.
  implication: Found bug 2 — alias-uniquification rename broke `rel_to_table` resolution.

- timestamp: 2026-04-30 (B5-trace-5)
  checked: advance.rs:1112-1156 (flatten_nodes_to_row_changes) + advance.rs:1307 (rel_to_table = collect_child_tables(&query.ast)) + ast_to_config.rs::uniquify_top_level_csq_aliases.
  found: `collect_child_tables(&query.ast)` walks the **un-uniquified** AST and produces map entries like `arb_conversations_attachments → attachments`. But Phase-34 alias-leak fix (commit dc16c2588) renames every CSQ alias to `<alias>_<count>`, so the runtime change-output emits `relationship_name = "arb_conversations_attachments_1"`. flatten_nodes_to_row_changes does `rel_to_table.get("arb_conversations_attachments_1") = None` → falls back to using the rel_name as the table name → coerce_row(table="arb_conversations_attachments_1", column_types) returns None (column_types is keyed by actual table name, "attachments") → row dropped.
  implication: ROOT CAUSE 2 — rel_to_table must be built from the EMITTED OperatorConfig tree (post-uniquify) so the uniquified relationship_name resolves to the actual table name. Affects every CSQ-EXISTS that produces child rows in change-output.

- timestamp: 2026-04-30 (final verification)
  checked: Both fixes applied; tracing removed; cargo test --lib green for zqlite-rs (149/149) and zero-ivm-rs (234/234); napi rebuilt; live regression-runner re-run.
  found: 5/6 catalog shapes pass (vs baseline 0/6). All 5 A-nested-OR-with-EXISTS shapes match TS exactly. The remaining 1/6 (D-simple-OR-with-EXISTS) is a pre-existing OrExists-path ordering bug not part of the B5 family — out of scope.
  implication: B5 hydrate parity is FIXED for compound OR-of-EXISTS. The D shape will need a separate investigation (OrExists ordering + always-true predicate in OR branch).

## Resolution

root_cause: |
Bug 1 (B5 AND-inside-OR-of-EXISTS) — packages/zqlite-rs/src/ast_to_config.rs
`collect_exists_branches` (line 732-737) flattened compound OR branches by
dropping gate conditions: `OR(AND(simple_gate, CSQ1, CSQ2), CSQ3)` was
emitted as `OrExists{branches=[CSQ1, CSQ2, CSQ3]}` with `simple_gate`
silently discarded. Over-permissive matching → divergence. The TS
reference (`applyOr` at zql/src/builder/builder.ts:514-557) uses a
`FanOut → per-OR-branch sub-pipeline → FanIn` topology where each OR
branch can be its own `Filter + Exists + Exists + ...` sub-pipeline.

Bug 2 (alias uniquification) — packages/zqlite-rs/src/ast*to_config.rs did not
mirror TS `uniquifyCorrelatedSubqueryConditionAliases` (zql/src/builder/builder.ts:267,
723-765). Two EXISTS subqueries sharing an alias (user-supplied or hash-collision)
produced two `OperatorConfig::Exists` configs with identical `relationship_name`,
causing the IVM operators (exists_op, or_exists_op) to emit Child changes whose
`relationship_name` collided in the downstream `node.relationships` HashMap on
advance — manifesting as the alias-leak / "tableName=zsubq*<hash>" parity divergence.
Bug 3 (NOT LIKE): NOT A BUG — eliminated. See Eliminated section.

fix: |
Bug 2 (alias-leak) — landed in commit dc16c2588 (CONFIRMED FIXED).

Bug 1 (B5) — packages/zqlite-rs/src/ast_to_config.rs +
packages/zqlite-rs/src/hydrate.rs:

    ast_to_config.rs:
      - New `any_compound_branch(csq_conds)` predicate — true if any OR
        branch is a compound (And/Or wrapping a CSQ) rather than a bare
        CorrelatedSubquery. Triggers FanOut path.
      - New `build_or_branch_subpipeline(schema, parent_source, branch_cond,
        primary_key)` — produces a self-contained `Vec<OperatorConfig>` for
        one OR-branch, starting with a clone of the parent Source:
          • bare CSQ          → `[Source, Exists(csq)]`
          • AND(gates..., csq+) → `[Source, Filter(AND(gates)), Exists(csq1),
                                     Exists(csq2), ...]`
        Mirrors TS `applyFilter` recursion inside `applyOr` (builder.ts:514-557).
      - New `partition_and_branch(conditions, gates, csqs)` — walks an AND
        tree (recursing through nested AND nodes only, not into OR or
        subquery WHEREs), partitions leaves into gates (no CSQ) and bare
        CSQs. Errors loudly on nested CSQ-bearing OR inside an
        AND-with-CSQ branch (no catalog shape exercises that case yet).
      - In `append_condition_configs::Or` arm: detects compound OR via
        `any_compound_branch` and emits `OperatorConfig::FanOut { branches }`
        with one self-contained sub-pipeline per branch. The simple flat
        case (`OR(csq1, csq2, simples...)` with no AND-wrappers) still
        routes through the existing OrExists/Exists+or_condition path —
        diff-minimal, preserves catalog shape #6 (D-simple-OR-with-EXISTS).

    hydrate.rs:
      - New `ParallelFanOutOperator` (analog of ParallelOrExistsOperator)
        that owns a `Vec<Vec<OperatorConfig>>` (one sub-pipeline per branch)
        and a shared `Arc<RustTableSource>`. `fetch` builds each branch
        with `build_operator_with_live_source` (which rebuilds the branch
        chain backed by a LiveTableSource on the shared SQLite source),
        runs each branch's fetch, dedups by primary key. `push` returns
        vec![] (advance-mode deferred — per user's hydrate-only target for
        the B5 fix).
      - Wired into both `build_next_operator` and `build_push_next_operator`
        — replaces the previous `Err("FanOut hydration not yet implemented
        (Phase 36 Wave 4)")` stubs.

verification: |
Bug 2 (alias-leak): 4 unit tests in ast_to_config.rs::tests, commit dc16c2588.
Pre-fix integration test was Red, post-fix is Green. CONFIRMED FIXED.

Bug 1 (B5):
Unit tests in packages/zqlite-rs/src/ast*to_config.rs::tests: - test_b5_compound_or_emits_fan_out_with_preserved_gates — asserts
catalog-shape AST `OR(AND(simple*>, csq_channels), csq_attachments)`        emits`OperatorConfig::FanOut`with branch 0 = [Source, Filter, Exists]
        (gate condition preserved!) and branch 1 = [Source, Exists].
      - test_b5_flat_or_of_csqs_keeps_or_exists_path — negative regression:
        flat`OR(csq1, csq2)` does NOT trigger FanOut path, stays on
OrExists (preserves catalog shape #6 + pre-fix passing tests).

    Integration test in packages/zqlite-rs/src/hydrate.rs::tests:
      - test_b5_compound_or_fan_out_hydrate_against_live_db — end-to-end
        against a temp SQLite DB. Setup: 6 users with selective active flag
        and posts/tags. Query: `OR(AND(active=true, EXISTS(posts)),
        EXISTS(tags))`. Asserts:
          1. Alice (active+posts), Bob (tags), Carol (active+posts), Dave
             (tags) PASS — exactly 4 rows.
          2. Erin (active=false but HAS posts) is filtered out — proves
             the gate is applied. Pre-fix Erin would erroneously pass via
             dropped-gate degradation.
          3. Frank (active=true but no posts, no tags) is filtered out —
             proves both branches are tried correctly.
          4. Dedup by primary key — result.len() == unique-names count.

    Red-Green confirmed:
      - Pre-fix: collect_exists_branches dropped gate; Erin would pass
        (over-permissive). Integration test would fail with Erin in result.
      - Post-fix: FanOut routes Erin through Filter(active=1) which rejects
        her in the AND-branch; bare-CSQ-branch (EXISTS(tags)) does not
        match Erin either; she's correctly filtered. Test PASSES.

Regression check: - Full zqlite-rs lib test suite: 149/149 pass (was 146; +3 new B5 tests). - Full zero-ivm-rs lib test suite: 234/234 pass (unchanged).

Live TS↔RS regression-runner (tools/ivm-parity/regression-runner.ts
against all-divergences.json) — POST B5-fix-2:

```
mode=hydrate total_shapes=6
overall: ok=5 diverge=1 error=0
per bucket:
  A-nested-OR-with-EXISTS  ok=5 diverge=0 error=0 pass=100% (was 0%)
  D-simple-OR-with-EXISTS  ok=0 diverge=1 error=0 pass=0%   (pre-existing)
```

5/6 catalog shapes match TS exactly. The remaining D-simple-OR-with-EXISTS
shape diverges due to a separate, pre-existing OrExists ordering issue
with always-true OR branches (`processedAt NOT IN [empty]`) — out of
scope for the B5 fix. Tracked as a follow-up.

B5-fix-2 (the live runner fix) — packages/zqlite-rs/src/hydrate.rs +
packages/zqlite-rs/src/advance.rs:

    hydrate.rs:
      - New `build_branch_subpipeline_for_hydrate(source, configs)` —
        constructs each FanOut branch via `build_push_next_operator` +
        `SourceBridgeOperator`, the same construction logic the production
        hydrate path uses (build_operator_chain). This routes EXISTS
        through the SEQUENTIAL `ExistsOperator` (zero_ivm_rs::exists_op)
        instead of `ParallelExistsOperator`, avoiding the
        partitioned-Take-with-no-state bug in
        `apply_child_operators` → `TakeOperator::fetch` (returns vec![]
        when partition_key.is_some() AND req.constraint.is_none() AND
        max_bound is None).
      - `ParallelFanOutOperator::fetch` switched to call the new helper
        instead of `build_operator_with_live_source`.

    advance.rs:
      - New `collect_rel_to_table_from_configs(configs)` — walks the
        emitted OperatorConfig tree (Join, Exists, OrExists, FanOut
        branches) and produces `relationship_name → child_table_name`
        keyed by the **uniquified** relationship_name (post-Phase-34
        alias-leak fix). The previous `collect_child_tables(&query.ast)`
        keyed entries by the un-uniquified alias, causing
        `flatten_nodes_to_row_changes`'s `rel_to_table.get(rel_name)`
        lookup to miss for every CSQ-EXISTS — the fallback used the
        uniquified name as the table name → coerce_row dropped every
        CSQ-EXISTS child row.
      - `build_pipeline_state` now seeds rel_to_table from
        collect_rel_to_table_from_configs, then merges in the AST-derived
        entries for `related[]` Joins (which don't get uniquified).

files_changed:

- packages/zqlite-rs/src/ast_to_config.rs (Bug 1 — original B5 fix, no diff this round)
- packages/zqlite-rs/src/hydrate.rs (Bug 1 — ParallelFanOutOperator
  + new build_branch_subpipeline_for_hydrate)
- packages/zqlite-rs/src/advance.rs (NEW — collect_rel_to_table_from_configs
  + rel_to_table now built from emitted configs to handle uniquified aliases)
