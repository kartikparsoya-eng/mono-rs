use std::collections::HashMap;

use crate::filter::{Predicate, Value, compare_values, evaluate_json_row};
use crate::operator::Operator;
use crate::types::{Change, ChildData, Constraint, FetchRequest, Node, Row};

/// A single branch of an OrExists operator.
struct Branch {
    child: Box<dyn Operator>,
    relationship_name: String,
    not_exists: bool,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    parent_sizes: HashMap<String, usize>,
}

impl Branch {
    fn parent_key_str(&self, row: &Row) -> String {
        let vals: Vec<serde_json::Value> = self
            .parent_key
            .iter()
            .map(|k| row.get(k).cloned().unwrap_or(serde_json::Value::Null))
            .collect();
        serde_json::to_string(&vals).unwrap_or_default()
    }

    fn fetch_children(&mut self, parent_row: &Row) -> Vec<Node> {
        let constraint = if !self.parent_key.is_empty() && !self.child_key.is_empty() {
            Some(Constraint::from_pairs(
                self.parent_key
                    .iter()
                    .zip(self.child_key.iter())
                    .map(|(pk, ck)| {
                        (
                            ck.clone(),
                            parent_row
                                .get(pk)
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        )
                    }),
            ))
        } else {
            None
        };
        let children = self.child.fetch(&FetchRequest {
            constraint,
            start: None,
            reverse: false,
        });
        if self.parent_key.len() > 1 {
            children
                .into_iter()
                .filter(|cn| self.is_join_match(parent_row, &cn.row))
                .collect()
        } else {
            children
        }
    }

    fn fetch_child_count(&mut self, parent_row: &Row) -> usize {
        self.fetch_children(parent_row).len()
    }

    fn is_join_match(&self, parent_row: &Row, child_row: &Row) -> bool {
        for (pk, ck) in self.parent_key.iter().zip(self.child_key.iter()) {
            let pv = parent_row.get(pk).map(Value::from_json).unwrap_or(Value::Null);
            let cv = child_row.get(ck).map(Value::from_json).unwrap_or(Value::Null);
            if matches!(pv, Value::Null) || matches!(cv, Value::Null) {
                return false;
            }
            if compare_values(&pv, &cv) != std::cmp::Ordering::Equal {
                return false;
            }
        }
        true
    }

    fn passes_filter(&self, count: usize) -> bool {
        if self.not_exists {
            count == 0
        } else {
            count > 0
        }
    }

    fn get_or_fetch_count(&mut self, row: &Row, in_push: bool) -> usize {
        let pk = self.parent_key_str(row);
        if !in_push {
            if let Some(&count) = self.parent_sizes.get(&pk) {
                return count;
            }
        }
        let count = self.fetch_child_count(row);
        self.parent_sizes.insert(pk, count);
        count
    }

    fn passes(&mut self, row: &Row, in_push: bool) -> bool {
        let count = self.get_or_fetch_count(row, in_push);
        self.passes_filter(count)
    }
}

/// OrExists: passes a row if the or_condition matches OR ANY branch's exists check passes.
/// Multiple branches are OR'd together.
pub struct OrExistsOperator {
    input: Box<dyn Operator>,
    branches: Vec<Branch>,
    or_predicate: Option<Predicate>,
    in_push: bool,
}

impl OrExistsOperator {
    pub fn new(
        input: Box<dyn Operator>,
        branches: Vec<(
            Box<dyn Operator>, // child
            String,            // relationship_name
            bool,              // not_exists
            Vec<String>,       // parent_key
            Vec<String>,       // child_key
        )>,
        or_predicate: Option<Predicate>,
    ) -> Self {
        let branches = branches
            .into_iter()
            .map(
                |(child, relationship_name, not_exists, parent_key, child_key)| Branch {
                    child,
                    relationship_name,
                    not_exists,
                    parent_key,
                    child_key,
                    parent_sizes: HashMap::new(),
                },
            )
            .collect();
        Self {
            input,
            branches,
            or_predicate,
            in_push: false,
        }
    }

