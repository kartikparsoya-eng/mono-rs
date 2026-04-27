use std::collections::HashMap;

use crate::filter::{Value, compare_values};
use crate::operator::Operator;
use crate::types::{Change, ChildData, Constraint, FetchRequest, Node, Row};

pub struct JoinOperator {
    parent: Box<dyn Operator>,
    child: Box<dyn Operator>,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
    // TS overlay tracking fields: #inprogressChildChange, #inprogressChildChangePosition.
    // In the TS generator model, these track the in-progress child push so that
    // fetch() calls during yield can apply an overlay (undo uncommitted changes
    // for not-yet-notified parents). In Rust's synchronous push model, push()
    // returns all changes atomically — no interleaving fetch is possible — so
    // the overlay is structurally unnecessary. Fields retained for 1:1 parity.
    #[allow(dead_code)]
    inprogress_child_change: Option<Change>,
    #[allow(dead_code)]
    inprogress_child_change_position: Option<Row>,
}

impl JoinOperator {
    pub fn new(
        parent: Box<dyn Operator>,
        child: Box<dyn Operator>,
        parent_key: Vec<String>,
        child_key: Vec<String>,
        relationship_name: String,
    ) -> Self {
        Self {
            parent,
            child,
            parent_key,
            child_key,
            relationship_name,
            inprogress_child_change: None,
            inprogress_child_change_position: None,
        }
    }

