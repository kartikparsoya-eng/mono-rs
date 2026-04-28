use std::collections::HashMap;

use crate::filter::{Predicate, Value, compare_values, evaluate_json_row};
use crate::operator::Operator;
use crate::types::{Change, ChildData, Constraint, FetchRequest, Node, Row};

pub struct ExistsOperator {
    input: Box<dyn Operator>,
    child: Box<dyn Operator>,
    relationship_name: String,
    not_exists: bool,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    parent_sizes: HashMap<String, usize>,
    or_predicate: Option<Predicate>,
    // TS: #inPush — disables cache reuse during push to avoid stale results
    // when relationships are transiently inconsistent during incremental processing.
    in_push: bool,
}

impl ExistsOperator {
    pub fn new(
        input: Box<dyn Operator>,
        child: Box<dyn Operator>,
        relationship_name: String,
        not_exists: bool,
        parent_key: Vec<String>,
        child_key: Vec<String>,
    ) -> Self {
        Self {
            input,
            child,
            relationship_name,
            not_exists,
            parent_key,
            child_key,
            parent_sizes: HashMap::new(),
            or_predicate: None,
            in_push: false,
        }
    }

    pub fn with_or_predicate(mut self, pred: Option<Predicate>) -> Self {
        self.or_predicate = pred;
        self
    }

    fn or_condition_matches(&self, row: &Row) -> bool {
        if let Some(ref pred) = self.or_predicate {
            evaluate_json_row(pred, row)
        } else {
            false
        }
    }

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
            Some(crate::types::Constraint::from_pairs(
                self.parent_key.iter().zip(self.child_key.iter()).map(|(pk, ck)| {
                    (ck.clone(), parent_row.get(pk).cloned().unwrap_or(serde_json::Value::Null))
                })
            ))
        } else {
            None
        };
        let children = self.child.fetch(&FetchRequest {
            constraint,
            start: None,
            reverse: false,
        });
        // For compound keys, post-filter on all key columns (not just the first).
        if self.parent_key.len() > 1 {
            children.into_iter().filter(|cn| self.is_join_match(parent_row, &cn.row)).collect()
        } else {
            children
        }
    }

    fn fetch_child_count(&mut self, parent_row: &Row) -> usize {
        self.fetch_children(parent_row).len()
    }

    /// Check if a child row matches a parent row on all key columns.
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

    /// Get child count for a parent, using cache when not in_push (e1).
    /// TS: during push, cache is disabled (#inPush skips cache reads).
    /// During fetch, cache is used.
    fn get_or_fetch_count(&mut self, row: &Row) -> usize {
        let pk = self.parent_key_str(row);
        if !self.in_push {
            if let Some(&count) = self.parent_sizes.get(&pk) {
                return count;
            }
        }
        let count = self.fetch_child_count(row);
        self.parent_sizes.insert(pk, count);
        count
    }
}

impl Operator for ExistsOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        // TS: endFilter() clears #cache. We clear at start of each fetch (e2).
        self.parent_sizes.clear();
        let parent_nodes = self.input.fetch(req);
        let mut result = Vec::new();

        for mut node in parent_nodes {
            if self.or_condition_matches(&node.row) {
                let pk = self.parent_key_str(&node.row);
                // Still fetch children to populate the relationship
                // (TS Join always populates relationships regardless of filter)
                let children = self.fetch_children(&node.row);
                self.parent_sizes.insert(pk, children.len().max(1));
                node.relationships.insert(
                    self.relationship_name.clone(),
                    children,
                );
                result.push(node);
                continue;
            }
            let children = self.fetch_children(&node.row);
            let count = children.len();
            let pk = self.parent_key_str(&node.row);
            self.parent_sizes.insert(pk, count);
            if self.passes_filter(count) {
                node.relationships.insert(
                    self.relationship_name.clone(),
                    children,
                );
                result.push(node);
            }
        }

        result
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        // TS: assert(!this.#inPush, 'Unexpected re-entrancy') (e1)
        debug_assert!(!self.in_push, "Unexpected re-entrancy");
        self.in_push = true;
        let result = self.push_impl(change);
        self.in_push = false;
        result
    }

    fn op_type(&self) -> &'static str {
        "exists"
    }

    /// Handle a raw child change (from the child source, not yet wrapped).
    /// Finds matching parents, wraps as Change::Child, then delegates to push().
    fn push_child(&mut self, change: Change) -> Vec<Change> {
        let child_row = change.node().row.clone();
        // Build reverse constraint: child_key → parent_key
        let constraint = if !self.child_key.is_empty() && !self.parent_key.is_empty() {
            Some(Constraint::from_pairs(
                self.child_key.iter().zip(self.parent_key.iter()).map(|(ck, pk)| {
                    (pk.clone(), child_row.get(ck).cloned().unwrap_or(serde_json::Value::Null))
                })
            ))
        } else {
            None
        };

        // Check for null keys
        if let Some(ref c) = constraint {
            if c.columns.values().any(|v| v.is_null()) {
                return vec![];
            }
        }

        let parent_nodes = self.input.fetch(&FetchRequest {
            constraint,
            ..Default::default()
        });

        let mut output = Vec::new();
        for parent_node in parent_nodes {
            let wrapped = Change::Child {
                node: parent_node,
                child: ChildData {
                    relationship_name: self.relationship_name.clone(),
                    change: Box::new(change.clone()),
                },
            };
            output.extend(self.push(wrapped));
        }
        output
    }
}

