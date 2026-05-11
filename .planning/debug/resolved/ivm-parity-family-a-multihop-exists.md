---
status: resolved
trigger: 'Family A: multi-hop EXISTS chain returns 0 rows in RS where TS returns N rows. 6 hydrate divergences cluster here.'
created: 2026-05-11T00:00:00Z
updated: 2026-05-11T13:00:00Z
resolved_commit: e07b6634b
---

## Current Focus

reasoning_checkpoint:
hypothesis: "Bug A pattern (documented in knowledge-base / resolved B5 session) recurs in nested-EXISTS production hydrate path. Top-level sequential ExistsOperator's child pipeline is `LiveTableSource → ParallelExistsOperator(inner) → TakeOperator(partition_key=child_field)`. The outer ExistsOperator calls `child.fetch(constraint={ck: parent.pk})` — single-level works because the constraint reaches the outer Take. But for nested EXISTS, the inner ParallelExistsOperator's `batch_fetch_children` calls `apply_child_operators(child_config=[Source(channels), Filter, Take(partition_key=['id'])])`, which constructs `PreloadedSource → Filter → TakeOperator(partition_key=Some, no constraint, no max_bound)` and calls `.fetch(FetchRequest::default())`. TakeOperator::fetch at take_op.rs:286-295 returns vec![] when partition_key.is_some() && req.constraint.is_none() && max_bound.is_none(). All channel rows dropped → inner EXISTS check sees 0 children for every conversation → conversations all rejected → outer EXISTS check sees 0 children for every message → RS=0."
confirming_evidence: - "ast_to_config.rs:553 sets `child_partition = Some(related.correlation.child_field.clone())` for the recursive ast_to_operator_configs call inside CSQ; apply_exists_limit at line 994-998 then appends a Take with this partition_key. So every EXISTS_LIMIT Take in a child config has partition_key.is_some()." - "hydrate.rs:993-1015 build_push_next_operator builds Exists as sequential ExistsOperator. hydrate.rs:1262 calls build_operator_with_live_source which uses build_next_operator (line 798-799). build_next_operator at line 1006 builds Exists as ParallelExistsOperator. So nested Exists below the top-level becomes Parallel." - "ParallelExistsOperator::fetch (hydrate.rs:444-501) → batch_fetch_children → apply_child_operators(child_config, nodes) → for each config in child_config[1..] uses build_next_operator → fetch(FetchRequest::default())." - "take_op.rs:284-295: when partition_key.is_some() AND req.constraint.is_none(), checks max_bound; if max_bound.is_none() returns vec![]." - "Identical to Bug A documented at `.planning/debug/knowledge-base.md` (resolved B5/B6 session) — same root cause but at a different call site. Previous fix at hydrate.rs:710 explicitly says 'tripping a partitioned-Take-with-no-state bug in apply_child_operators' — it only worked around it for FanOut branches, not the production hydrate path."
falsification_test: "After applying the fix (per-group constraint in apply_child_operators), fuzz_00782 must produce 8 rows in RS. If it still produces 0, hypothesis is wrong."
fix_rationale: "Root cause is that apply_child_operators calls .fetch(FetchRequest::default()) on a pipeline whose last operator is a partitioned Take with no max_bound. All nodes in a single apply_child_operators call belong to one parent-group, so they share the partition_key value. We extract the partition_key from any Take in child_config[1..] and build a constraint {pk_field: nodes[0].row[pk_field]}, then pass that constraint to .fetch(). This hits TakeOperator's `req.constraint.is_some()` branch (take_op.rs:287-289), which derives state-key from constraint and proceeds with normal initial-fetch logic. The fix recurses correctly: if the chain contains a nested ParallelExistsOperator, its own batch_fetch_children → apply_child_operators call self-heals via the same constraint-derivation logic."
blind_spots: "1) The fix touches apply_child_operators which is shared by ParallelJoinOperator (related[]) and ParallelExistsOperator. For related[] with user limit, this changes behavior — needs regression check (catalog 6/6 should still pass). 2) ParallelOrExistsOperator also calls batch_fetch_children per branch — same fix applies transitively. 3) The constraint synthesized may be the wrong field if child_config has multiple Take operators with different partition_keys; but in current builder output, there's only one Take per child_config (apply_exists_limit appends one, or user limit applies one)."

hypothesis: Multi-hop EXISTS hydrate fails because apply_child_operators invokes a partitioned Take with no constraint and no max_bound, returning vec![] and dropping all rows before the inner EXISTS can evaluate.
test: Apply fix (synthesize partition constraint in apply_child_operators from first node), rebuild zqlite-rs, restart RS, and run fuzz_00782. Expect RS=8 to match TS=8.
expecting: All 6 Family A cases flip from RS=0 to RS=TS. Catalog 6/6 stays green.
next_action: Implement the fix in packages/zqlite-rs/src/hydrate.rs::apply_child_operators. Cite TS reference (TS uses sequential Join/Exists throughout — packages/zql/src/builder/builder.ts:626-646 builds child pipeline via buildPipelineInternal and wraps in Join, no batch shortcut).

