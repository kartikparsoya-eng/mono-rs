---
phase: 20
plan: '01'
title: 'Core Types, Operator Trait, Filter/Skip/Cap Operators'
wave: 1
depends_on: []
files_modified:
  - packages/zero-ivm-rs/src/lib.rs
  - packages/zero-ivm-rs/src/types.rs
  - packages/zero-ivm-rs/src/operator.rs
  - packages/zero-ivm-rs/src/filter_op.rs
  - packages/zero-ivm-rs/src/skip_op.rs
  - packages/zero-ivm-rs/src/cap_op.rs
requirements_addressed: [OPR-01]
autonomous: true
---

# Plan 01: Core Types, Operator Trait, Filter/Skip/Cap Operators

<objective>
Define the unified Operator trait and core IVM types (Node, Change, FetchRequest) in Rust, then implement the three simpler operators (Filter, Skip, Cap) as trait implementations wrapping existing Rust helpers.
</objective>

<threat_model>
| Threat | Severity | Mitigation |
|--------|----------|------------|
| Type mismatch between Rust and TS representations | medium | Use serde_json::Value for Row, matching existing pattern in filter.rs/join.rs |
| Filter predicate evaluation diverges from TS | low | Wrap existing evaluate_filter() which has 139 cargo tests |
| Unsafe memory access via trait objects | low | All trait objects are Box<dyn Operator>, no unsafe code |
</threat_model>

<tasks>

<task id="20-01-01">
<title>Create core IVM types module</title>
<read_first>
- packages/zero-ivm-rs/src/filter.rs
- packages/zql/src/ivm/data.ts
- packages/zql/src/ivm/change.ts
- packages/zql/src/ivm/operator.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/types.rs` with these types:

```rust
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

pub type Row = serde_json::Map<String, serde_json::Value>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub row: Row,
    pub relationships: HashMap<String, Vec<Node>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChangeType {
    Add,    // 0
    Remove, // 1
    Child,  // 2
    Edit,   // 3
}

#[derive(Clone, Debug)]
pub struct ChildData {
    pub relationship_name: String,
    pub change: Box<Change>,
}

#[derive(Clone, Debug)]
pub enum Change {
    Add(Node),
    Remove(Node),
    Child { node: Node, child: ChildData },
    Edit { node: Node, old_node: Node },
}

