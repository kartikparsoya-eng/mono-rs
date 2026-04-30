# Phase 36: FlippedJoin Family Port — Research

**Researched:** 2026-04-30
**Domain:** TS↔Rust IVM operator port for child-driven joins, dedup-merge, and filter-graph fork/merge
**Confidence:** HIGH (all primary claims [VERIFIED] against codebase or [CITED] from local source files; one [ASSUMED] item explicitly listed in §Assumptions Log)

## Summary

Phase 36 ports the FlippedJoin operator family from TS (`packages/zql/src/ivm/`)
to Rust (`packages/zero-ivm-rs/src/`) and wires the AST translation in
`packages/zqlite-rs/src/ast_to_config.rs` to actually emit the new operators
when a CSQ has `flip:true`. Currently the Rust path silently treats `flip:true`
as a regular Exists (`ast_to_config.rs:401-432, 456-485, 589-616`) — closing
deep-audit finding **B7** and removing parity allow-list entries `fuzz_00132`
and `fuzz_00133`.

**Primary recommendation.** Schedule six waves: Wave 0 research (port-mapping
tables for the two non-trivial operators), Wave 1 FanOut+FanIn (simple
filter-graph), Wave 2 UnionFanOut+UnionFanIn (dedup+reorder), Wave 3
FlippedJoin (child-driven join with overlay), Wave 4 ast_to_config translation
(`applyFilterWithFlips` equivalent), Wave 5 differential parity test removal +
new diff tests. Wave 6 verification gate. Calendar estimate 3-4 weeks at one
engineer-equivalent.

The **highest-risk translation** is generator-to-synchronous for UnionFanIn's
`push` accumulator + `fanOutDonePushing` protocol — the TS implementation uses
`fc.letrec`-style cooperative yields between branches that don't have a direct
sync analog. The synchronous translation works but the per-row relationship
merge in `pushAccumulatedChanges` is non-trivial; flag for spike if Wave 2
slips >3 days.

---

## Source File Inventory

### TS sources to port (1035 LOC total)

| File               | LOC | Operator                      | Complexity                                  |
| ------------------ | --: | ----------------------------- | ------------------------------------------- |
| `flipped-join.ts`  | 505 | `FlippedJoin`                 | HIGH — fetch + 2 push paths + overlay state |
| `union-fan-in.ts`  | 296 | `UnionFanIn` + `mergeFetches` | HIGH — dedup PQ + accumulator               |
| `union-fan-out.ts` |  57 | `UnionFanOut`                 | LOW — fork + protocol handoff               |
| `fan-in.ts`        |  94 | `FanIn`                       | LOW — filter-OR + accumulator               |
| `fan-out.ts`       |  83 | `FanOut`                      | LOW — filter fork                           |

### TS tests (reference only — port FIXTURES, not scaffolding)

| File                              |   LOC | Notes                                                       |
| --------------------------------- | ----: | ----------------------------------------------------------- |
| `flipped-join.fetch.test.ts`      |  1999 | fetch correctness; representative shapes only               |
| `flipped-join.push.test.ts`       | 10223 | push-parent + push-child; HUGE — pick representative shapes |
| `flipped-join.sibling.test.ts`    |  1188 | sibling-relationship cases                                  |
| `flipped-join.more-fetch.test.ts` |   245 | edge cases on fetch                                         |
| `union-fan-in.test.ts`            |   268 | dedup + reorder unit tests                                  |
| `union-fan-out.test.ts`           |   216 | protocol handoff                                            |
| `fan-out-fan-in.test.ts`          |   339 | filter-graph fork+merge                                     |

### Rust target files (new + modified)

| File                                           | Status |          Lines added (est.) |
| ---------------------------------------------- | ------ | --------------------------: |
| `packages/zero-ivm-rs/src/flipped_join_op.rs`  | NEW    |                        ~600 |
| `packages/zero-ivm-rs/src/union_fan_in_op.rs`  | NEW    |                        ~330 |
| `packages/zero-ivm-rs/src/union_fan_out_op.rs` | NEW    |                        ~100 |
| `packages/zero-ivm-rs/src/fan_in_op.rs`        | NEW    |                        ~120 |
| `packages/zero-ivm-rs/src/fan_out_op.rs`       | NEW    |                         ~80 |
| `packages/zero-ivm-rs/src/pipeline.rs`         | MOD    | ~80 (variants + match arms) |
| `packages/zero-ivm-rs/src/lib.rs`              | MOD    |       ~5 (mod declarations) |
| `packages/zqlite-rs/src/ast_to_config.rs`      | MOD    |                        ~200 |
| `packages/zqlite-rs/src/hydrate.rs`            | MOD    |                         ~40 |