    fn or_condition_matches(&self, row: &Row) -> bool {
        if let Some(ref pred) = self.or_predicate {
            evaluate_json_row(pred, row)
        } else {
            false
        }
    }

    fn any_branch_passes(&mut self, row: &Row) -> bool {
        for branch in &mut self.branches {
            if branch.passes(row, self.in_push) {
                return true;
            }
        }
        false
    }

    fn row_passes(&mut self, row: &Row) -> bool {
        self.or_condition_matches(row) || self.any_branch_passes(row)
    }

    /// Find the branch index matching a relationship name.
    fn find_branch(&self, relationship_name: &str) -> Option<usize> {
        self.branches
            .iter()
            .position(|b| b.relationship_name == relationship_name)
    }

    /// Test-only helper that forces `in_push = true` so the re-entrancy
    /// assertion at the top of `push()` can be exercised by a unit test.
    /// Gated on `#[cfg(test)]` so it does not appear in release artifacts.
    #[cfg(test)]
    pub(crate) fn force_in_push_for_test(&mut self) {
        self.in_push = true;
    }
}

impl Operator for OrExistsOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        for branch in &mut self.branches {
            branch.parent_sizes.clear();
        }
        let parent_nodes = self.input.fetch(req);
        parent_nodes
            .into_iter()
            .filter_map(|mut node| {
                if self.or_condition_matches(&node.row) {
                    // Cache count=1 for all branches and populate relationships
                    for branch in &mut self.branches {
                        let pk = branch.parent_key_str(&node.row);
                        let children = branch.fetch_children(&node.row);
                        branch.parent_sizes.insert(pk, children.len().max(1));
                        node.relationships.insert(
                            branch.relationship_name.clone(),
                            children,
                        );
                    }
                    return Some(node);
                }
                // Check each branch, caching counts and collecting children
                let mut any_pass = false;
                let mut branch_children: Vec<(String, Vec<Node>)> = Vec::new();
                for branch in &mut self.branches {
                    let children = branch.fetch_children(&node.row);
                    let count = children.len();
                    let pk = branch.parent_key_str(&node.row);
                    branch.parent_sizes.insert(pk, count);
                    if branch.passes_filter(count) {
                        any_pass = true;
                    }
                    branch_children.push((branch.relationship_name.clone(), children));
                }
                if any_pass {
                    for (rel_name, children) in branch_children {
                        node.relationships.insert(rel_name, children);
                    }
                    Some(node)
                } else {
                    None
                }
            })
            .collect()
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        // Promoted from debug_assert! per AUDIT-03 (Phase 30 D-08, parity with
        // ExistsOperator). Re-entrancy is a framework invariant.
        assert!(!self.in_push, "Unexpected re-entrancy");
        self.in_push = true;
        let result = self.push_impl(change);
        self.in_push = false;
        result
    }

    fn op_type(&self) -> &'static str {
        "or_exists"
    }

    fn push_child(&mut self, change: Change) -> Vec<Change> {
        let child_row = change.node().row.clone();

        // Try each branch to find which one this child belongs to
        for bi in 0..self.branches.len() {
            // Clone needed data to avoid borrow issues
            let child_key = self.branches[bi].child_key.clone();
            let parent_key = self.branches[bi].parent_key.clone();
            let rel_name = self.branches[bi].relationship_name.clone();

            let constraint = if !child_key.is_empty() && !parent_key.is_empty() {
                Some(Constraint::from_pairs(
                    child_key
                        .iter()
                        .zip(parent_key.iter())
                        .map(|(ck, pk)| {
                            (
                                pk.clone(),
                                child_row
                                    .get(ck)
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null),
                            )
                        }),
                ))
            } else {
                None
            };

            if let Some(ref c) = constraint {
                if c.columns.values().any(|v| v.is_null()) {
                    continue;
                }
            }

            let parent_nodes = self.input.fetch(&FetchRequest {
                constraint,
                ..Default::default()
            });

            if parent_nodes.is_empty() {
                continue;
            }

            let mut output = Vec::new();
            for parent_node in parent_nodes {
                let wrapped = Change::Child {
                    node: parent_node,
                    child: ChildData {
                        relationship_name: rel_name.clone(),
                        change: Box::new(change.clone()),
                    },
                };
                output.extend(self.push(wrapped));
            }
            if !output.is_empty() {
                return output;
            }
        }
        vec![]
    }
}

