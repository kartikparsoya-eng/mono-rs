//! TS spec: `packages/zql/src/ivm/union-fan-in.ts` (296 LOC) — dedup-by-PK
//! + reorder-by-sort merge.
//!
//! See `.planning/phases/36-flipped-join-family/36-RESEARCH.md`
//! §"UnionFanIn TS↔Rust intent map" for the line-by-line port plan.
//!
//! # TS↔Rust port mapping
//!
//! | TS line                       | TS construct                                                                | Rust intent |
//! | ----------------------------- | --------------------------------------------------------------------------- | ----------- |
//! | `union-fan-in.ts:25-95`       | constructor — merge schemas across branches, assert sort/compareRows match | `UnionFanInOperator::new(fan_out, inputs)` — gather schemas, assert tableName/primaryKey/system/sort match; build merged schema. |
//! | `union-fan-in.ts:103-108`     | `fetch(req)` — `mergeFetches(inputs.map(input.fetch(req)), compareRows)`   | `fn fetch(&self, req: &FetchRequest) -> Vec<Node>` — collect each branch's Vec<Node>, run merge_dedup. |
//! | `union-fan-in.ts:222-296`     | `mergeFetches` — k-way merge with lookahead, dedup                          | `BinaryHeap<(Reverse<Key>, branch_idx, node_idx)>`. Pop min; if last_yielded compares equal skip; else emit & advance. |
//! | `union-fan-in.ts:114-120`     | `*push(change, pusher)` — Idle → push_internal_change; else accumulate     | `fn push(&mut self, change: Change, pusher_idx: usize) -> Vec<Change>`. |
//! | `union-fan-in.ts:142-181`     | `*pushInternalChange` — forward Child immediately; suppress Add/Remove if other branch has row | Iterate branches except pusher; build PK constraint; if any branch's fetch returns non-empty, suppress; else forward. |
//! | `union-fan-in.ts:183-189`     | `fanOutStartedPushing()` — assert idle, flip                                | `phase = InFanOut`. |
//! | `union-fan-in.ts:191-215`     | `*fanOutDonePushing(fanOutChangeType)` — flush via pushAccumulatedChanges  | `let acc = mem::take(...); push_accumulated_changes(acc, change_type, schema, mergeRelationships, makeAddEmptyRelationships)`. |
//!
//! # `pushAccumulatedChanges` port (TS `push-accumulated.ts`)
//!
//! - Group accumulated changes by primary key (actually by ChangeType per TS).
//! - For each candidate, fold via `merge_fn` (relationships union).
//! - Wrap result via `make_fn` (pad missing relationships with empty arrays).
//! - Switch on `fanOutChangeType` to enforce invariants:
//!   - REMOVE → all branches must yield REMOVE; emit one merged remove.
//!   - ADD → all branches must yield ADD; emit one merged add.
//!   - EDIT → can yield ADD, REMOVE, EDIT; if EDIT present it supersedes; else
//!     ADD+REMOVE → reconstruct EDIT; else single ADD or REMOVE.
//!   - CHILD → any branch may convert to ADD/REMOVE; CHILD precedence; else
//!     single ADD or REMOVE.
//!
//! # Generator-to-synchronous translation
//!
//! TS uses cooperative `yield* output.push(...)` to interleave with FanOut's
//! per-output iteration. Rust collects all branch outputs upfront (Vec<Change>)
//! and applies dedup/merge synchronously. Validated by R-01 spike: the merge
//! is per-PK and order-insensitive, so eager collection is semantically
//! identical.
//!
//! # Implementation status
//!
//! Wave 0 stub — struct + UnionPushPhase enum + new() skeleton.
//! Wave 2 implements `Operator` trait + helper module + ≥20 unit tests.

use std::sync::{Arc, Mutex};

use crate::operator::Operator;
use crate::types::ChangeType;

/// Push-protocol phase tracker for UnionFanIn.
///
/// TS spec: `union-fan-in.ts:183-215` (fanOutStartedPushing/fanOutDonePushing).
///
/// While `Idle`, push() dispatches to push_internal_change (forwarding logic).
/// While `InFanOut`, push() accumulates without forwarding; flush happens at
/// fanOutDonePushing.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub enum UnionPushPhase {
    /// Default state. External pushes (e.g. from a flip-join Child) take the
    /// internal-change path. TS line 184.
    Idle,
    /// Set by `fanOutStartedPushing`; cleared by `fanOutDonePushing`.
    /// All pushes accumulate. The carried `ChangeType` is the original
    /// fan-out change type, used to validate invariants at flush time.
    InFanOut(ChangeType),
}

/// Accumulated push entry — tracks the change and which branch (input index)
/// produced it. The pusher index is required for the internal-change
/// dedup logic (suppressing duplicate Add/Remove from sibling branches).
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct AccumulatedPush {
    pub change: crate::types::Change,
    pub pusher_idx: usize,
}

/// UnionFanIn operator — k-way dedup-by-PK merge of multiple branches.
///
/// TS spec: `packages/zql/src/ivm/union-fan-in.ts` lines 25-220.
///
/// Used in `applyFilterWithFlips` to merge OR-branches where at least one
/// branch is a flipped EXISTS.
///
/// Wave 0 stub. Wave 2 implements `Operator` trait + ≥20 unit tests.
#[allow(dead_code)]
pub struct UnionFanInOperator {
    /// TS line 27: branches feeding into this operator. Each input is the
    /// terminal Operator of one branch from the paired UnionFanOut.
    pub(crate) inputs: Vec<Arc<Mutex<dyn Operator>>>,
    /// TS line 28: schema gathered from fan-out + assertions over branches.
    /// In Rust this is the table-source schema (sort spec + primary key).
    pub(crate) primary_key: Vec<String>,
    /// TS line 30: protocol phase tracker.
    pub(crate) phase: UnionPushPhase,
    /// TS line 31: pushes accumulated while phase is InFanOut.
    pub(crate) accumulated: Vec<AccumulatedPush>,
}

impl UnionFanInOperator {
    /// Constructor stub.
    ///
    /// TS spec: `union-fan-in.ts:25-95`.
    ///
    /// Wave 2 will add schema-mismatch assertions; for Wave 0 the constructor
    /// exists to pin the field layout.
    #[allow(dead_code)]
    pub fn new(inputs: Vec<Arc<Mutex<dyn Operator>>>, primary_key: Vec<String>) -> Self {
        Self {
            inputs,
            primary_key,
            phase: UnionPushPhase::Idle,
            accumulated: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    // Wave 2 fills in ≥20 unit tests. Wave 0 leaves this empty.
}