## Symptoms

expected: AST `messages.whereExists(conversation, q => q.whereExists(channel, q2 => q2.where('id', "ch-pub-1")))` returns 8 rows in RS (matching TS).
actual: RS returns 0 rows for every multi-hop EXISTS chain (2+ levels of nested whereExists).
errors: No runtime errors. RS view-syncer flushes CVR with `"rows":0`. Silent empty output.
reproduction: 1198-AST corpus harness; minimal repro = fuzz_00782. 6 cases all produce RS=0.
started: Always broken for 2+ level nested EXISTS. Catalog 6/6 passes (none exercise this pattern).

## Eliminated

(none yet)

## Evidence

- timestamp: 2026-05-11T initial
  checked: .planning/debug/ivm-parity-remaining-divergences.md
  found: 6 hydrate cases in Family A all produce RS=0. Pattern is whereExists(rel, q => q.whereExists(rel2, q2 => q2.where(...))).
  implication: Single root cause likely covers all 6 cases. Targeting fuzz_00782 as minimal repro.

- timestamp: 2026-05-11T11:30
  checked: ast_to_config.rs:544-579 + 737-820 (CSQ recursion), hydrate.rs:146-249 (apply_child_operators + batch_fetch_children), hydrate.rs:444-501 (ParallelExistsOperator), hydrate.rs:826-855 (B5 build_branch_subpipeline_for_hydrate), take_op.rs:284-396 (TakeOperator::fetch).
  found: Traced full data flow for fuzz_00782. Top-level uses sequential ExistsOperator (correct constraint flow), but its child pipeline is built via build_operator_with_live_source which uses build_next_operator → ParallelExistsOperator for any nested Exists. ParallelExistsOperator::fetch invokes batch_fetch_children → apply_child_operators which always called .fetch(FetchRequest::default()). Take with partition_key=Some + no constraint + no max_bound → returns vec![] (take_op.rs:294). This dropped all rows before the inner EXISTS could evaluate. B5 fix (hydrate.rs:710-738) explicitly documents this same bug pattern but only carved around it for FanOut branches.
  implication: Root cause confirmed. Fix must synthesize a partition constraint in apply_child_operators.

- timestamp: 2026-05-11T12:00
  checked: Applied fix part 1 (synthesize_partition_constraint) — built, restarted RS, ran 6 Family A cases.
  found: 5 of 6 cases now match TS (fuzz_00782=8/8, fuzz_00985=5/5, seed_09=5/5, seed_12=25/25, seed_19=11/11). seed_23 still RS=0.
  implication: Hypothesis right for simple nested EXISTS but seed_23 has additional nesting (3-level with innermost FanOut). Need to also pass child-table source through apply_child_operators.

- timestamp: 2026-05-11T12:15
  checked: Re-traced seed_23's 3-level structure. Level 2 ParallelExistsOperator's child_config is [Source(channels), FanOut(...), Take(partition_key=["id"])]. Its self.source = conv_source. apply_child_operators(source=conv_source, channels_config, ...) builds ParallelFanOutOperator passing source=conv_source. ParallelFanOutOperator::fetch uses SourceBridgeOperator(conv_source) — wrong table for channels branches.
  found: apply_child_operators was passing the OUTER source through to nested operators that needed the CHILD source. ParallelFanOutOperator and nested ParallelExistsOperator both rebuild SQL access from their `source` field — they need the table that child_config[0] names.
  implication: Need fix part 2: build a child-table source from child_config[0] via make_child_source and use it for build_next_operator inside apply_child_operators.

- timestamp: 2026-05-11T12:25
  checked: Applied fix part 2 (effective_source = make_child_source(source, child_config) for build_next_operator). Rebuilt, restarted RS, redeployed permissions.
  found: fuzz_00782=8/8, fuzz_00985=5/5, seed_09=5/5, seed_12=25/25, seed_19=11/11 all OK. seed_23 improved from RS=0 to RS=33/36. The remaining 3-row gap in seed_23 is on the participants relationship payload, not on the messages count — a different bug class (FanOut relationship-combining) outside Family A's "RS=0 for nested EXISTS" symptom.
  implication: Family A "RS=0" symptom completely eliminated. 5/6 cases fully pass. seed_23 remaining divergence is in relationship-output combination from FanOut branches and is outside the scope of this Family A session.

- timestamp: 2026-05-11T12:30
  checked: Catalog regression-runner — 6/6 OK (A-nested-OR-with-EXISTS 5/5 + D-simple-OR-with-EXISTS 1/1). Full 1198-AST hydrate sweep — ok=1183, diverge=7, error=8 (was ok=1176, diverge=14, error=8 pre-fix). Net +7 OK improvements.
  found: All 5 fully-fixed Family A cases (fuzz_00782, fuzz_00985, seed_09, seed_12, seed_19) flipped from diverge→ok. seed_23 still diverges but with different symptom (relationship-payload, not RS=0). No regressions in the catalog or other cases.
  implication: Fix is verified at the full-corpus level. The 7 remaining hydrate divergences are Families B (2), C (2), D (1), E (1), and seed_23's downstream relationship-payload — outside this session's scope.