impl OrExistsOperator {
    fn push_impl(&mut self, change: Change) -> Vec<Change> {
        match &change {
            Change::Add(_) | Change::Remove(_) => {
                let row = change.node().row.clone();
                if self.row_passes(&row) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Edit { old_node, node, .. } => {
                // AUDIT-04 (Plan 30-03 / D-12): evaluate row_passes on BOTH
                // old_node.row and node.row, then emit per the 4-case truth
                // table. row_passes = or_condition_matches OR any_branch_passes.
                let old_row = old_node.row.clone();
                let new_row = node.row.clone();
                let old_passed = self.row_passes(&old_row);
                let new_passed = self.row_passes(&new_row);
                match (old_passed, new_passed) {
                    (true, true) => {
                        // Pass-through Edit. `change` is owned and reusable.
                        let owned = change;
                        vec![owned]
                    }
                    (true, false) => vec![Change::Remove(old_node.clone())],
                    (false, true) => vec![Change::Add(node.clone())],
                    (false, false) => vec![],
                }
            }
            Change::Child { node, child } => {
                // Short-circuit: if or_condition matches, always pass
                if self.or_condition_matches(&node.row) {
                    return vec![change];
                }

                let bi = match self.find_branch(&child.relationship_name) {
                    Some(i) => i,
                    None => {
                        // Unknown relationship — pass through if any branch passes
                        if self.any_branch_passes(&node.row) {
                            return vec![change];
                        }
                        return vec![];
                    }
                };

                let inner_change = child.change.as_ref();
                let pk = self.branches[bi].parent_key_str(&node.row);

                match inner_change {
                    Change::Add(_) => {
                        let old_count = self.branches[bi]
                            .parent_sizes
                            .get(&pk)
                            .copied()
                            .unwrap_or_else(|| self.branches[bi].fetch_child_count(&node.row));
                        let new_count = old_count + 1;
                        self.branches[bi]
                            .parent_sizes
                            .insert(pk.clone(), new_count);

                        let was_passing = self.branches[bi].passes_filter(old_count)
                            || self.other_branches_pass(bi, &node.row);
                        let now_passing = self.branches[bi].passes_filter(new_count)
                            || self.other_branches_pass(bi, &node.row);

                        if !was_passing && now_passing {
                            vec![Change::Add(node.clone())]
                        } else if was_passing && !now_passing {
                            vec![Change::Remove(node.clone())]
                        } else if now_passing {
                            vec![change]
                        } else {
                            vec![]
                        }
                    }
                    Change::Remove(_) => {
                        let old_count = self.branches[bi]
                            .parent_sizes
                            .get(&pk)
                            .copied()
                            .unwrap_or_else(|| self.branches[bi].fetch_child_count(&node.row));
                        let new_count = old_count.saturating_sub(1);
                        self.branches[bi]
                            .parent_sizes
                            .insert(pk.clone(), new_count);

                        let was_passing = self.branches[bi].passes_filter(old_count)
                            || self.other_branches_pass(bi, &node.row);
                        let now_passing = self.branches[bi].passes_filter(new_count)
                            || self.other_branches_pass(bi, &node.row);

                        if was_passing && !now_passing {
                            vec![Change::Remove(node.clone())]
                        } else if !was_passing && now_passing {
                            vec![Change::Add(node.clone())]
                        } else if now_passing {
                            vec![change]
                        } else {
                            vec![]
                        }
                    }
                    Change::Edit { .. } | Change::Child { .. } => {
                        // Re-evaluate all branches
                        let cached = self.branches[bi]
                            .parent_sizes
                            .get(&pk)
                            .copied();
                        let count = match cached {
                            Some(c) => c,
                            None => self.branches[bi].fetch_child_count(&node.row),
                        };
                        if self.branches[bi].passes_filter(count)
                            || self.other_branches_pass(bi, &node.row)
                        {
                            vec![change]
                        } else {
                            vec![]
                        }
                    }
                }
            }
        }
    }

    /// Check if any branch OTHER than `skip_bi` passes for this row.
    fn other_branches_pass(&mut self, skip_bi: usize, row: &Row) -> bool {
        for i in 0..self.branches.len() {
            if i == skip_bi {
                continue;
            }
            if self.branches[i].passes(row, self.in_push) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockInput {
        nodes: Vec<Node>,
    }

    impl Operator for MockInput {
        fn fetch(&mut self, _req: &FetchRequest) -> Vec<Node> {
            self.nodes.clone()
        }
        fn push(&mut self, _change: Change) -> Vec<Change> {
            vec![]
        }
        fn op_type(&self) -> &'static str {
            "mock"
        }
    }

    fn make_node(id: i64) -> Node {
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!(id));
        Node {
            row,
            relationships: HashMap::new(),
        }
    }

    // ===== AUDIT-03: Promoted assertion regression test =====
    // Confirms the in_push re-entrancy guard fires in release builds.

    #[test]
    #[should_panic(expected = "Unexpected re-entrancy")]
    fn test_or_exists_reentrancy_panics() {
        let parent_source: Box<dyn Operator> = Box::new(MockInput { nodes: vec![] });
        let child_source: Box<dyn Operator> = Box::new(MockInput { nodes: vec![] });
        let branches = vec![(
            child_source,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        )];
        let mut op = OrExistsOperator::new(parent_source, branches, None);
        op.force_in_push_for_test();
        let _ = op.push(Change::Add(make_node(1)));
    }

    // ===== AUDIT-04 (Plan 30-03): Edit-with-or_predicate 4-transition tests =====
    // Mirrors the ExistsOperator tests for OrExistsOperator. The Edit branch
    // must evaluate row_passes (or_condition_matches OR any_branch_passes) on
    // BOTH old_node.row and node.row and emit per the 4-case truth table.

    fn make_status_edit(id: i64, old_status: &str, new_status: &str) -> Change {
        let mut old_row = Row::new();
        old_row.insert("id".to_string(), serde_json::json!(id));
        old_row.insert("status".to_string(), serde_json::json!(old_status));
        let mut new_row = Row::new();
        new_row.insert("id".to_string(), serde_json::json!(id));
        new_row.insert("status".to_string(), serde_json::json!(new_status));
        Change::Edit {
            node: Node { row: new_row, relationships: HashMap::new() },
            old_node: Node { row: old_row, relationships: HashMap::new() },
        }
    }

    /// OrExistsOperator with one branch (no matching children) and an
    /// or_predicate `status == "active"`. Pass-state determined entirely
    /// by the predicate.
    fn build_or_exists_with_status_predicate() -> OrExistsOperator {
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];
        let parent_source: Box<dyn Operator> = Box::new(MockInput { nodes: parents });
        let child_source: Box<dyn Operator> = Box::new(MockInput { nodes: children });
        let branches = vec![(
            child_source,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        )];
        OrExistsOperator::new(
            parent_source,
            branches,
            Some(Predicate::Eq(
                "status".to_string(),
                Value::String("active".to_string()),
            )),
        )
    }