impl Change {
    pub fn node(&self) -> &Node { ... }
    pub fn change_type(&self) -> ChangeType { ... }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Constraint {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Start {
    pub row: Row,
    pub basis: String, // "at" or "after"
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FetchRequest {
    pub constraint: Option<Constraint>,
    pub start: Option<Start>,
    pub reverse: bool,
}

#[derive(Clone, Debug)]
pub struct SortSpec {
    pub field: String,
    pub direction: SortDirection,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SortDirection {
    Asc,
    Desc,
}
```

Implement `compare_rows(a: &Row, b: &Row, sort: &[SortSpec]) -> std::cmp::Ordering` using the existing `compare_values` logic from filter.rs. Implement serde Serialize/Deserialize for Change (custom impl mapping to TS tuple format).
</action>
<acceptance_criteria>

- `packages/zero-ivm-rs/src/types.rs` contains `pub type Row = serde_json::Map<String, serde_json::Value>`
- `types.rs` contains `pub struct Node`
- `types.rs` contains `pub enum Change` with variants `Add`, `Remove`, `Child`, `Edit`
- `types.rs` contains `pub struct FetchRequest`
- `types.rs` contains `pub fn compare_rows(`
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-01-02">
<title>Define Operator trait</title>
<read_first>
- packages/zero-ivm-rs/src/types.rs
- packages/zql/src/ivm/operator.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/operator.rs`:

```rust
use crate::types::{Change, FetchRequest, Node};

pub trait Operator: Send {
    /// Fetch data matching the request. Returns nodes in sort order.
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node>;

    /// Push an incremental change through this operator.
    /// Returns output changes to propagate downstream.
    fn push(&mut self, change: Change) -> Vec<Change>;

    /// Get the operator type name (for debugging/logging).
    fn op_type(&self) -> &'static str;
}
```

The trait is `Send` so it can be moved to Rayon threads in Phase 22/24. Uses `&mut self` since operators own mutable state (per D-06). No `Env` dependency.
</action>
<acceptance_criteria>

- `packages/zero-ivm-rs/src/operator.rs` contains `pub trait Operator: Send`
- `operator.rs` contains `fn fetch(&mut self, req: &FetchRequest) -> Vec<Node>`
- `operator.rs` contains `fn push(&mut self, change: Change) -> Vec<Change>`
- `cargo check` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-01-03">
<title>Implement FilterOperator</title>
<read_first>
- packages/zero-ivm-rs/src/filter.rs
- packages/zero-ivm-rs/src/operator.rs
- packages/zero-ivm-rs/src/types.rs
- packages/zql/src/ivm/filter.ts
- packages/zql/src/ivm/filter-push.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/filter_op.rs`:

```rust
use crate::filter::{Predicate, evaluate_filter};
use crate::operator::Operator;
use crate::types::{Change, FetchRequest, Node};

pub struct FilterOperator {
    input: Box<dyn Operator>,
    predicate: Predicate,
}

impl FilterOperator {
    pub fn new(input: Box<dyn Operator>, predicate: Predicate) -> Self {
        Self { input, predicate }
    }

    fn matches_row(&self, row: &serde_json::Map<String, serde_json::Value>) -> bool {
        // Convert Row to serde_json::Value for evaluate_filter
        evaluate_filter(&serde_json::Value::Object(row.clone()), &self.predicate)
    }
}
```

Implement `Operator` for `FilterOperator`:

- `fetch()`: call `self.input.fetch(req)`, filter nodes where `matches_row(node.row)` is true. Also filter child relationships recursively within each node.
- `push()`: For Add/Remove changes, check predicate on node.row — propagate if matches. For Edit changes, check both old and new node: if old matches but new doesn't → convert to Remove; if old doesn't match but new does → convert to Add; if both match → propagate Edit; if neither → skip. For Child changes, check predicate on node.row and propagate if matches.

Add unit tests: filter with Eq predicate on fetch results, filter push with Add/Remove/Edit changes.
</action>
<acceptance_criteria>

- `packages/zero-ivm-rs/src/filter_op.rs` contains `pub struct FilterOperator`
- `filter_op.rs` contains `impl Operator for FilterOperator`
- `filter_op.rs` contains `fn fetch(` and `fn push(`
- `filter_op.rs` contains `#[cfg(test)]` with at least 3 test functions
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-01-04">
<title>Implement SkipOperator</title>
<read_first>
- packages/zero-ivm-rs/src/operator.rs
- packages/zero-ivm-rs/src/types.rs
- packages/zql/src/ivm/skip.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/skip_op.rs`:

```rust
use crate::operator::Operator;
use crate::types::{Change, FetchRequest, Node, Row, SortSpec, Start, compare_rows};

pub struct Bound {
    pub row: Row,
    pub exclusive: bool,
}

pub struct SkipOperator {
    input: Box<dyn Operator>,
    bound: Bound,
    sort: Vec<SortSpec>,
}
```

Implement `Operator` for `SkipOperator`:

- `fetch()`: Compute effective start — if no existing start or bound is after existing start, use bound as start. If `req.reverse`, filter out nodes before bound. If forward, pass modified request with bound as start to input.
- `push()`: For each change, check if the change's node row is at or after the bound (using `compare_rows`). Only propagate changes for rows that should be visible (at/after bound). For Edit changes where old is visible but new is not (or vice versa), convert to Remove/Add.

The bound comparison: `compare_rows(node.row, bound.row, sort)` must be > 0 (if exclusive) or >= 0 (if inclusive).

Add unit tests: skip filters out rows before bound in fetch, skip converts push changes at boundary.
</action>
<acceptance_criteria>

- `packages/zero-ivm-rs/src/skip_op.rs` contains `pub struct SkipOperator`
- `skip_op.rs` contains `impl Operator for SkipOperator`
- `skip_op.rs` contains `#[cfg(test)]` with at least 2 test functions
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-01-05">
<title>Implement CapOperator</title>
<read_first>
- packages/zero-ivm-rs/src/operator.rs
- packages/zero-ivm-rs/src/types.rs
- packages/zql/src/ivm/cap.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/cap_op.rs`:

```rust
use std::collections::{HashMap, HashSet};
use crate::operator::Operator;
use crate::types::{Change, FetchRequest, Node, Row};

struct CapState {
    size: usize,
    pks: HashSet<String>,
}

pub struct CapOperator {
    input: Box<dyn Operator>,
    limit: usize,
    primary_key: Vec<String>,
    partition_key: Option<Vec<String>>,
    states: HashMap<String, CapState>,
}
```

Implement `Operator` for `CapOperator`:

- `fetch()`: Call `self.input.fetch(req)`, take at most `limit` nodes per partition key (or globally if no partition key). Track PKs in state.
- `push()`: For Add changes — if current size < limit, propagate and increment size, add PK to set. If at limit, skip. For Remove changes — if PK is in set, propagate and decrement, remove PK. If PK not in set, skip. For Edit — propagate if PK in set. For Child — propagate if PK in set.

Partition key: if set, compute partition key string from row values and use per-partition state. If not set, use single global state (key = "").

Add unit tests: cap limits fetch results, cap tracks add/remove push changes.
</action>
<acceptance_criteria>

- `packages/zero-ivm-rs/src/cap_op.rs` contains `pub struct CapOperator`
- `cap_op.rs` contains `impl Operator for CapOperator`
- `cap_op.rs` contains `#[cfg(test)]` with at least 2 test functions
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-01-06">
<title>Register new modules in lib.rs</title>
<read_first>
- packages/zero-ivm-rs/src/lib.rs
</read_first>
<action>
Update `packages/zero-ivm-rs/src/lib.rs` to add the new modules:

```rust
#![deny(clippy::all)]

pub mod filter;
pub mod join;
pub mod storage;
pub mod take_state;
pub mod exists;
pub mod types;
pub mod operator;
pub mod filter_op;
pub mod skip_op;
pub mod cap_op;
```

</action>
<acceptance_criteria>
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod types;`
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod operator;`
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod filter_op;`
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod skip_op;`
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod cap_op;`
- `cargo test` in `packages/zero-ivm-rs/` passes
</acceptance_criteria>
</task>

</tasks>

<verification>
```bash
cd packages/zero-ivm-rs && cargo test
cd packages/zero-ivm-rs && cargo clippy -- -D warnings
```
</verification>

<must_haves>

- Operator trait defined with fetch() and push() methods
- Core types (Node, Change, FetchRequest, Row) defined matching TS semantics
- FilterOperator wraps existing evaluate_filter() from filter.rs
- SkipOperator modifies fetch bounds and filters push changes
- CapOperator enforces count-based limits with PK tracking
- All operators implement Send (no Env dependency)
- All existing 139 cargo tests continue to pass
  </must_haves>
