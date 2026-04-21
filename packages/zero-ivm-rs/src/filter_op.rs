use std::collections::HashMap;

use crate::filter::{Predicate, Value, evaluate};
use crate::operator::Operator;
use crate::types::{Change, FetchRequest, Node, Row};

pub struct FilterOperator {
    input: Box<dyn Operator>,
    predicate: Predicate,
}

impl FilterOperator {
    pub fn new(input: Box<dyn Operator>, predicate: Predicate) -> Self {
        Self { input, predicate }
    }

    fn matches_row(&self, row: &Row) -> bool {
        let map = row_to_value_map(row);
        evaluate(&self.predicate, &map)
    }
}

fn row_to_value_map(row: &Row) -> HashMap<String, Value> {
    row.iter()
        .map(|(k, v)| (k.clone(), Value::from_json(v)))
        .collect()
}

impl Operator for FilterOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        self.input
            .fetch(req)
            .into_iter()
            .filter(|node| self.matches_row(&node.row))
            .collect()
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        match change {
            Change::Add(ref node) => {
                if self.matches_row(&node.row) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Remove(ref node) => {
                if self.matches_row(&node.row) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Child { ref node, .. } => {
                if self.matches_row(&node.row) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Edit {
                ref node,
                ref old_node,
            } => {
                let old_matches = self.matches_row(&old_node.row);
                let new_matches = self.matches_row(&node.row);
                match (old_matches, new_matches) {
                    (true, true) => vec![change],
                    (true, false) => vec![Change::Remove(old_node.clone())],
                    (false, true) => vec![Change::Add(node.clone())],
                    (false, false) => vec![],
                }
            }
        }
    }

    fn op_type(&self) -> &'static str {
        "filter"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChangeType;
    use std::collections::HashMap as StdHashMap;

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

    fn make_node(pairs: &[(&str, serde_json::Value)]) -> Node {
        Node {
            row: pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            relationships: StdHashMap::new(),
        }
    }

    #[test]
    fn test_filter_fetch() {
        let nodes = vec![
            make_node(&[("status", serde_json::json!("active")), ("id", serde_json::json!(1))]),
            make_node(&[("status", serde_json::json!("deleted")), ("id", serde_json::json!(2))]),
            make_node(&[("status", serde_json::json!("active")), ("id", serde_json::json!(3))]),
        ];
        let input = Box::new(MockInput { nodes });
        let pred = Predicate::Eq("status".into(), Value::String("active".into()));
        let mut op = FilterOperator::new(input, pred);

        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(3));
    }

    #[test]
    fn test_filter_push_add_remove() {
        let input = Box::new(MockInput { nodes: vec![] });
        let pred = Predicate::Eq("status".into(), Value::String("active".into()));
        let mut op = FilterOperator::new(input, pred);

        let matching = make_node(&[("status", serde_json::json!("active"))]);
        let non_matching = make_node(&[("status", serde_json::json!("deleted"))]);

        assert_eq!(op.push(Change::Add(matching.clone())).len(), 1);
        assert_eq!(op.push(Change::Add(non_matching.clone())).len(), 0);
        assert_eq!(op.push(Change::Remove(matching)).len(), 1);
        assert_eq!(op.push(Change::Remove(non_matching)).len(), 0);
    }

    #[test]
    fn test_filter_push_edit_split() {
        let input = Box::new(MockInput { nodes: vec![] });
        let pred = Predicate::Eq("status".into(), Value::String("active".into()));
        let mut op = FilterOperator::new(input, pred);

        let old_active = make_node(&[("status", serde_json::json!("active")), ("id", serde_json::json!(1))]);
        let new_deleted = make_node(&[("status", serde_json::json!("deleted")), ("id", serde_json::json!(1))]);
        let new_active = make_node(&[("status", serde_json::json!("active")), ("id", serde_json::json!(2))]);
        let old_deleted = make_node(&[("status", serde_json::json!("deleted")), ("id", serde_json::json!(2))]);

        // old matches, new doesn't -> Remove
        let result = op.push(Change::Edit {
            node: new_deleted.clone(),
            old_node: old_active.clone(),
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Remove);

        // old doesn't match, new does -> Add
        let result = op.push(Change::Edit {
            node: new_active.clone(),
            old_node: old_deleted.clone(),
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Add);

        // both match -> Edit
        let result = op.push(Change::Edit {
            node: old_active.clone(),
            old_node: old_active.clone(),
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Edit);

        // neither matches -> drop
        let result = op.push(Change::Edit {
            node: new_deleted,
            old_node: old_deleted,
        });
        assert_eq!(result.len(), 0);
    }
}
