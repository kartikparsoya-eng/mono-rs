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
            Change::Add(_) | Change::Remove(_) | Change::Edit { .. } => {
                let row = change.node().row.clone();
                if self.row_passes(&row) {
                    vec![change]
                } else {
                    vec![]
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
}