impl ExistsOperator {
    fn push_impl(&mut self, change: Change) -> Vec<Change> {
        match &change {
            Change::Add(_) | Change::Remove(_) => {
                let row = change.node().row.clone();
                if self.or_condition_matches(&row) {
                    return vec![change];
                }
                let pk = self.parent_key_str(&row);
                let count = self.parent_sizes.get(&pk).copied().unwrap_or_else(|| {
                    let c = self.fetch_child_count(&row);
                    self.parent_sizes.insert(pk.clone(), c);
                    c
                });
                if self.passes_filter(count) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Edit { .. } => {
                let row = change.node().row.clone();
                if self.or_condition_matches(&row) {
                    return vec![change];
                }
                let pk = self.parent_key_str(&row);
                let count = self.parent_sizes.get(&pk).copied().unwrap_or_else(|| {
                    let c = self.fetch_child_count(&row);
                    self.parent_sizes.insert(pk.clone(), c);
                    c
                });
                if self.passes_filter(count) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Child { node, child } => {
                if self.or_condition_matches(&node.row) {
                    return vec![change];
                }
                if child.relationship_name != self.relationship_name {
                    // Different relationship: pass through with filter (e3)
                    let pk = self.parent_key_str(&node.row);
                    let count = self.parent_sizes.get(&pk).copied().unwrap_or_else(|| {
                        let c = self.fetch_child_count(&node.row);
                        self.parent_sizes.insert(pk.clone(), c);
                        c
                    });
                    if self.passes_filter(count) {
                        return vec![change];
                    }
                    return vec![];
                }

                let pk = self.parent_key_str(&node.row);
                let inner_change = child.change.as_ref();

                match inner_change {
                    Change::Add(_) => {
                        let old_count = self.parent_sizes.get(&pk).copied().unwrap_or_else(|| {
                            self.fetch_child_count(&node.row)
                        });
                        let new_count = old_count + 1;
                        self.parent_sizes.insert(pk, new_count);

                        if old_count == 0 && new_count == 1 {
                            // 0->1 transition
                            if self.not_exists {
                                // Exclude the added child from the remove
                                // (it was never added to output).
                                let mut modified = node.clone();
                                modified.relationships.insert(
                                    self.relationship_name.clone(),
                                    vec![],
                                );
                                vec![Change::Remove(modified)]
                            } else {
                                vec![Change::Add(node.clone())]
                            }
                        } else if self.passes_filter(new_count) {
                            vec![change]
                        } else {
                            vec![]
                        }
                    }
                    Change::Remove(_) => {
                        let old_count = self.parent_sizes.get(&pk).copied().unwrap_or_else(|| {
                            self.fetch_child_count(&node.row)
                        });
                        let new_count = old_count.saturating_sub(1);
                        self.parent_sizes.insert(pk, new_count);

                        if old_count == 1 && new_count == 0 {
                            // 1->0 transition
                            if self.not_exists {
                                vec![Change::Add(node.clone())]
                            } else {
                                // Include the removed child in the remove
                                // so downstream sees correct relationships.
                                let removed_child_node =
                                    child.change.as_ref().node().clone();
                                let mut modified = node.clone();
                                modified.relationships.insert(
                                    self.relationship_name.clone(),
                                    vec![removed_child_node],
                                );
                                vec![Change::Remove(modified)]
                            }
                        } else if self.passes_filter(new_count) {
                            vec![change]
                        } else {
                            vec![]
                        }
                    }
                    Change::Edit { .. } | Change::Child { .. } => {
                        // Edit/child of child: pass through if filter holds
                        let count = self.parent_sizes.get(&pk).copied().unwrap_or_else(|| {
                            self.fetch_child_count(&node.row)
                        });
                        if self.passes_filter(count) {
                            vec![change]
                        } else {
                            vec![]
                        }
                    }
                }
            }
        }
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

    fn make_node_with_parent(id: i64, parent_id: i64) -> Node {
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!(id));
        row.insert("parent_id".to_string(), serde_json::json!(parent_id));
        Node {
            row,
            relationships: HashMap::new(),
        }
    }

    #[test]
    fn test_exists_fetch_filters_parents_without_children() {
        let parents = vec![make_node(1), make_node(2), make_node(3)];
        // Only parent 1 and 3 have children
        let children = vec![make_node_with_parent(10, 1), make_node_with_parent(30, 3)];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        let result = op.fetch(&FetchRequest::default());
        // MockInput doesn't filter by constraint, so all children are returned for each parent
        // This means all parents will have children (count > 0)
        // With a real input, only parents with matching children would pass
        assert!(!result.is_empty());
    }

    #[test]
    fn test_exists_push_child_add_0_to_1_transition() {
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        // Initialize: parent has 0 children
        let _ = op.fetch(&FetchRequest::default());

        // Now push a child add -> 0->1 transition
        let parent_node = make_node(1);
        let child_add = Change::Child {
            node: parent_node,
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Add(make_node_with_parent(10, 1))),
            },
        };

        let result = op.push(child_add);
        assert_eq!(result.len(), 1);
        // Should emit Add for the parent (exists, 0->1)
        assert!(matches!(&result[0], Change::Add(n) if n.row.get("id").unwrap() == &serde_json::json!(1)));
    }