    /// Build a constraint for fetching children based on a parent row.
    /// Returns None if any parent key column is null/missing.
    fn build_child_constraint(&self, parent_row: &Row) -> Option<Constraint> {
        for pk in &self.parent_key {
            match parent_row.get(pk) {
                None | Some(serde_json::Value::Null) => return None,
                _ => {}
            }
        }
        let constraint = Constraint::from_pairs(
            self.parent_key.iter().zip(self.child_key.iter()).map(|(pk, ck)| {
                (ck.clone(), parent_row.get(pk).cloned().unwrap_or(serde_json::Value::Null))
            })
        );
        Some(constraint)
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

    /// Fetch children for a parent row, filtering by all key columns.
    fn fetch_children(&mut self, parent_row: &Row) -> Vec<Node> {
        let constraint = match self.build_child_constraint(parent_row) {
            Some(c) => c,
            None => return vec![],
        };
        let child_nodes = self.child.fetch(&FetchRequest {
            constraint: Some(constraint),
            ..Default::default()
        });
        if self.parent_key.len() > 1 {
            child_nodes
                .into_iter()
                .filter(|cn| self.is_join_match(parent_row, &cn.row))
                .collect()
        } else {
            child_nodes
        }
    }

    /// Attach children to a node by fetching from child source.
    fn attach_children(&mut self, mut node: Node) -> Node {
        let children = self.fetch_children(&node.row);
        node.relationships
            .insert(self.relationship_name.clone(), children);
        node
    }

    /// Build a "reverse" constraint for fetching parents based on a child row.
    /// Maps child_key columns → parent_key columns.
    /// Returns None if any child key column is null/missing.
    fn build_parent_constraint(&self, child_row: &Row) -> Option<Constraint> {
        for ck in &self.child_key {
            match child_row.get(ck) {
                None | Some(serde_json::Value::Null) => return None,
                _ => {}
            }
        }
        let constraint = Constraint::from_pairs(
            self.child_key
                .iter()
                .zip(self.parent_key.iter())
                .map(|(ck, pk)| {
                    (
                        pk.clone(),
                        child_row
                            .get(ck)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    )
                }),
        );
        Some(constraint)
    }

    /// TS `#pushChildChange`: find matching parents and emit Change::Child for each.
    fn push_child_change(&mut self, child_row: &Row, change: Change) -> Vec<Change> {
        // Set in-progress state (matches TS #pushChildChange)
        self.inprogress_child_change = Some(change.clone());
        self.inprogress_child_change_position = None;

        let constraint = match self.build_parent_constraint(child_row) {
            Some(c) => c,
            None => {
                self.inprogress_child_change = None;
                return vec![];
            }
        };
        let parent_nodes = self.parent.fetch(&FetchRequest {
            constraint: Some(constraint),
            ..Default::default()
        });

        let mut output = Vec::new();
        for parent_node in parent_nodes {
            // Track position for overlay (TS: this.#inprogressChildChangePosition = parentNode.row)
            self.inprogress_child_change_position = Some(parent_node.row.clone());
            let parent_with_rels = self.attach_children(parent_node);
            output.push(Change::Child {
                node: parent_with_rels,
                child: ChildData {
                    relationship_name: self.relationship_name.clone(),
                    change: Box::new(change.clone()),
                },
            });
        }

        // Clear in-progress state (TS: finally block)
        self.inprogress_child_change = None;
        output
    }

    /// Check if child key columns changed between old and new row.
    fn child_key_changed(&self, old_row: &Row, new_row: &Row) -> bool {
        for ck in &self.child_key {
            let old_v = old_row.get(ck).map(Value::from_json).unwrap_or(Value::Null);
            let new_v = new_row.get(ck).map(Value::from_json).unwrap_or(Value::Null);
            if compare_values(&old_v, &new_v) != std::cmp::Ordering::Equal {
                return true;
            }
        }
        false
    }

    /// Check if join key columns changed between old and new row.
    fn join_key_changed(&self, old_row: &Row, new_row: &Row) -> bool {
        for pk in &self.parent_key {
            let old_v = old_row.get(pk).map(Value::from_json).unwrap_or(Value::Null);
            let new_v = new_row.get(pk).map(Value::from_json).unwrap_or(Value::Null);
            if compare_values(&old_v, &new_v) != std::cmp::Ordering::Equal {
                return true;
            }
        }
        false
    }
}

impl Operator for JoinOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let parent_nodes = self.parent.fetch(req);
        parent_nodes
            .into_iter()
            .map(|mut node| {
                let children = self.fetch_children(&node.row);
                node.relationships
                    .insert(self.relationship_name.clone(), children);
                node
            })
            .collect()
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        match change {
            Change::Add(node) => {
                let node = self.attach_children(node);
                vec![Change::Add(node)]
            }
            Change::Remove(node) => {
                let node = self.attach_children(node);
                vec![Change::Remove(node)]
            }
            Change::Edit { node, old_node } => {
                // TS asserts the edit cannot change the join key
                debug_assert!(
                    !self.join_key_changed(&old_node.row, &node.row),
                    "Parent edit must not change relationship."
                );
                let children = self.fetch_children(&node.row);
                let rel_name = self.relationship_name.clone();
                let mut new_node = node;
                new_node
                    .relationships
                    .insert(rel_name.clone(), children.clone());
                let mut old = old_node;
                old.relationships.insert(rel_name, children);
                vec![Change::Edit {
                    node: new_node,
                    old_node: old,
                }]
            }
            Change::Child { node, child } => {
                // Preserve original child data (matches TS #pushParent CHILD case)
                let parent_with_rels = self.attach_children(node);
                vec![Change::Child {
                    node: parent_with_rels,
                    child,
                }]
            }
        }
    }

    fn op_type(&self) -> &'static str {
        "join"
    }

    /// TS `#pushChild`: handles changes from the child input.
    /// For each matching parent, wraps the child change as Change::Child.
    fn push_child(&mut self, change: Change) -> Vec<Change> {
        match &change {
            Change::Add(node) | Change::Remove(node) => {
                self.push_child_change(&node.row.clone(), change)
            }
            Change::Child { node, .. } => {
                self.push_child_change(&node.row.clone(), change)
            }
            Change::Edit { node, old_node } => {
                // TS asserts child edit cannot change the join key
                debug_assert!(
                    !self.child_key_changed(&old_node.row, &node.row),
                    "Child edit must not change relationship."
                );
                self.push_child_change(&node.row.clone(), change)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChangeType;

    struct MockSource {
        nodes: Vec<Node>,
    }

    impl Operator for MockSource {
        fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
            if let Some(ref constraint) = req.constraint {
                self.nodes
                    .iter()
                    .filter(|n| constraint.matches_row(&n.row))
                    .cloned()
                    .collect()
            } else {
                self.nodes.clone()
            }
        }
        fn push(&mut self, _change: Change) -> Vec<Change> {
            vec![]
        }
        fn op_type(&self) -> &'static str {
            "mock"
        }
    }

    fn make_node(pairs: &[(&str, serde_json::Value)]) -> Node {
        Node {
            row: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            relationships: HashMap::new(),
        }
    }

    #[test]
    fn test_join_fetch_attaches_children() {
        let parents = vec![
            make_node(&[
                ("id", serde_json::json!(1)),
                ("name", serde_json::json!("Alice")),
            ]),
            make_node(&[
                ("id", serde_json::json!(2)),
                ("name", serde_json::json!("Bob")),
            ]),
        ];
        let children = vec![
            make_node(&[
                ("parentId", serde_json::json!(1)),
                ("val", serde_json::json!("c1")),
            ]),
            make_node(&[
                ("parentId", serde_json::json!(1)),
                ("val", serde_json::json!("c2")),
            ]),
            make_node(&[
                ("parentId", serde_json::json!(2)),
                ("val", serde_json::json!("c3")),
            ]),
        ];

        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].relationships["children"].len(), 2);
        assert_eq!(result[1].relationships["children"].len(), 1);
    }

    #[test]
    fn test_join_push_parent_add_fetches_children() {
        let children = vec![make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        let parent = make_node(&[
            ("id", serde_json::json!(1)),
            ("name", serde_json::json!("Alice")),
        ]);
        let result = op.push(Change::Add(parent));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Add);
        assert_eq!(result[0].node().relationships["children"].len(), 1);
    }

    #[test]
    fn test_join_push_child_change_propagates() {
        let children = vec![make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "items".into(),
        );

        let parent = make_node(&[("id", serde_json::json!(1))]);
        let child_node = make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("new")),
        ]);
        let child_change = Change::Child {
            node: parent,
            child: ChildData {
                relationship_name: "orig".into(),
                change: Box::new(Change::Add(child_node)),
            },
        };

        let result = op.push(child_change);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Child);
        if let Change::Child { ref child, .. } = result[0] {
            // Preserves original child data (not overwritten to join's rel name)
            assert_eq!(child.relationship_name, "orig");
        } else {
            panic!("expected Child change");
        }
    }

    #[test]
    fn test_join_push_null_key_no_child_fetch() {
        let children = vec![make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        let parent = make_node(&[("id", serde_json::Value::Null)]);
        let result = op.push(Change::Add(parent));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].node().relationships["children"].len(), 0);
    }

    #[test]
    fn test_join_push_edit_same_key_attaches_children() {
        // TS asserts that parent edit cannot change the join key.
        // When the key is unchanged, the edit passes through with children attached.
        let children = vec![make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        let old_node = make_node(&[
            ("id", serde_json::json!(1)),
            ("name", serde_json::json!("old")),
        ]);
        let new_node = make_node(&[
            ("id", serde_json::json!(1)),
            ("name", serde_json::json!("new")),
        ]);
        let result = op.push(Change::Edit {
            node: new_node,
            old_node,
        });
        assert_eq!(result.len(), 1);
        match &result[0] {
            Change::Edit { node, old_node } => {
                assert_eq!(node.row.get("name").unwrap(), &serde_json::json!("new"));
                assert_eq!(old_node.row.get("name").unwrap(), &serde_json::json!("old"));
                assert!(node.relationships.contains_key("children"));
                assert!(old_node.relationships.contains_key("children"));
            }
            _ => panic!("Expected Edit change"),
        }
    }

    #[test]
    fn test_join_push_child_preserves_relationship_name() {
        // When a Child change comes from a nested relationship (not this join's),
        // the original relationship_name must be preserved.
        let children = vec![make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "items".into(),
        );

        let parent = make_node(&[("id", serde_json::json!(1))]);
        let inner_child = make_node(&[("nested_id", serde_json::json!(42))]);
        let child_change = Change::Child {
            node: parent,
            child: ChildData {
                relationship_name: "nested_rel".into(),
                change: Box::new(Change::Add(inner_child)),
            },
        };

        let result = op.push(child_change);
        assert_eq!(result.len(), 1);
        if let Change::Child { ref node, ref child } = result[0] {
            // Join attaches its own children to the parent node
            assert_eq!(node.relationships["items"].len(), 1);
            // But preserves the original child data
            assert_eq!(child.relationship_name, "nested_rel");
        } else {
            panic!("expected Child change");
        }
    }

    #[test]
    fn test_join_push_remove_attaches_children() {
        let children = vec![make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        let parent = make_node(&[
            ("id", serde_json::json!(1)),
            ("name", serde_json::json!("Alice")),
        ]);
        let result = op.push(Change::Remove(parent));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Remove);
        assert_eq!(result[0].node().relationships["children"].len(), 1);
    }

    #[test]
    fn test_join_push_edit_same_key_shares_children() {
        let children = vec![make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        let old_node = make_node(&[
            ("id", serde_json::json!(1)),
            ("name", serde_json::json!("old")),
        ]);
        let new_node = make_node(&[
            ("id", serde_json::json!(1)),
            ("name", serde_json::json!("new")),
        ]);
        let result = op.push(Change::Edit {
            node: new_node,
            old_node,
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Edit);
        if let Change::Edit { ref node, ref old_node } = result[0] {
            // Both nodes share the same children (key didn't change)
            assert_eq!(node.relationships["children"].len(), 1);
            assert_eq!(old_node.relationships["children"].len(), 1);
        } else {
            panic!("expected Edit");
        }
    }

    #[test]
    fn test_join_compound_key_filters_correctly() {
        let children = vec![
            make_node(&[
                ("a", serde_json::json!(1)),
                ("b", serde_json::json!(2)),
                ("val", serde_json::json!("match")),
            ]),
            make_node(&[
                ("a", serde_json::json!(1)),
                ("b", serde_json::json!(99)),
                ("val", serde_json::json!("no_match")),
            ]),
        ];
        let parents = vec![make_node(&[
            ("x", serde_json::json!(1)),
            ("y", serde_json::json!(2)),
        ])];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: children }),
            vec!["x".into(), "y".into()],
            vec!["a".into(), "b".into()],
            "items".into(),
        );

        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 1);
        // Only the child with a=1,b=2 should match
        assert_eq!(result[0].relationships["items"].len(), 1);
        assert_eq!(
            result[0].relationships["items"][0].row.get("val").unwrap(),
            &serde_json::json!("match")
        );
    }

    #[test]
    fn test_build_child_constraint_compound_key_uses_first_column() {
        // j3: build_child_constraint uses first parent_key/child_key column
        // for the DB constraint; compound filtering is done by is_join_match.
        let op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: vec![] }),
            vec!["org_id".into(), "dept_id".into()],
            vec!["parent_org".into(), "parent_dept".into()],
            "members".into(),
        );
        let parent_row: Row = vec![
            ("org_id".into(), serde_json::json!("acme")),
            ("dept_id".into(), serde_json::json!(42)),
        ]
        .into_iter()
        .collect();
        let c = op.build_child_constraint(&parent_row).unwrap();
        assert_eq!(c.columns.get("parent_org"), Some(&serde_json::json!("acme")));
        assert_eq!(c.columns.get("parent_dept"), Some(&serde_json::json!(42)));
        assert_eq!(c.columns.len(), 2);
    }

    #[test]
    fn test_build_child_constraint_null_key_returns_none() {
        let op = JoinOperator::new(
            Box::new(MockSource { nodes: vec![] }),
            Box::new(MockSource { nodes: vec![] }),
            vec!["org_id".into(), "dept_id".into()],
            vec!["parent_org".into(), "parent_dept".into()],
            "members".into(),
        );
        // Second key column is null → should return None
        let parent_row: Row = vec![
            ("org_id".into(), serde_json::json!("acme")),
            ("dept_id".into(), serde_json::Value::Null),
        ]
        .into_iter()
        .collect();
        assert!(op.build_child_constraint(&parent_row).is_none());
    }

    // ===== j1: push_child tests =====

    #[test]
    fn test_push_child_add_emits_child_change_per_parent() {
        // When a child is added, push_child should find matching parents
        // and emit Change::Child for each.
        let parents = vec![
            make_node(&[("id", serde_json::json!(1)), ("name", serde_json::json!("Alice"))]),
            make_node(&[("id", serde_json::json!(2)), ("name", serde_json::json!("Bob"))]),
        ];
        let children = vec![
            make_node(&[("parentId", serde_json::json!(1)), ("val", serde_json::json!("existing"))]),
        ];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        let new_child = make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("new_child")),
        ]);
        let result = op.push_child(Change::Add(new_child));
        // Should produce exactly 1 Change::Child (only parent id=1 matches)
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Child);
        if let Change::Child { ref node, ref child } = result[0] {
            // Parent node should be id=1 with children attached
            assert_eq!(node.row.get("id").unwrap(), &serde_json::json!(1));
            assert!(node.relationships.contains_key("children"));
            // The wrapped change should be the original Add
            assert_eq!(child.relationship_name, "children");
            assert_eq!(child.change.change_type(), ChangeType::Add);
        } else {
            panic!("expected Child change");
        }
    }

    #[test]
    fn test_push_child_remove_emits_child_change() {
        let parents = vec![
            make_node(&[("id", serde_json::json!(1))]),
        ];
        let children = vec![
            make_node(&[("parentId", serde_json::json!(1)), ("val", serde_json::json!("c1"))]),
        ];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "items".into(),
        );

        let removed = make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("c1")),
        ]);
        let result = op.push_child(Change::Remove(removed));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Child);
        if let Change::Child { ref child, .. } = result[0] {
            assert_eq!(child.relationship_name, "items");
            assert_eq!(child.change.change_type(), ChangeType::Remove);
        } else {
            panic!("expected Child");
        }
    }

    #[test]
    fn test_push_child_null_key_no_output() {
        let parents = vec![
            make_node(&[("id", serde_json::json!(1))]),
        ];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: vec![] }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        // Child with null join key → no matching parents → no output
        let child = make_node(&[("parentId", serde_json::Value::Null)]);
        let result = op.push_child(Change::Add(child));
        assert!(result.is_empty());
    }

    #[test]
    fn test_push_child_edit_same_key() {
        let parents = vec![
            make_node(&[("id", serde_json::json!(1))]),
        ];
        let children = vec![];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: children }),
            vec!["id".into()],
            vec!["parentId".into()],
            "items".into(),
        );

        let old_child = make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("old")),
        ]);
        let new_child = make_node(&[
            ("parentId", serde_json::json!(1)),
            ("val", serde_json::json!("new")),
        ]);
        let result = op.push_child(Change::Edit {
            node: new_child,
            old_node: old_child,
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Child);
        if let Change::Child { ref child, .. } = result[0] {
            assert_eq!(child.change.change_type(), ChangeType::Edit);
        } else {
            panic!("expected Child");
        }
    }

    #[test]
    fn test_push_child_multiple_matching_parents() {
        // Two parents with same join key value → child change emitted for each
        let parents = vec![
            make_node(&[("groupId", serde_json::json!(10)), ("name", serde_json::json!("A"))]),
            make_node(&[("groupId", serde_json::json!(10)), ("name", serde_json::json!("B"))]),
        ];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: vec![] }),
            vec!["groupId".into()],
            vec!["gid".into()],
            "members".into(),
        );

        let child = make_node(&[("gid", serde_json::json!(10)), ("val", serde_json::json!("x"))]);
        let result = op.push_child(Change::Add(child));
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|c| c.change_type() == ChangeType::Child));
    }

    #[test]
    fn test_push_child_no_matching_parent() {
        let parents = vec![
            make_node(&[("id", serde_json::json!(1))]),
        ];
        let mut op = JoinOperator::new(
            Box::new(MockSource { nodes: parents }),
            Box::new(MockSource { nodes: vec![] }),
            vec!["id".into()],
            vec!["parentId".into()],
            "children".into(),
        );

        // Child references parent id=99 which doesn't exist
        let child = make_node(&[("parentId", serde_json::json!(99))]);
        let result = op.push_child(Change::Add(child));
        assert!(result.is_empty());
    }
}
