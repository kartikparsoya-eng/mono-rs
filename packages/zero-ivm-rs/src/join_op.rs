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
        let val = parent_row.get(&self.parent_key[0])?.clone();
        Some(Constraint {
            key: self.child_key[0].clone(),
            value: val,
        })
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

    /// Fetch children, checking overlay first, then delegating to child.fetch().
    fn fetch_children_with_overlay(
        &mut self,
        parent_row: &Row,
        overlay: &HashMap<String, Vec<Node>>,
    ) -> Vec<Node> {
        let key = self.serialize_join_key(parent_row);
        if let Some(nodes) = overlay.get(&key) {
            return nodes.clone();
        }
        self.fetch_children(parent_row)
    }

    /// Serialize the join key values from a parent row for overlay lookup.
    fn serialize_join_key(&self, row: &Row) -> String {
        let vals: Vec<String> = self
            .parent_key
            .iter()
            .map(|k| {
                row.get(k)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string())
            })
            .collect();
        vals.join("|")
    }

    /// Attach children to a node using overlay.
    fn attach_children_with_overlay(
        &mut self,
        mut node: Node,
        overlay: &HashMap<String, Vec<Node>>,
    ) -> Node {
        let children = self.fetch_children_with_overlay(&node.row, overlay);
        node.relationships
            .insert(self.relationship_name.clone(), children);
        node
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
        let overlay: HashMap<String, Vec<Node>> = HashMap::new();

        match change {
            Change::Add(node) => {
                let node = self.attach_children_with_overlay(node, &overlay);
                vec![Change::Add(node)]
            }
            Change::Remove(node) => {
                let node = self.attach_children_with_overlay(node, &overlay);
                vec![Change::Remove(node)]
            }
            Change::Edit { node, old_node } => {
                if self.join_key_changed(&old_node.row, &node.row) {
                    let old_with_children =
                        self.attach_children_with_overlay(old_node, &overlay);
                    let new_with_children =
                        self.attach_children_with_overlay(node, &overlay);
                    vec![
                        Change::Remove(old_with_children),
                        Change::Add(new_with_children),
                    ]
                } else {
                    let children = self.fetch_children_with_overlay(&node.row, &overlay);
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
            }
            Change::Child { node, child } => {
                let parent_with_rels =
                    self.attach_children_with_overlay(node, &overlay);
                vec![Change::Child {
                    node: parent_with_rels,
                    child: ChildData {
                        relationship_name: self.relationship_name.clone(),
                        change: child.change,
                    },
                }]
            }
        }
    }

    fn op_type(&self) -> &'static str {
        "join"
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
                    .filter(|n| n.row.get(&constraint.key) == Some(&constraint.value))
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
            assert_eq!(child.relationship_name, "items");
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
    fn test_join_push_edit_key_changed_splits() {
        let children = vec![
            make_node(&[
                ("parentId", serde_json::json!(1)),
                ("val", serde_json::json!("c1")),
            ]),
            make_node(&[
                ("parentId", serde_json::json!(2)),
                ("val", serde_json::json!("c2")),
            ]),
        ];
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
            ("id", serde_json::json!(2)),
            ("name", serde_json::json!("new")),
        ]);
        let result = op.push(Change::Edit {
            node: new_node,
            old_node,
        });
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].change_type(), ChangeType::Remove);
        assert_eq!(result[1].change_type(), ChangeType::Add);
    }
}
