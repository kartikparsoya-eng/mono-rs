//! FanOut + FanIn composite operator (filter-graph variant).
//!
//! TS spec: `packages/zql/src/ivm/fan-out.ts` (83 LOC) +
//! `packages/zql/src/ivm/fan-in.ts` (94 LOC).
//!
//! # Architectural deviation from plan 36-02 (D-06 extension)
//!
//! The TS implementation splits this into two operators (`FanOut`, `FanIn`)
//! that communicate via callback registration (`setFilterOutput`, `setFanIn`)
//! and cooperative push (`fanOutDonePushingToAllBranches`).
//!
//! In Rust's synchronous-push model (Operator::push returns `Vec<Change>`),
//! splitting into two separate Operators with shared `Arc<Mutex<…>>` state
//! adds complexity without semantic benefit — the operators exist purely as
//! a generator-shaped seam for cooperative yield. We collapse them into a
//! single composite operator that owns N parallel branches and applies the
//! dedup/merge protocol synchronously.
//!
//! This is the same architectural pattern already used by `OrExistsOperator`
//! (Phase 17/35) for OR-of-EXISTS branches.
//!
//! Wire-format (`OperatorConfig`) still exposes `FanOut { branches }` per the
//! plan; the FanIn role is implicit at the same operator boundary.
//!
//! # Push protocol (TS-equivalent)
//!
//! 1. Receive Change from upstream.
//! 2. For each branch: clone Change, run through branch's pipeline,
//!    collect Vec<Change> output.
//! 3. Concatenate all branches' outputs into `accumulated`.
//! 4. Run `push_accumulated_changes(accumulated, fan_out_change_type,
//!    identity_merge, identity_make)` — filter-graph variant doesn't merge
//!    relationships (TS uses `identity` for both).
//! 5. Return resulting Vec<Change>.
//!
//! # Fetch protocol
//!
//! TS FanOut.fetch is not part of the FilterOperator interface (it's
//! filter-only). At the operator-tree level, the fan-out's *fetch* is the
//! union of branch fetches with no dedup (since this is filter-graph; the
//! Source upstream is shared). We delegate to branch[0] which is sufficient
//! when all branches share the same Source upstream — the canonical case.

use crate::operator::Operator;
use crate::push_accumulated::{
    identity_make, identity_merge, push_accumulated_changes,
};
use crate::types::{Change, FetchRequest, Node};

/// FanOut+FanIn composite operator.
///
/// Owns a fixed list of branch pipelines. Each branch is a `Box<dyn Operator>`
/// that itself terminates the per-branch sub-pipeline.
pub struct FanOutOperator {
    /// Branch pipelines. On push, each is fed the same input change.
    branches: Vec<Box<dyn Operator>>,
}

impl FanOutOperator {
    /// Construct a FanOut+FanIn composite over `branches`.
    ///
    /// At least one branch is required (TS construction asserts there's
    /// always a paired FanIn with non-empty inputs).
    pub fn new(branches: Vec<Box<dyn Operator>>) -> Self {
        assert!(
            !branches.is_empty(),
            "FanOut requires at least one branch"
        );
        Self { branches }
    }

    /// Number of branches (used by tests + serialization mirror).
    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }
}

