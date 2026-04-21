---
phase: 20
plan: '02'
title: 'Join Operator'
wave: 2
depends_on: ['01']
files_modified:
  - packages/zero-ivm-rs/src/join_op.rs
  - packages/zero-ivm-rs/src/lib.rs
requirements_addressed: [OPR-02]
autonomous: true
---

# Plan 02: Join Operator

<objective>
Implement the JoinOperator — the most complex IVM operator — wrapping existing join.rs helpers inside the Operator trait. Handles parent-child relationship attachment during fetch and bidirectional change propagation during push.
</objective>

<threat_model>
| Threat | Severity | Mitigation |
|--------|----------|------------|
| Child relationship ordering diverges from TS | medium | Use same join key matching logic from existing join.rs; unit tests verify order |
| Overlay state corruption during self-join push | medium | Per-push overlay HashMap cleared after each push() call; no cross-call leakage |
| Stack overflow on deeply nested joins | low | Practical depth is 3-5 levels; recursive calls bounded by query structure |
</threat_model>

<tasks>

<task id="20-02-01">
<title>Implement JoinOperator with fetch</title>
<read_first>
- packages/zero-ivm-rs/src/join.rs
- packages/zero-ivm-rs/src/operator.rs
- packages/zero-ivm-rs/src/types.rs
- packages/zql/src/ivm/join.ts
- packages/zql/src/ivm/join-utils.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/join_op.rs`:

```rust
use std::collections::HashMap;
use crate::filter::compare_values;
use crate::operator::Operator;
use crate::types::{Change, ChildData, Constraint, FetchRequest, Node, Row};

pub struct JoinOperator {
    parent: Box<dyn Operator>,
    child: Box<dyn Operator>,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
}
```

Implement `Operator::fetch()`:

1. Call `self.parent.fetch(req)` to get parent nodes
2. For each parent node, build a child constraint from parent row using `parent_key` → `child_key` mapping (reuse `build_join_constraint` logic from join.rs)
3. Call `self.child.fetch(&FetchRequest { constraint: Some(child_constraint), ..Default::default() })` to get child nodes
4. Attach child nodes as `node.relationships[relationship_name] = child_nodes`
5. Return parent nodes with relationships attached

If any parent key column is null, skip child fetch (no match possible) and attach empty relationship.
</action>
<acceptance_criteria>

- `packages/zero-ivm-rs/src/join_op.rs` contains `pub struct JoinOperator`
- `join_op.rs` contains `fn fetch(` that calls `self.parent.fetch` and `self.child.fetch`
- `join_op.rs` contains logic to build child constraint from parent key columns
- `cargo check` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-02-02">
<title>Implement JoinOperator push (parent and child paths)</title>
<read_first>
- packages/zero-ivm-rs/src/join_op.rs
- packages/zql/src/ivm/join.ts
- packages/zql/src/ivm/join-utils.ts
</read_first>
<action>
Add push() implementation to JoinOperator:

**Parent change path** (Add/Remove/Edit on parent):

1. For Add: fetch children for the new parent row, attach relationships, emit Add with relationships
2. For Remove: fetch children for removed parent row, attach relationships, emit Remove with relationships
3. For Edit: if join key columns changed, emit Remove(old) + Add(new) each with fetched children. If join key unchanged, emit Edit with same children.

**Child change path** (Child change type):

1. Extract the child change from `Change::Child { node, child }`
2. The `node` is the parent row. Build a ChildChange that wraps the child's change with `relationship_name`
3. Emit `Change::Child { node: parent_with_relationships, child: ChildData { relationship_name, change } }`

**Overlay support** (structural preparation for Phase 24):

- During push, maintain a `HashMap<String, Vec<Node>>` overlay keyed by serialized join key
- When fetching children during push (for parent Add/Remove), check overlay first, then delegate to child.fetch()
- Clear overlay at end of push()

Add unit tests:

- Join fetch attaches child relationships to parent nodes
- Join push propagates parent Add with fetched children
- Join push propagates child change as ChildChange on parent
- Join push with null key column produces no child fetch
  </action>
  <acceptance_criteria>
- `join_op.rs` contains `fn push(` with match arms for `Change::Add`, `Change::Remove`, `Change::Edit`, `Change::Child`
- `join_op.rs` contains `#[cfg(test)]` with at least 4 test functions
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-02-03">
<title>Register join_op module in lib.rs</title>
<read_first>
- packages/zero-ivm-rs/src/lib.rs
</read_first>
<action>
Add `pub mod join_op;` to `packages/zero-ivm-rs/src/lib.rs`.
</action>
<acceptance_criteria>
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod join_op;`
- `cargo test` in `packages/zero-ivm-rs/` passes
</acceptance_criteria>
</task>

</tasks>

<verification>
```bash
cd packages/zero-ivm-rs && cargo test join_op
cd packages/zero-ivm-rs && cargo test
```
</verification>

<must_haves>

- JoinOperator.fetch() attaches child relationships via child.fetch() per parent row
- JoinOperator.push() handles parent changes (Add/Remove/Edit) with child re-fetch
- JoinOperator.push() handles child changes as ChildChange propagation
- Null join key columns produce empty relationships (no child fetch)
- Overlay HashMap structure present for self-join correctness
  </must_haves>