**Estimated total LOC added across waves: ~1555 production + ~3500 test.**

---

## Port Mapping Tables

### FlippedJoin TS↔Rust intent map

| TS line                   | TS construct                                                                                                                      | Rust intent                                                                                                                                                                                                                                                                                                                                                       |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `flipped-join.ts:52-105`  | constructor + schema merge                                                                                                        | `FlippedJoinOperator::new` — store parent/child operators, parent_key/child_key, relationship_name. Schema is computed from parent and child via `merge_schemas(parent_schema, relationship_name, child_schema, hidden, system)`.                                                                                                                                 |
| `flipped-join.ts:74-78`   | `assert(parent !== child, ...)` and key-length assert                                                                             | `assert!(parent_arc.id != child_arc.id, ...)` and `assert_eq!(parent_key.len(), child_key.len(), ...)` at construction.                                                                                                                                                                                                                                           |
| `flipped-join.ts:99-104`  | `parent.setOutput({push: pushParent})` etc.                                                                                       | Rust uses synchronous `Operator::push` so we don't register output callbacks. Instead `FlippedJoinOperator::push_parent(change)` is called externally by `Operator::push` dispatch (which inspects `change.source`); see Phase 11/12 pattern.                                                                                                                     |
| `flipped-join.ts:124-148` | `*fetch(req)` — translate parent constraint to child constraint, fetch children                                                   | `fn fetch(&self, req: &FetchRequest) -> Vec<Node>` — synchronous. Translate parent constraint to child constraint (`translate_constraint(req.constraint, parent_key, child_key)`); call `child.fetch({constraint: child_constraint})`; collect into `Vec<Node>`.                                                                                                  |
| `flipped-join.ts:158-165` | overlay-undo: if inprogress is REMOVE, splice the removed node back into childNodes at sorted position                            | `if let Some(InProgressOverlay { change: Change::Remove(node), .. }) = self.in_progress_child.as_ref() { let pos = binary_search(&child_nodes,                                                                                                                                                                                                                    | n   | compare_rows(&node.row, &n.row)); child_nodes.insert(pos, node.clone()); }`                                                                                                                                                                                                                                                                                 |
| `flipped-join.ts:166-205` | open per-child parent iterators, prime nextParentNodes                                                                            | Rust collects child nodes upfront, then runs `let parent_streams: Vec<Vec<Node>> = child_nodes.iter().map(                                                                                                                                                                                                                                                        | cn  | { let constraint = build_join_constraint(&cn.row, &child_key, &parent_key); if !compatible(&constraint, &req.constraint) { vec![] } else { parent.fetch(&FetchRequest { constraint: merge(req.constraint, &constraint), ..req.clone() }) }}).collect();`. Position cursors are `Vec<usize>`indices into each`Vec<Node>` — synchronous priority-queue merge. |
| `flipped-join.ts:207-296` | `while(true)` k-way merge of parentIterators by parent's compareRows order; emit minParentNode with relationships                 | Rust: `BinaryHeap<(Reverse<RowKey>, child_idx, node_idx)>` — push first node from each non-empty stream; pop min, collect all child indexes whose current row equals min, advance cursors. Emit a `Node { row: min_parent.row, relationships: { rel_name: child_nodes_at_indexes } }`.                                                                            |
| `flipped-join.ts:247-284` | overlay applied per-emitted-parent — filter or generate-with-overlay based on whether change has been pushed past the cursor      | Rust: `let overlaid = apply_overlay_for_parent(&self.in_progress_child, &min_parent, &related_child_nodes, &child_key, &parent_key, &child_schema);` — pure function returns `Cow<[Node]>`.                                                                                                                                                                       |
| `flipped-join.ts:287-294` | `if (overlaidRelatedChildNodes.length > 0) yield {...minParentNode, relationships: {[relName]: () => overlaidRelatedChildNodes}}` | Rust: `if !overlaid.is_empty() { out.push(node_with_relationship(&min_parent, &relationship_name, overlaid.into_owned())); }`. The TS `() => overlaidRelatedChildNodes` lazy closure is replaced with eager materialization (Rust port pattern from Join).                                                                                                        |
| `flipped-join.ts:298-319` | iterator throw/return cleanup                                                                                                     | Not needed — Rust drops `Vec`s on scope exit.                                                                                                                                                                                                                                                                                                                     |
| `flipped-join.ts:322-344` | `*pushChild(change)` — switch on change type, dispatch to `pushChildChange(change, exists?)`                                      | `fn push_child(&mut self, change: Change) -> Vec<Change>` — switch on `change.change_type()` (Add, Remove → `push_child_change(change, false)`; Edit, Child → `push_child_change(change, true)`).                                                                                                                                                                 |
| `flipped-join.ts:346-425` | `*pushChildChange` — for each parent matching the child constraint, emit ChildChange or Add/Remove                                | Rust: `let constraint = build_join_constraint(&change.node().row, &child_key, &parent_key); let parents = if constraint.is_some() { parent.fetch(...) } else { vec![] }; for parent_node in parents { ... emit changes via output Vec<Change> }`. The `inprogressChildChange{,Position}` is set to the change + parent_row at top of loop body and cleared after. |
| `flipped-join.ts:373-388` | "exists" check via secondary child fetch when not exists                                                                          | Mirror as a function: `fn other_child_exists(child: &dyn Operator, parent_row: &Row, parent_key: &[String], child_key: &[String], excluding: &Row) -> bool` — fetches with constraint, returns true if any node not equal to `excluding` is found.                                                                                                                |
| `flipped-join.ts:389-420` | emit either `makeChildChange` or `makeAdd/RemoveChange` based on `exists`                                                         | Direct mapping to Rust `Change::Child { ... }` / `Change::Add { ... }` / `Change::Remove { ... }`.                                                                                                                                                                                                                                                                |
| `flipped-join.ts:427-505` | `*pushParent(change)` — fetch children for parent; if hasChild emit Add/Remove/Edit/Child with relationship                       | Rust: `fn push_parent(&mut self, change: Change) -> Vec<Change>` — compute `constraint = build_join_constraint(&parent_row, &parent_key, &child_key)`; `let related = child.fetch(...).is_empty() ? return vec![] : ...`; emit a `flip(node)` with `relationships: { rel_name: child_nodes }` analogous to TS.                                                    |
| `flipped-join.ts:484-499` | parent-edit must not change relationship — `assert(rowEqualsForCompoundKey(old, new, parent_key))`                                | `assert!(row_equals_for_compound_key(&old.row, &new.row, &parent_key))`.                                                                                                                                                                                                                                                                                          |