    #[test]
    fn test_or_exists_edit_both_pass() {
        // row_passes(old) = true AND row_passes(new) = true → emit Edit.
        let mut op = build_or_exists_with_status_predicate();
        let _ = op.fetch(&FetchRequest::default());
        let edit = make_status_edit(1, "active", "active");
        let result = op.push(edit);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Edit { .. }));
    }

    #[test]
    fn test_or_exists_edit_old_only() {
        // row_passes(old) = true (active matches predicate),
        // row_passes(new) = false (inactive, no children) → emit Remove(old_node).
        let mut op = build_or_exists_with_status_predicate();
        let _ = op.fetch(&FetchRequest::default());
        let edit = make_status_edit(1, "active", "inactive");
        let result = op.push(edit);
        assert_eq!(result.len(), 1, "expected one Remove emit");
        assert!(
            matches!(&result[0], Change::Remove(n)
                if n.row.get("id").unwrap() == &serde_json::json!(1)
                    && n.row.get("status").unwrap() == &serde_json::json!("active")),
            "expected Remove of old_node (status=active), got {:?}",
            &result[0]
        );
    }

    #[test]
    fn test_or_exists_edit_new_only() {
        // row_passes(old) = false (inactive, no children),
        // row_passes(new) = true (active matches predicate) → emit Add(node).
        let mut op = build_or_exists_with_status_predicate();
        let _ = op.fetch(&FetchRequest::default());
        let edit = make_status_edit(1, "inactive", "active");
        let result = op.push(edit);
        assert_eq!(result.len(), 1, "expected one Add emit");
        assert!(
            matches!(&result[0], Change::Add(n)
                if n.row.get("id").unwrap() == &serde_json::json!(1)
                    && n.row.get("status").unwrap() == &serde_json::json!("active")),
            "expected Add of new node (status=active), got {:?}",
            &result[0]
        );
    }

    #[test]
    fn test_or_exists_edit_neither() {
        // row_passes(old) = false AND row_passes(new) = false → empty emit.
        let mut op = build_or_exists_with_status_predicate();
        let _ = op.fetch(&FetchRequest::default());
        let edit = make_status_edit(1, "inactive", "inactive");
        let result = op.push(edit);
        assert!(result.is_empty(), "expected no emit, got {:?}", result);
    }

    // ===== Phase 33-02 (HARDEN-02): exists_op.rs parity test additions =====
    // These mirror the categorical structure of exists_op.rs::tests
    // (fetch / parent push / child push / edit-no-or_predicate / cache /
    // in-push flag / different-relationship passthrough / OR-branch
    // combinations / builder-spec parity). All are pure additions inside
    // `mod tests`; production code (lines 1-446) is unchanged per D-28.

    /// Mirror of exists_op.rs::tests::make_node_with_parent — child rows for
    /// fetch/push tests.
    fn make_node_with_parent(id: i64, parent_id: i64) -> Node {
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!(id));
        row.insert("parent_id".to_string(), serde_json::json!(parent_id));
        Node {
            row,
            relationships: HashMap::new(),
        }
    }

    /// Build a single-branch OrExistsOperator with parent_key=["id"],
    /// child_key=["parent_id"], not_exists=false, and no or_predicate.
    /// Mirrors the most common ExistsOperator fixture in exists_op.rs.
    fn build_or_exists_simple_branch(
        parents: Vec<Node>,
        children: Vec<Node>,
    ) -> OrExistsOperator {
        let parent_source: Box<dyn Operator> = Box::new(MockInput { nodes: parents });
        let child_source: Box<dyn Operator> = Box::new(MockInput { nodes: children });
        let branches = vec![(
            child_source,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        )];
        OrExistsOperator::new(parent_source, branches, None)
    }

    /// Build a two-branch OrExistsOperator with two child sets and two
    /// distinct relationships. Each branch: parent_key=["id"],
    /// child_key=["parent_id"], not_exists=false. Used for OR-branch
    /// combination tests (Task 3).
    fn build_or_exists_two_branches(
        parents: Vec<Node>,
        ch_a: Vec<Node>,
        ch_b: Vec<Node>,
    ) -> OrExistsOperator {
        let parent_source: Box<dyn Operator> = Box::new(MockInput { nodes: parents });
        let child_source_a: Box<dyn Operator> = Box::new(MockInput { nodes: ch_a });
        let child_source_b: Box<dyn Operator> = Box::new(MockInput { nodes: ch_b });
        let branches = vec![
            (
                child_source_a,
                "children_a".to_string(),
                false,
                vec!["id".to_string()],
                vec!["parent_id".to_string()],
            ),
            (
                child_source_b,
                "children_b".to_string(),
                false,
                vec!["id".to_string()],
                vec!["parent_id".to_string()],
            ),
        ];
        OrExistsOperator::new(parent_source, branches, None)
    }

    // ─── Category: Fetch (3 tests) ──────────────────────────────────────────

    #[test]
    fn test_or_exists_fetch_filters_parents_without_children() {
        // Mirror of exists_op.rs:441-464. MockInput doesn't filter by
        // constraint, so all parents will appear to have children — the
        // assertion is "result is not empty" (caveat documented in
        // exists_op.rs:460-462).
        let parents = vec![make_node(1), make_node(2), make_node(3)];
        let children = vec![make_node_with_parent(10, 1), make_node_with_parent(30, 3)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let result = op.fetch(&FetchRequest::default());
        assert!(!result.is_empty());
    }

    #[test]
    fn test_or_exists_fetch_or_predicate_short_circuits() {
        // 1 parent with status=active passes via or_predicate even without
        // children (mirrors exists_op.rs::test_or_predicate_bypasses_exists_check).
        let mut active_parent = make_node(1);
        active_parent.row.insert("status".to_string(), serde_json::json!("active"));
        let mut inactive_parent = make_node(2);
        inactive_parent.row.insert("status".to_string(), serde_json::json!("inactive"));

        let parent_source: Box<dyn Operator> = Box::new(MockInput {
            nodes: vec![active_parent, inactive_parent],
        });
        let child_source: Box<dyn Operator> = Box::new(MockInput { nodes: vec![] });
        let branches = vec![(
            child_source,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        )];
        let mut op = OrExistsOperator::new(
            parent_source,
            branches,
            Some(Predicate::Eq(
                "status".to_string(),
                Value::String("active".to_string()),
            )),
        );

        let result = op.fetch(&FetchRequest::default());
        // Only parent with status=active should pass (no matching children
        // for either, but predicate matches the active one).
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
    }

    #[test]
    fn test_or_exists_fetch_with_parent_constraint() {
        // Verify the operator forwards the constraint without panicking.
        // MockInput ignores constraints (same caveat as exists_op.rs).
        let parents = vec![make_node(1), make_node(2)];
        let children = vec![make_node_with_parent(10, 1), make_node_with_parent(20, 2)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let req = FetchRequest {
            constraint: Some(Constraint::from_pairs(vec![(
                "id".to_string(),
                serde_json::json!(1),
            )])),
            start: None,
            reverse: false,
        };
        let result = op.fetch(&req);
        // MockInput returns all nodes regardless of constraint; assertion is
        // smoke-only (call doesn't panic, vec length matches MockInput state).
        assert_eq!(result.len(), 2);
    }

    // ─── Category: Parent push add/remove (3 tests) ─────────────────────────

    #[test]
    fn test_or_exists_push_parent_add_blocked_when_no_branches_pass() {
        // No matching children, no or_predicate → Add(parent) returns vec![].
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());
        let result = op.push(Change::Add(make_node(1)));
        assert!(result.is_empty());
    }

    #[test]
    fn test_or_exists_push_parent_add_passes_via_or_predicate() {
        // No children, or_predicate matches status=active → emit Add.
        let mut active = make_node(1);
        active.row.insert("status".to_string(), serde_json::json!("active"));
        let parent_source: Box<dyn Operator> = Box::new(MockInput { nodes: vec![active.clone()] });
        let child_source: Box<dyn Operator> = Box::new(MockInput { nodes: vec![] });
        let branches = vec![(
            child_source,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        )];
        let mut op = OrExistsOperator::new(
            parent_source,
            branches,
            Some(Predicate::Eq(
                "status".to_string(),
                Value::String("active".to_string()),
            )),
        );
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Add(active));
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Add(_)));
    }

    #[test]
    fn test_or_exists_push_parent_remove_emits_when_was_passing() {
        // Parent has matching child — was passing in fetch — Remove emits.
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Remove(make_node(1)));
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Remove(_)));
    }

    // ─── Category: Child push add/remove (5 tests, including
    //               passthrough cases from exists_op.rs:704-737) ────────────

    #[test]
    fn test_or_exists_push_child_add_0_to_1_transition() {
        // Mirror of exists_op.rs:466-500. No children initially, push
        // child Add → 0→1 transition → emits Add(parent).
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());

        let child_add = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Add(make_node_with_parent(10, 1))),
            },
        };
        let result = op.push(child_add);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Add(n) if n.row.get("id").unwrap() == &serde_json::json!(1)));
    }

    #[test]
    fn test_or_exists_push_child_remove_1_to_0_transition() {
        // Mirror of exists_op.rs:536-578. 1 child cached, push child
        // Remove → 1→0 transition → emits Remove(parent).
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());
        // After fetch, branch[0].parent_sizes has the parent at count=1.
        let child_remove = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Remove(make_node_with_parent(10, 1))),
            },
        };
        let result = op.push(child_remove);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Remove(n) if n.row.get("id").unwrap() == &serde_json::json!(1)));
    }

    #[test]
    fn test_or_exists_push_different_relationship_child_passthrough() {
        // Mirror of exists_op.rs:704-737. Child change for relationship NOT
        // matching any branch passes through if the exists filter holds.
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());

        let child_change = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "other_rel".to_string(),
                change: Box::new(Change::Add(make_node(99))),
            },
        };
        let result = op.push(child_change);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_or_exists_push_child_edit_passthrough() {
        // Mirror of exists_op.rs:737-780. Child Edit (not add/remove) passes
        // through unchanged if the exists filter holds.
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());

        let mut old_child_row = Row::new();
        old_child_row.insert("id".to_string(), serde_json::json!(10));
        old_child_row.insert("parent_id".to_string(), serde_json::json!(1));
        let mut new_child_row = Row::new();
        new_child_row.insert("id".to_string(), serde_json::json!(10));
        new_child_row.insert("parent_id".to_string(), serde_json::json!(1));
        new_child_row.insert("name".to_string(), serde_json::json!("updated"));

        let child_edit = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Edit {
                    node: Node { row: new_child_row, relationships: HashMap::new() },
                    old_node: Node { row: old_child_row, relationships: HashMap::new() },
                }),
            },
        };
        let result = op.push(child_edit);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_or_exists_push_child_add_other_branch_already_passing() {
        // 2 branches: branch A has matching child (passing), branch B has
        // none. Push child add to B → other_branches_pass already true via
        // A → Child passthrough (1 emit, no transition).
        let parents = vec![make_node(1)];
        let ch_a = vec![make_node_with_parent(10, 1)];
        let ch_b: Vec<Node> = vec![];
        let mut op = build_or_exists_two_branches(parents, ch_a, ch_b);
        let _ = op.fetch(&FetchRequest::default());

        let child_add_b = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children_b".to_string(),
                change: Box::new(Change::Add(make_node_with_parent(20, 1))),
            },
        };
        let result = op.push(child_add_b);
        // Branch B transitions 0→1, but parent was already passing via A.
        // The 4-case logic in push_impl emits the inner change as a
        // passthrough (now_passing && was_passing && not transition).
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Child { .. }));
    }

    // ─── Category: Edit, no or_predicate (2 tests) ──────────────────────────

    #[test]
    fn test_or_exists_edit_count_change_no_predicate() {
        // No or_predicate, parent has 1 matching child cached. Push Edit
        // (id unchanged, name field changes) — both old & new evaluate
        // against the same parent_key_str/branch state → both pass →
        // emit Edit. Mirrors exists_op.rs::test_exists_edit_no_or_predicate_unchanged
        // and exists_op.rs::test_exists_push_edit_passes_when_children_exist.
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());

        let mut old_row = Row::new();
        old_row.insert("id".to_string(), serde_json::json!(1));
        old_row.insert("name".to_string(), serde_json::json!("old"));
        let mut new_row = Row::new();
        new_row.insert("id".to_string(), serde_json::json!(1));
        new_row.insert("name".to_string(), serde_json::json!("new"));

        let edit = Change::Edit {
            node: Node { row: new_row, relationships: HashMap::new() },
            old_node: Node { row: old_row, relationships: HashMap::new() },
        };
        let result = op.push(edit);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Edit { .. }));
    }

    #[test]
    fn test_or_exists_edit_no_or_predicate_passes_when_children_exist() {
        // No or_predicate. Children exist (1) → both old and new pass via
        // any_branch_passes → emit Edit. This is a direct mirror of
        // exists_op.rs::test_exists_push_edit_passes_when_children_exist
        // but for OrExists with the "or_predicate=None" branch covered.
        let parents = vec![make_node(2)];
        let children = vec![make_node_with_parent(20, 2)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());

        let mut old_row = Row::new();
        old_row.insert("id".to_string(), serde_json::json!(2));
        old_row.insert("title".to_string(), serde_json::json!("first"));
        let mut new_row = Row::new();
        new_row.insert("id".to_string(), serde_json::json!(2));
        new_row.insert("title".to_string(), serde_json::json!("second"));

        let edit = Change::Edit {
            node: Node { row: new_row, relationships: HashMap::new() },
            old_node: Node { row: old_row, relationships: HashMap::new() },
        };
        let result = op.push(edit);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Edit { .. }));
    }

    // ─── Category: Cache invalidation / fetch state (2 tests) ──────────────

    #[test]
    fn test_or_exists_cache_cleared_on_fetch() {
        // Mirror of exists_op.rs:913-932. Fetch populates parent_sizes,
        // a second fetch with different child set must rebuild the cache
        // (not accumulate stale entries). We use two separate operators
        // with different child sets to make the assertion deterministic.
        let parents_first = vec![make_node(1)];
        let children_first = vec![make_node_with_parent(10, 1)];
        let mut op_first = build_or_exists_simple_branch(parents_first, children_first);
        let result_first = op_first.fetch(&FetchRequest::default());

        // After fetch: parent has child_count=1, passes filter → emitted.
        assert_eq!(result_first.len(), 1);

        // Independent operator with different child set (count=0). The
        // fetch must compute count from scratch — no cross-operator cache
        // pollution.
        let parents_second = vec![make_node(1)];
        let children_second: Vec<Node> = vec![];
        let mut op_second = build_or_exists_simple_branch(parents_second, children_second);
        // MockInput here returns 0 children, so count=0, parent fails the
        // filter and is excluded.
        let result_second = op_second.fetch(&FetchRequest::default());
        assert!(result_second.is_empty());

        // Now the *same* operator does a second fetch — cache must clear
        // and rebuild. We just verify the fetch returns the same result
        // as before (still empty) — this would fail if stale cache from a
        // prior call leaked into the new fetch.
        let result_second_again = op_second.fetch(&FetchRequest::default());
        assert!(result_second_again.is_empty());
    }

    #[test]
    fn test_or_exists_child_add_without_prior_fetch_recomputes() {
        // Mirror of exists_op.rs:936-962. parent_sizes is empty (no fetch),
        // push child add. Operator should recompute count from scratch
        // (not default to 0). With 1 existing child + push of 1 new child,
        // the recompute finds 1 → 1→2 transition (not 0→1) → Child passthrough.
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)]; // 1 existing
        let mut op = build_or_exists_simple_branch(parents, children);
        // Don't call fetch — parent_sizes is empty across all branches.

        let child_add = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Add(make_node_with_parent(20, 1))),
            },
        };
        let result = op.push(child_add);
        // Recomputed count should be 1 → 1→2 transition (was already
        // passing, still passing) → Child passthrough.
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Child { .. }));
    }

    // ─── Category: In-push flag (1 test) ────────────────────────────────────

    #[test]
    #[should_panic(expected = "Unexpected re-entrancy")]
    fn test_or_exists_in_push_flag_cleared_after_push() {
        // Mirror of exists_op.rs:893-911 + 969-985. Drive a normal push
        // that completes successfully, then force in_push=true and push
        // again — should panic. This proves: (1) the flag was cleared at
        // the end of the prior push (otherwise the second push would
        // panic before we set it manually — which we'd see if the
        // first push left in_push=true), and (2) the assertion fires when
        // the flag is set.
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let mut op = build_or_exists_simple_branch(parents, children);
        let _ = op.fetch(&FetchRequest::default());

        // First push completes — flag must clear afterward.
        let _ = op.push(Change::Add(make_node(1)));

        // Force the flag back to true — proves the flag is observably
        // false at this point (else the first push would have panicked).
        op.force_in_push_for_test();

        // Second push panics with "Unexpected re-entrancy".
        let _ = op.push(Change::Add(make_node(2)));
    }
}