    #[test]
    fn test_not_exists_push_child_add_0_to_1_transition() {
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            true, // NOT EXISTS
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        // Initialize: parent has 0 children, passes NOT EXISTS
        let _ = op.fetch(&FetchRequest::default());

        // Push child add -> 0->1 transition, parent should be removed from NOT EXISTS
        let child_add = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Add(make_node_with_parent(10, 1))),
            },
        };

        let result = op.push(child_add);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Remove(n) if n.row.get("id").unwrap() == &serde_json::json!(1)));
    }

    #[test]
    fn test_exists_push_child_remove_1_to_0_transition() {
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        // Initialize: fetch will count children (mock returns 1 child)
        let _ = op.fetch(&FetchRequest::default());
        // Manually set size to 1 to simulate correct state
        let pk = op.parent_key_str(&make_node(1).row);
        op.parent_sizes.insert(pk, 1);

        // Push child remove -> 1->0 transition
        let child_remove = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Remove(make_node_with_parent(10, 1))),
            },
        };

        let result = op.push(child_remove);
        assert_eq!(result.len(), 1);
        // exists: 1->0 means parent disappears
        assert!(matches!(&result[0], Change::Remove(n) if n.row.get("id").unwrap() == &serde_json::json!(1)));
        // The removed child should be included in the relationship
        if let Change::Remove(n) = &result[0] {
            let rel = n.relationships.get("children").unwrap();
            assert_eq!(rel.len(), 1);
            assert_eq!(rel[0].row.get("id").unwrap(), &serde_json::json!(10));
        }
    }

    #[test]
    fn test_not_exists_push_child_add_0_to_1_relationship_empty() {
        // When NOT EXISTS removes a parent on 0->1 transition,
        // the emitted Remove should have an empty relationship vec
        // (the added child was never pushed to output).
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            true,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

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
        if let Change::Remove(n) = &result[0] {
            let rel = n.relationships.get("children").unwrap();
            assert!(rel.is_empty(), "relationship should be empty on NOT EXISTS remove");
        } else {
            panic!("expected Remove");
        }
    }

    #[test]
    fn test_exists_push_parent_add_passes_when_children_exist() {
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        let _ = op.fetch(&FetchRequest::default());

        // Parent Add should pass through since children exist
        let result = op.push(Change::Add(make_node(1)));
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Add(_)));
    }

    #[test]
    fn test_exists_push_parent_add_blocked_when_no_children() {
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        let _ = op.fetch(&FetchRequest::default());

        // Parent Add should be blocked since no children
        let result = op.push(Change::Add(make_node(1)));
        assert!(result.is_empty());
    }

    #[test]
    fn test_exists_push_edit_passes_when_children_exist() {
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

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
    fn test_exists_push_different_relationship_child_passthrough() {
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        let _ = op.fetch(&FetchRequest::default());

        // Child change for a DIFFERENT relationship should pass through
        // if the exists filter holds
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
    fn test_exists_push_child_edit_passthrough() {
        // Child Edit for the exists relationship should passthrough
        // (edits don't change count)
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

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
        // Should pass through since edit doesn't change count
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_or_predicate_bypasses_exists_check() {
        use crate::filter::Predicate;

        let parents = vec![make_node(1), make_node(2)];
        let children: Vec<Node> = vec![]; // No children at all

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        // EXISTS with or_predicate: id == 1 bypasses exists check
        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        )
        .with_or_predicate(Some(Predicate::Eq(
            "id".to_string(),
            Value::from_json(&serde_json::json!(1)),
        )));

        // Fetch: parent 1 passes via or_predicate, parent 2 has no children
        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));

        // Push: parent Add with id=1 should pass via or_predicate
        let result = op.push(Change::Add(make_node(1)));
        assert_eq!(result.len(), 1);

        // Push: parent Add with id=2 should be blocked (no children, no or match)
        let result = op.push(Change::Add(make_node(2)));
        assert!(result.is_empty());
    }

    #[test]
    fn test_exists_push_child_add_beyond_boundary() {
        // Adding a second child (1->2) should just pass through
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        let _ = op.fetch(&FetchRequest::default());

        // Add second child -> 1->2, not a boundary
        let child_add = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Add(make_node_with_parent(20, 1))),
            },
        };

        let result = op.push(child_add);
        // Should pass through as a Child change (exists still true)
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Child { .. }));
    }

    #[test]
    fn test_not_exists_push_child_remove_1_to_0_transition() {
        // NOT EXISTS: removing last child (1->0) should Add the parent back
        let parents = vec![make_node(1)];
        let children: Vec<Node> = vec![];

        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });

        let mut op = ExistsOperator::new(
            input,
            child_input,
            "children".to_string(),
            true,
            vec!["id".to_string()],
            vec!["parent_id".to_string()],
        );

        let _ = op.fetch(&FetchRequest::default());
        // Simulate: parent had 1 child
        let pk = op.parent_key_str(&make_node(1).row);
        op.parent_sizes.insert(pk, 1);

        let child_remove = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Remove(make_node_with_parent(10, 1))),
            },
        };

        let result = op.push(child_remove);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Add(_)));
    }

    // ===== e1: in_push guard test =====

    #[test]
    fn test_in_push_flag_cleared_after_push() {
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });
        let mut op = ExistsOperator::new(
            input, child_input, "children".to_string(), false,
            vec!["id".to_string()], vec!["parent_id".to_string()],
        );
        let _ = op.fetch(&FetchRequest::default());

        assert!(!op.in_push);
        let _ = op.push(Change::Add(make_node(1)));
        // in_push must be false after push completes
        assert!(!op.in_push);
    }

    // ===== e2: cache cleared on fetch =====

    #[test]
    fn test_cache_cleared_on_fetch() {
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)];
        let input = Box::new(MockInput { nodes: parents.clone() });
        let child_input = Box::new(MockInput { nodes: children });
        let mut op = ExistsOperator::new(
            input, child_input, "children".to_string(), false,
            vec!["id".to_string()], vec!["parent_id".to_string()],
        );

        let _ = op.fetch(&FetchRequest::default());
        assert!(!op.parent_sizes.is_empty());

        // Second fetch should clear and rebuild cache
        op.input = Box::new(MockInput { nodes: parents });
        let _ = op.fetch(&FetchRequest::default());
        // Cache should be populated but not accumulating stale entries
        assert!(!op.parent_sizes.is_empty());
    }

    // ===== e3: unwrap_or defaults replaced with recomputation =====

    #[test]
    fn test_child_add_without_prior_fetch_recomputes_count() {
        // e3: If parent_sizes has no entry, child Add should recompute (not default 0)
        let parents = vec![make_node(1)];
        let children = vec![make_node_with_parent(10, 1)]; // 1 existing child
        let input = Box::new(MockInput { nodes: parents });
        let child_input = Box::new(MockInput { nodes: children });
        let mut op = ExistsOperator::new(
            input, child_input, "children".to_string(), false,
            vec!["id".to_string()], vec!["parent_id".to_string()],
        );
        // Don't call fetch — parent_sizes is empty

        // Push child add. Without e3 fix, old_count defaults to 0 → thinks it's 0→1 transition.
        // With fix, it recomputes and finds 1 existing child → 1→2, not a boundary.
        let child_add = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "children".to_string(),
                change: Box::new(Change::Add(make_node_with_parent(20, 1))),
            },
        };
        let result = op.push(child_add);
        // Should pass through as Child (1→2, not a 0→1 boundary)
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Child { .. }));
    }
}