### UnionFanIn TS↔Rust intent map

| TS line                   | TS construct                                                                                                                                              | Rust intent                                                                                                                                                                                                                                                            |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---- | ------------------------------------------------------------------------------------ |
| `union-fan-in.ts:25-95`   | constructor — merge schemas across branches, assert sort/compareRows match                                                                                | `UnionFanInOperator::new(fan_out: Arc<UnionFanOutOperator>, inputs: Vec<Arc<dyn Operator>>)` — gather schemas, assert tableName/primaryKey/system/sort match; build merged schema.                                                                                     |
| `union-fan-in.ts:103-108` | `fetch(req)` — `mergeFetches(inputs.map(input => input.fetch(req)), compareRows)`                                                                         | `fn fetch(&self, req: &FetchRequest) -> Vec<Node>` — collect each branch's `Vec<Node>`, run `merge_dedup(&branch_nodes,                                                                                                                                                | l, r | (self.schema.compare_rows)(&l.row, &r.row))`.                                        |
| `union-fan-in.ts:222-296` | `mergeFetches` — k-way merge using `iterators.map(...)`, lookahead one node per branch, dedup by emitting first occurrence (skip equal)                   | Rust: `BinaryHeap<(Reverse<Key>, branch_idx, node_idx)>` priority queue. Pop min; if `Some(last) = last_yielded.as_ref()` and `compareRows(last, &min) == 0` skip; else emit and update last_yielded; push next from same branch.                                      |
| `union-fan-in.ts:114-120` | `*push(change, pusher)` — if `!fanOutPushStarted` dispatch to `pushInternalChange`, else accumulate                                                       | Rust: `fn push(&mut self, change: Change, pusher_idx: usize) -> Vec<Change>` — if `self.push_phase == Idle` then call `self.push_internal_change(change, pusher_idx)`; else `self.accumulated.push(change); vec![]`.                                                   |
| `union-fan-in.ts:142-181` | `*pushInternalChange` — for ChildChange forward; for Add/Remove only forward if no other branch has the row                                               | Rust: forward Child immediately. For Add/Remove iterate `inputs` skipping `pusher_idx`; build constraint over primary key from `change.node().row`; if any branch's `fetch(constraint)` returns non-empty, return `vec![]` (suppressed); else forward.                 |
| `union-fan-in.ts:183-189` | `fanOutStartedPushing()` — assert idle, flip phase                                                                                                        | `fn fan_out_started_pushing(&mut self) { assert!(matches!(self.push_phase, UnionPushPhase::Idle)); self.push_phase = UnionPushPhase::InFanOut; }`                                                                                                                      |
| `union-fan-in.ts:191-215` | `*fanOutDonePushing(fanOutChangeType)` — flush `accumulatedPushes` via `pushAccumulatedChanges` with `mergeRelationships` and `makeAddEmptyRelationships` | Rust: `fn fan_out_done_pushing(&mut self, change_type: ChangeType) -> Vec<Change> { let acc = mem::take(&mut self.accumulated); self.push_phase = UnionPushPhase::Idle; if acc.is_empty() { return vec![]; } push_accumulated_changes(&acc, change_type, &self.schema, | a, b | merge_relationships(a, b)) }`. The `pushAccumulatedChanges` helper has its own port. |

