---
phase: 20
plan: '03'
title: 'Take and Exists Operators'
wave: 2
depends_on: ['01']
files_modified:
  - packages/zero-ivm-rs/src/take_op.rs
  - packages/zero-ivm-rs/src/exists_op.rs
  - packages/zero-ivm-rs/src/lib.rs
requirements_addressed: [OPR-02]
autonomous: true
---

# Plan 03: Take and Exists Operators

<objective>
Implement TakeOperator (LIMIT with stateful bound tracking) and ExistsOperator (child existence filtering) wrapping existing Rust helpers from take_state.rs and exists.rs.
</objective>

<threat_model>
| Threat | Severity | Mitigation |
|--------|----------|------------|
| Take bound state diverges from TS across push cycles | medium | Wrap existing TakeState/RustTakeState logic which is already tested; unit tests verify bound updates |
| Exists size counter off-by-one on add/remove transitions | medium | Port exact transition logic from exists.rs decide_exists_push; test 0→1 and 1→0 transitions |
| Memory leak in Take state HashMap | low | States keyed by correlation key; cleanup on Remove; bounded by active query count |
</threat_model>

<tasks>

<task id="20-03-01">
<title>Implement TakeOperator</title>
<read_first>
- packages/zero-ivm-rs/src/take_state.rs
- packages/zero-ivm-rs/src/operator.rs
- packages/zero-ivm-rs/src/types.rs
- packages/zql/src/ivm/take.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/take_op.rs`:

```rust
use std::collections::HashMap;
use crate::operator::Operator;
use crate::types::{Change, FetchRequest, Node, Row, SortSpec, compare_rows};

struct TakeState {
    size: usize,
    bound: Option<Row>,
}

pub struct TakeOperator {
    input: Box<dyn Operator>,
    limit: usize,
    sort: Vec<SortSpec>,
    partition_key: Option<Vec<String>>,
    states: HashMap<String, TakeState>,
}
```

Implement `Operator::fetch()`:

1. If a bound exists for the partition key, set `req.start = Some(Start { row: bound, basis: "at" })` to avoid re-scanning
2. Call `self.input.fetch(modified_req)`
3. Take at most `limit` nodes from results
4. Update state: set size = count of taken nodes, bound = last taken node's row (or None if empty)
5. Return taken nodes

Implement `Operator::push()`:
For each change, determine partition key string from row. Then:

- **Add**: If size < limit, propagate Add, increment size, update bound. If size == limit, compare new row with bound: if row is before bound (in sort order), emit Add for new + Remove for displaced bound row, update bound to new last row.
- **Remove**: If row is within window (before or at bound), propagate Remove, decrement size. If there's a row just after the old bound, fetch it and emit Add to fill the gap, update bound.
- **Edit**: If both old and new are within window and sort position unchanged, propagate Edit. Otherwise split into Remove + Add logic.
- **Child**: If row is within window, propagate.

Use `compare_rows()` from types.rs for all sort comparisons. Reuse the comparison logic from existing `take_state.rs` `compare_row_values()` method.

Add unit tests:

- Take limits fetch to N rows and tracks bound
- Take push Add within limit propagates
- Take push Add at limit displaces bound row
- Take push Remove decrements and may fetch replacement
  </action>
  <acceptance_criteria>
- `packages/zero-ivm-rs/src/take_op.rs` contains `pub struct TakeOperator`
- `take_op.rs` contains `impl Operator for TakeOperator`
- `take_op.rs` contains `fn fetch(` and `fn push(`
- `take_op.rs` contains `#[cfg(test)]` with at least 4 test functions
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-03-02">
<title>Implement ExistsOperator</title>
<read_first>
- packages/zero-ivm-rs/src/exists.rs
- packages/zero-ivm-rs/src/operator.rs
- packages/zero-ivm-rs/src/types.rs
- packages/zql/src/ivm/exists.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/exists_op.rs`:

```rust
use std::collections::HashMap;
use crate::operator::Operator;
use crate::types::{Change, ChildData, FetchRequest, Node};

pub struct ExistsOperator {
    input: Box<dyn Operator>,
    child: Box<dyn Operator>,
    relationship_name: String,
    not_exists: bool,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    /// Tracks count of child rows per parent key
    parent_sizes: HashMap<String, usize>,
}
```

Implement `Operator::fetch()`:

1. Call `self.input.fetch(req)` to get parent nodes
2. For each parent node, fetch children via `self.child.fetch()` with join constraint
3. Count child rows. If `not_exists`: include parent only if count == 0. If `exists`: include only if count > 0.
4. Track count in `parent_sizes`
5. Return filtered parent nodes

Implement `Operator::push()`:
Use the transition logic from existing `decide_exists_push()` in exists.rs:

- **Parent Add/Remove/Edit**: Check existence condition, propagate if passes filter (exists → has children, not_exists → no children)
- **Child change** (Change::Child where child.relationship_name matches):
  - Child Add: increment parent size. If 0→1 transition: emit Add (exists) or Remove (not_exists)
  - Child Remove: decrement parent size. If 1→0 transition: emit Remove (exists) or Add (not_exists)
  - Child Edit/Child: propagate if existence condition still holds

Add unit tests:

- Exists fetch filters parents without children
- Exists push child Add triggers 0→1 transition (parent appears)
- Not-exists push child Add triggers 0→1 transition (parent disappears)
- Exists push child Remove triggers 1→0 transition
  </action>
  <acceptance_criteria>
- `packages/zero-ivm-rs/src/exists_op.rs` contains `pub struct ExistsOperator`
- `exists_op.rs` contains `impl Operator for ExistsOperator`
- `exists_op.rs` contains `fn fetch(` and `fn push(`
- `exists_op.rs` contains `parent_sizes: HashMap<String, usize>`
- `exists_op.rs` contains `#[cfg(test)]` with at least 4 test functions
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-03-03">
<title>Register take_op and exists_op modules in lib.rs</title>
<read_first>
- packages/zero-ivm-rs/src/lib.rs
</read_first>
<action>
Add to `packages/zero-ivm-rs/src/lib.rs`:
```rust
pub mod take_op;
pub mod exists_op;
```
</action>
<acceptance_criteria>
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod take_op;`
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod exists_op;`
- `cargo test` in `packages/zero-ivm-rs/` passes
</acceptance_criteria>
</task>

</tasks>

<verification>
```bash
cd packages/zero-ivm-rs && cargo test take_op
cd packages/zero-ivm-rs && cargo test exists_op
cd packages/zero-ivm-rs && cargo test
```
</verification>

<must_haves>

- TakeOperator enforces LIMIT with stateful bound tracking across fetch and push
- TakeOperator handles displacement (Add at limit removes boundary row)
- ExistsOperator filters parents by child existence count
- ExistsOperator handles 0→1 and 1→0 transitions correctly for both EXISTS and NOT EXISTS
- Both operators implement Send trait
  </must_haves>