## Resolution

root_cause: |
Two cascading bugs in packages/zqlite-rs/src/hydrate.rs::apply_child_operators caused multi-hop EXISTS hydration to return RS=0.

Bug 1 (partition constraint): The function called .fetch(FetchRequest::default()) on a pipeline whose tail operator was the EXISTS_LIMIT TakeOperator(partition_key=Some(child_field)) emitted by ast_to_config.rs::apply_exists_limit. With partition_key set, no constraint, and no max_bound, TakeOperator::fetch (take_op.rs:286-295) returns vec![] by design — silently dropping every child row. All nodes in one apply_child_operators invocation share a single parent-group's partition value (they were grouped by it in batch_fetch_children), so a constraint synthesized from any node's row at the partition_key fields produces a correct, narrow fetch that hits TakeOperator's req.constraint.is_some() branch.

Bug 2 (child-table source): apply_child_operators passed its `source` parameter (the OUTER table's source — e.g., conversations when called from a Level-2 ParallelExistsOperator over channels) straight into build_next_operator. Operators that internally rebuild sub-pipelines from this source — ParallelFanOutOperator (via build_branch_subpipeline_for_hydrate → SourceBridgeOperator) and nested ParallelExistsOperator — therefore queried the wrong table. For seed_23 (3-level EXISTS with innermost FanOut), this meant the channels FanOut branches were fetching from conv_source. make_child_source(source, child_config) builds the correct child source from child_config[0]'s Source variant, reusing the shared connection pool.

Both bugs surface only when EXISTS is nested at least two levels deep (single-level EXISTS uses sequential ExistsOperator at the top via build_push_next_operator, which already passes per-parent constraints correctly). Catalog shapes never exercised 2+ level nesting, which is why the bugs survived the B5 fix that documented this exact pattern (hydrate.rs:710-738) but only worked around it for FanOut-branch construction.
fix: |
In packages/zqlite-rs/src/hydrate.rs::apply_child_operators (2 changes in same function):

1. Synthesize a per-group constraint from any partitioned Take in child_config[1..] via a new helper synthesize_partition_constraint(child_config, nodes). The constraint takes nodes[0].row's values at the partition_key fields. Pass this constraint into .fetch() so the trailing TakeOperator hits its req.constraint.is_some() branch.

2. Build an effective_source via make_child_source(source, child_config) before iterating child_config[1..], and pass effective_source.clone() into build_next_operator. This ensures nested ParallelFanOutOperator and ParallelExistsOperator query the child table (channels), not the outer one (conversations or messages).

TS reference: packages/zql/src/builder/builder.ts:626-646 — applyCorrelatedSubQuery builds the child pipeline via buildPipelineInternal and wraps in a Join. TS Join.fetch is called per-parent with a constraint by the surrounding Exists; there is no batch shortcut. The Rust port batches for performance but the per-group invocation of apply_child_operators is analogous to one TS per-parent fetch — it must carry the partition value and use the right child source.
verification: |

- fuzz_00782 (2-level EXISTS): RS 0 → 8 rows (matches TS=8) ✓
- fuzz_00985 (2-level EXISTS): RS 0 → 5 rows (matches TS=5) ✓
- seed_09_attachments_conv_flip_exists_single_branch (2-level w/flip): RS 0 → 5 (matches TS=5) ✓
- seed_12_messages_whereExists_conv_whereExists_channel (2-level): RS 0 → 25 (matches TS=25) ✓
- seed_19_participants_whereExists_channel_whereExists_conversation (2-level): RS 0 → 11 (matches TS=11) ✓
- seed_23_messages_whereExists_conv_channel_or_and_exists (3-level + FanOut): RS 0 → 33 (TS=36). Messages/conversations/channels rows all correct (18/10/4), divergence shifted to participants relationship payload (TS=4, RS=1) — a different bug class (FanOut relationship-combining) outside Family A's "RS=0" symptom.
- Catalog regression: 6/6 OK (5 A-nested-OR-with-EXISTS + 1 D-simple-OR-with-EXISTS, all green).
- Full 1198-AST hydrate corpus: pre-fix 1176/14/8, post-fix 1183/7/8 (+7 OK, -7 diverge).
  files_changed:
- packages/zqlite-rs/src/hydrate.rs (apply_child_operators + new synthesize_partition_constraint helper)
  commit: e07b6634b
  deferred_followups:
- "seed_23 residual 3-row gap on `participants` relationship payload (TS=4, RS=1). Not a Family A symptom — Family A's `RS=0 for nested EXISTS` is fully resolved. The remaining divergence is FanOut relationship-output combining (channels FanOut emits one merged relationship list per parent message instead of one per parent×branch group). Deferred to a future Family E session focused on FanOut relationship-payload combining."