### `pushAccumulatedChanges` port (in `push-accumulated.ts`)

| TS function                                                                          | Rust intent                                                                                                                                                                                                                                                                                                  |
| ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --- | --- |
| `pushAccumulatedChanges(changes, output, source, fanOutChangeType, mergeFn, makeFn)` | Group accumulated changes by primary key. For each PK, fold changes via `mergeFn` (combine relationships across branches). For PKs that appear only as a `Filter` pass-through (not Add/Remove), wrap in `makeFn` (which adds empty relationships for non-emitting branches). Emit final merged change list. |
| `mergeRelationships(a, b)`                                                           | Combine `a.relationships` and `b.relationships` — keys are unique per branch (asserted in UnionFanIn constructor) so this is a simple `HashMap` extend.                                                                                                                                                      |
| `makeAddEmptyRelationships(schema)`                                                  | Returns a closure that wraps a Change to ensure all `schema.relationships` keys are present (some may be empty arrays from branches that didn't emit).                                                                                                                                                       |
| `identity`                                                                           | Used in `FanIn` (no relationship merging). Rust uses `                                                                                                                                                                                                                                                       | x   | x`. |

---

## Planner-AST shape: when does `flip:true` arrive?

### Source of truth

`packages/zql/src/planner/planner-builder.ts:240-280+` is where the planner
sets `flip` on a CorrelatedSubquery during cost-based planning.

[VERIFIED]: searched for `flip` in `packages/zql/src/planner/`:

- `planner-builder.ts:242` — `const manualFlip = condition.flip;` reads the
  user's explicit override (set in `apps/zbugs/shared/permissions.ts` for
  example).
- `planner-builder.ts:244-253` — branch logic:
  - `op === 'NOT EXISTS'` → `flippable = false` (NOT EXISTS can never be
    flipped).
  - `manualFlip === true` → `flippable = false; initialType = 'flipped'` —
    user-forced; planner doesn't reconsider.
  - `manualFlip === false` → `flippable = false; initialType = 'semi'`.
  - `manualFlip === undefined` (default) → `flippable = true` and the planner
    decides via cost (`planner-node.ts:26-39`).
- `planner-node.ts:26` — comment confirms a flipped join multiplies child
  cardinality by parent evaluation cost; cheap children + many-rows-per-child
  triggers flip.

The flag flows AST → builder → planner-decided AST → renderer → wire format
→ `zqlite-rs::ast_to_config`. By the time we receive an AST in
`ast_to_config.rs`, `flip:true` is final and authoritative.

### What this means for testing

Wave 5 needs **three planner-emission round-trip tests** (R-02 mitigation):

1. `apps/zbugs/shared/permissions.ts` — known to use `flip:true` explicitly.
   Drive its AST through `ast_to_config` and assert the new FlippedJoin path
   is hit.
2. A synthetic ZQL query whose cost model naturally triggers flip (small
   parent, large child with selective predicate). Assert the planner emits
   `flip:true` and Rust executes it.
3. A negative test: ZQL query with `manualFlip: false` — assert the regular
   Exists path is taken.

