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
        debug_assert!(!self.in_push, "Unexpected re-entrancy");
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