impl Operator for FanOutOperator {
    /// TS: FanOut/FanIn are filter-only; fetch is delegated to the first
    /// branch (all branches share the same upstream Source under the
    /// canonical filter-graph shape).
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        // Fetch from each branch and dedup — when branches share the same
        // upstream, all branches return the same nodes; we want a single
        // copy. (FanIn semantics — filter-graph: each branch acts like a
        // filter-pass-through, so result is the *intersection-of-passes*
        // across branches, but at the IVM level the source is upstream of
        // FanOut so each branch's `fetch` returns identical nodes filtered
        // by that branch's downstream predicates.)
        //
        // For now, the canonical and only shape used by `applyOr` is N
        // sibling Filter branches off a shared Source. fetch from branch[0]
        // is sufficient *if* all branches share the same upstream filter
        // semantics. In practice, applyOr produces branches that each apply
        // a different sub-condition, and FanIn deduplicates downstream.
        //
        // We use the "union with dedup-by-value" approach to be correct in
        // the general case: collect all branches' fetches and dedup by
        // primary-key serialization. This matches TS's `mergeFetches` with
        // `identity` comparator (filter-graph variant has no compareRows).
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<Node> = Vec::new();
        for branch in self.branches.iter_mut() {
            let nodes = branch.fetch(req);
            for n in nodes {
                let key = serde_json::to_string(&n.row).unwrap_or_default();
                if seen.insert(key) {
                    out.push(n);
                }
            }
        }
        out
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        // TS FanOut.push: for each output, output.push(change, this).
        // Then fan_in.fanOutDonePushingToAllBranches(change.type).
        let fan_out_change_type = change.change_type();
        let mut accumulated: Vec<Change> = Vec::new();
        for branch in self.branches.iter_mut() {
            let branch_out = branch.push(change.clone());
            accumulated.extend(branch_out);
        }
        // FanIn flush — filter-graph variant uses identity merge/make.
        push_accumulated_changes(
            accumulated,
            fan_out_change_type,
            identity_merge,
            identity_make,
        )
    }

    fn op_type(&self) -> &'static str {
        "fan_out"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Predicate, Value};
    use crate::filter_op::FilterOperator;
    use crate::pipeline::SourceOperator;
    use crate::types::{ChangeType, ChildData, SortDirection, SortSpec};
    use std::collections::HashMap;

    fn make_node(pairs: &[(&str, serde_json::Value)]) -> Node {
        Node {
            row: pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            relationships: HashMap::new(),
        }
    }

    fn id_sort() -> Vec<SortSpec> {
        vec![SortSpec { field: "id".to_string(), direction: SortDirection::Asc }]
    }

    fn make_filter_branch(pred: Predicate) -> Box<dyn Operator> {
        Box::new(FilterOperator::new(
            Box::new(SourceOperator::new(vec![], id_sort())),
            pred,
        ))
    }

    /// Build a fan-out with N filter branches over an empty source.
    fn build_n_branch_fan_out(preds: Vec<Predicate>) -> FanOutOperator {
        let branches: Vec<Box<dyn Operator>> = preds
            .into_iter()
            .map(make_filter_branch)
            .collect();
        FanOutOperator::new(branches)
    }

    #[test]
    fn test_fan_out_constructs_with_one_branch() {
        let op = build_n_branch_fan_out(vec![Predicate::Eq(
            "x".to_string(),
            Value::Number(1.0),
        )]);
        assert_eq!(op.branch_count(), 1);
    }

    #[test]
    fn test_fan_out_constructs_with_three_branches() {
        let op = build_n_branch_fan_out(vec![
            Predicate::Eq("a".to_string(), Value::Number(1.0)),
            Predicate::Eq("b".to_string(), Value::Number(2.0)),
            Predicate::Eq("c".to_string(), Value::Number(3.0)),
        ]);
        assert_eq!(op.branch_count(), 3);
    }

    #[test]
    #[should_panic(expected = "at least one branch")]
    fn test_fan_out_zero_branches_panics() {
        let _ = FanOutOperator::new(vec![]);
    }

    #[test]
    fn test_fan_out_op_type_is_fan_out() {
        let op = build_n_branch_fan_out(vec![Predicate::Eq(
            "x".to_string(),
            Value::Number(1.0),
        )]);
        assert_eq!(op.op_type(), "fan_out");
    }

    #[test]
    fn test_fan_out_push_add_one_branch_passes_filter() {
        // One branch with `status == "active"` filter; push an active row.
        let mut op = build_n_branch_fan_out(vec![Predicate::Eq(
            "status".to_string(),
            Value::String("active".to_string()),
        )]);
        let change = Change::Add(make_node(&[
            ("id", serde_json::json!(1)),
            ("status", serde_json::json!("active")),
        ]));
        let out = op.push(change);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Add);
    }

    #[test]
    fn test_fan_out_push_add_no_branch_passes_returns_empty() {
        // One branch with `status == "active"` filter; push an inactive row.
        let mut op = build_n_branch_fan_out(vec![Predicate::Eq(
            "status".to_string(),
            Value::String("active".to_string()),
        )]);
        let change = Change::Add(make_node(&[
            ("id", serde_json::json!(1)),
            ("status", serde_json::json!("inactive")),
        ]));
        let out = op.push(change);
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn test_fan_out_push_add_multiple_branches_pass_dedups_to_one() {
        // Two branches both pass `id=1` — dedup must yield one.
        let mut op = build_n_branch_fan_out(vec![
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
        ]);
        let change = Change::Add(make_node(&[("id", serde_json::json!(1))]));
        let out = op.push(change);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn test_fan_out_push_remove_dedup() {
        let mut op = build_n_branch_fan_out(vec![
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
        ]);
        let change = Change::Remove(make_node(&[("id", serde_json::json!(1))]));
        let out = op.push(change);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Remove);
    }

    #[test]
    fn test_fan_out_push_edit_one_branch_pass() {
        let mut op = build_n_branch_fan_out(vec![Predicate::Eq(
            "id".to_string(),
            Value::Number(1.0),
        )]);
        let change = Change::Edit {
            node: make_node(&[("id", serde_json::json!(1)), ("v", serde_json::json!(2))]),
            old_node: make_node(&[("id", serde_json::json!(1)), ("v", serde_json::json!(1))]),
        };
        let out = op.push(change);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Edit);
    }

    #[test]
    fn test_fan_out_push_child_only_add_emitted() {
        // ChildChange whose branch converts to Add via filter logic.
        // FilterOperator forwards Child unchanged when row passes; here
        // we just check that a Child push survives end-to-end.
        let mut op = build_n_branch_fan_out(vec![Predicate::Eq(
            "id".to_string(),
            Value::Number(1.0),
        )]);
        let change = Change::Child {
            node: make_node(&[("id", serde_json::json!(1))]),
            child: ChildData {
                relationship_name: "r".to_string(),
                change: Box::new(Change::Add(make_node(&[("id", serde_json::json!(2))]))),
            },
        };
        let out = op.push(change);
        // Should pass through as a Child change.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Child);
    }

    #[test]
    fn test_fan_out_push_two_disjoint_branches_only_one_passes() {
        // Branch A: id=1; Branch B: id=2. Push id=1 → only A passes.
        let mut op = build_n_branch_fan_out(vec![
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
            Predicate::Eq("id".to_string(), Value::Number(2.0)),
        ]);
        let change = Change::Add(make_node(&[("id", serde_json::json!(1))]));
        let out = op.push(change);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].node().row.get("id").unwrap(), &serde_json::json!(1));
    }

    #[test]
    fn test_fan_out_push_disjoint_branches_neither_passes() {
        let mut op = build_n_branch_fan_out(vec![
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
            Predicate::Eq("id".to_string(), Value::Number(2.0)),
        ]);
        let change = Change::Add(make_node(&[("id", serde_json::json!(99))]));
        let out = op.push(change);
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn test_fan_out_fetch_empty_source_returns_empty() {
        let mut op = build_n_branch_fan_out(vec![Predicate::Eq(
            "id".to_string(),
            Value::Number(1.0),
        )]);
        let out = op.fetch(&FetchRequest::default());
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn test_fan_out_fetch_dedups_across_branches() {
        // Build one source with rows, then fan out to two filter branches
        // that both pass id=1; fetch should yield id=1 once after dedup.
        let rows = vec![
            make_node(&[("id", serde_json::json!(1))]),
            make_node(&[("id", serde_json::json!(2))]),
        ];
        let src1 = Box::new(SourceOperator::new(rows.clone(), id_sort()));
        let src2 = Box::new(SourceOperator::new(rows, id_sort()));
        let f1 = Box::new(FilterOperator::new(
            src1,
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
        ));
        let f2 = Box::new(FilterOperator::new(
            src2,
            Predicate::Eq("id".to_string(), Value::Number(1.0)),
        ));
        let mut op = FanOutOperator::new(vec![f1, f2]);
        let out = op.fetch(&FetchRequest::default());
        assert_eq!(out.len(), 1, "fetch should dedup across branches");
        assert_eq!(out[0].row.get("id").unwrap(), &serde_json::json!(1));
    }
}