[ASSUMED]: that `apps/zbugs/shared/permissions.ts` still uses `flip:true`. Verify
during Wave 5 implementation; if no longer applicable, fall back to a synthetic
hand-rolled AST fixture.

---

## Generator-to-synchronous translation

### TS uses three generator idioms

1. **Cooperative yield for I/O readiness.** `yield 'yield'` in TS interleaves
   with the framework's outer scheduler — it's a backpressure signal. Rust
   doesn't need this because all `fetch` calls are synchronous (TableSource
   reads from Connection synchronously per Phase 21).
2. **`yield* output.push(change, this)` — push and wait.** TS pushes a change
   downstream and may suspend; Rust's `Operator::push` returns `Vec<Change>`
   eagerly. The Rust port invokes the operator's downstream by appending to a
   shared output buffer — already the pattern in JoinOperator/ExistsOperator.
3. **`yield* generateWithOverlayNoYield(...)` — overlay-aware generation.**
   Used in `flipped-join.ts:277-283`. Rust replaces with a pure function
   `generate_with_overlay` returning `Vec<Node>`.

### Translation rules (apply throughout the family)

- `function*` → `fn` returning `Vec<X>`.
- `yield 'yield'` → drop entirely (no async needed).
- `yield val` → `out.push(val)`.
- `yield* sub_generator()` → `out.extend(sub_generator())` or fold the
  sub-generator's body inline.
- `Iterator<Node | 'yield'>` → `Vec<Node>` (the 'yield' sentinel is dropped
  during collection).

This is the same pattern Phase 11 (Join) and Phase 12 (Exists) already
established. No new infrastructure needed.

---

## Distributive expansion in `applyFilterWithFlips`

### TS `builder.ts:386-449`

For an `OR` condition with mixed flipped+non-flipped subqueries:

```
OR(simple, EXISTS_a flip=false, EXISTS_b flip=true)
```

TS partitions:

- `withFlipped = [EXISTS_b]`
- `withoutFlipped = [simple, EXISTS_a]`

Then constructs:

```
input -> UnionFanOut -> [
  Filter(applyOr(withoutFlipped))      // emits rows for non-flipped branches
  applyFilterWithFlips(EXISTS_b)       // emits flipped-join for flipped branches
] -> UnionFanIn(branches)
```

### Rust port in `ast_to_config.rs::append_condition_configs_with_flips`

```rust
fn append_condition_configs_with_flips(
    schema: &mut SchemaCache,
    configs: &mut Vec<OperatorConfig>,
    cond: &Condition,
    primary_key: &[String],
) -> Result<(), String> {
    if !condition_has_flip(cond) {
        // Fast path — no flips, use the existing condition pipeline.
        return append_condition_configs(schema, configs, cond, primary_key);
    }
    match cond {
        Condition::Or { conditions } => {
            let (with_flipped, without_flipped) = partition_branches(conditions, condition_has_flip);
            let mut branches: Vec<Vec<OperatorConfig>> = vec![];
            if !without_flipped.is_empty() {
                let mut branch = vec![];
                append_condition_configs(schema, &mut branch, &Condition::Or { conditions: without_flipped }, primary_key)?;
                branches.push(branch);
            }
            for cond in with_flipped {
                let mut branch = vec![];
                append_condition_configs_with_flips(schema, &mut branch, &cond, primary_key)?;
                branches.push(branch);
            }
            configs.push(OperatorConfig::UnionFanOut {});
            configs.push(OperatorConfig::UnionFanIn { branches });
        }
        Condition::And { conditions } => {
            let (with_flipped, without_flipped) = partition_branches(conditions, condition_has_flip);
            if !without_flipped.is_empty() {
                append_condition_configs(schema, configs, &Condition::And { conditions: without_flipped }, primary_key)?;
            }
            for cond in with_flipped {
                append_condition_configs_with_flips(schema, configs, &cond, primary_key)?;
            }
        }
        Condition::CorrelatedSubquery { related, op, flip, .. } if flip == &Some(true) => {
            assert!(op != "NOT EXISTS", "NOT EXISTS cannot be flipped — planner should reject");
            let child_pk = schema.get_primary_key(&related.subquery.table)?;
            let child_partition = Some(related.correlation.child_field.clone());
            let mut child_configs = ast_to_operator_configs(schema, &related.subquery, &child_pk, child_partition)?;
            // No apply_exists_limit — flipped joins don't downgrade child Take.
            configs.push(OperatorConfig::FlippedJoin {
                relationship_name: relationship_name(related),
                parent_key: related.correlation.parent_field.clone(),
                child_key: related.correlation.child_field.clone(),
                child: child_configs,
                hidden: related.hidden.unwrap_or(false),
                system: related.system.clone().unwrap_or_else(|| "client".into()),
            });
        }
        _ => unreachable!("condition_has_flip lied about flip presence"),
    }
    Ok(())
}
```

`condition_has_flip(cond)` is a recursive walker analogous to TS
`conditionIncludesFlippedSubqueryAtAnyLevel`.

---

## Test fixture porting strategy

### What to port

- **flipped-join.fetch.test.ts** — pick representative shapes:
  - empty parent + non-empty child
  - empty child + non-empty parent
  - 1:1 join match
  - 1:N join match (one parent, multiple children)
  - N:1 (multiple parents share a child)
  - constraint on parent → translated to child
  - constraint on parent that does NOT translate (different key columns) → empty fetch
  - reverse: true with order-by
  - compound key (2-part parent_key + 2-part child_key)
- **flipped-join.push.test.ts** (10223 LOC — sample only):
  - push_parent Add: with related, without related (no emit)
  - push_parent Remove: with related, without related
  - push_parent Edit: keys unchanged (assert), keys changed (panic)
  - push_parent Child: forwarded with flip
  - push_child Add: where parent matches none, parent matches one, parent matches many
  - push_child Remove: same matrix, with overlay-undo verification
  - push_child Edit: keys unchanged, keys changed (panic)
  - push_child Child: forwarded as ChildChange to all matching parents
  - Multi-step interleaving: push_parent, push_child, push_parent — verify state isolation
- **flipped-join.sibling.test.ts** — 2-3 representative cases (sibling
  relationships are "two FlippedJoins on the same parent").

### What NOT to port

- TS-specific generator scaffolding (mock `Input` adapters, `runJoinTest`
  harness).
- `'yield'` sentinel cases (don't apply in synchronous Rust).
- TS-specific schema-merge edge cases that the Rust port handles via
  `merge_schemas` validation upfront.

### Coverage targets per Wave (from CONTEXT D-12)

- Wave 1 (Fan{In,Out}): 20 tests.
- Wave 2 (Union\*): 30 tests (UFI 20 + UFO 10).
- Wave 3 (FlippedJoin): 50 tests.
- Wave 4 (ast_to_config): 15 tests.
- Wave 5 (diff parity): 5 new shape tests + 2 allow-list removals = 7 deltas.

**Total estimated test count: ≥120 new tests.**

---

## Assumptions Log

- [ASSUMED] `apps/zbugs/shared/permissions.ts` still uses `flip:true` for
  authoritative usage. To be re-verified during Wave 5; fallback is a synthetic
  hand-rolled AST fixture.
- [ASSUMED] No CI integration changes needed — Phase 34's `fuzz-check:gate`
  picks up allow-list shrinkage automatically.
- [ASSUMED] Phase 33 benchmark thresholds (TTFB 1.5×, MemPeak 4×) apply to the
  new FlippedJoin path at-most as a baseline. Since this is a NEW path, latency
  is measured fresh; we do not compare against a no-flip baseline. Per CONTEXT
  D-14 #5.

---

## References

- `.planning/IVM-PORT-AUDIT-DEEP.md` §B7 (verified open-risk; flip silently
  treated as regular Exists).
- `PARITY_STATUS.md` (allow-list `fuzz_00132`/`fuzz_00133`; deferred pattern
  `B7-flipped-join`).
- `packages/zql/src/ivm/flipped-join.ts` — primary spec.
- `packages/zql/src/ivm/union-fan-in.ts`, `union-fan-out.ts`, `fan-in.ts`,
  `fan-out.ts` — supporting operators.
- `packages/zql/src/ivm/push-accumulated.ts` — `pushAccumulatedChanges` helper.
- `packages/zql/src/builder/builder.ts:376-482` — `applyFilterWithFlips`.
- `packages/zql/src/planner/planner-builder.ts:240-280` — flip emission.
- `packages/zqlite-rs/src/ast_to_config.rs:401, 456, 589` — current
  silent-treatment of `flip:true`.
- `packages/zero-ivm-rs/src/pipeline.rs:30-81` — current OperatorConfig.
- Phase 11 / Phase 12 plans — established patterns for synchronous Rust port
  of TS generator operators.
