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

impl CapOperator {
    pub fn new(
        input: Box<dyn Operator>,
        limit: usize,
        primary_key: Vec<String>,
        partition_key: Option<Vec<String>>,
    ) -> Self {
        Self {
            input,
            limit,
            primary_key,
            partition_key,
            states: HashMap::new(),
        }
    }

    fn partition_key_str(&self, row: &Row) -> String {
        match &self.partition_key {
            Some(pk) => {
                let vals: Vec<serde_json::Value> = pk
                    .iter()
                    .map(|k| row.get(k).cloned().unwrap_or(serde_json::Value::Null))
                    .collect();
                serde_json::to_string(&vals).unwrap_or_default()
            }
            None => String::new(),
        }
    }

    fn serialize_pk(&self, row: &Row) -> String {
        let vals: Vec<serde_json::Value> = self
            .primary_key
            .iter()
            .map(|k| row.get(k).cloned().unwrap_or(serde_json::Value::Null))
            .collect();
        serde_json::to_string(&vals).unwrap_or_default()
    }
}

impl Operator for CapOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let all_nodes = self.input.fetch(req);
        let mut result = Vec::new();

        for node in all_nodes {
            let part_key = self.partition_key_str(&node.row);
            let pk = self.serialize_pk(&node.row);

            let state = self.states.entry(part_key).or_insert_with(|| CapState {
                size: 0,
                pks: HashSet::new(),
            });

            if state.size < self.limit {
                state.size += 1;
                state.pks.insert(pk);
                result.push(node);
            }
        }

        result
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        let part_key = self.partition_key_str(&change.node().row);
        let pk = self.serialize_pk(&change.node().row);

        match &change {
            Change::Add(_) => {
                let state = match self.states.get_mut(&part_key) {
                    Some(s) => s,
                    None => return vec![],
                };
                if state.size < self.limit {
                    state.size += 1;
                    state.pks.insert(pk);
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Remove(_) => {
                let state = match self.states.get_mut(&part_key) {
                    Some(s) => s,
                    None => return vec![],
                };
                if state.pks.remove(&pk) {
                    state.size -= 1;
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Edit { old_node, .. } => {
                let old_pk = self.serialize_pk(&old_node.row);
                let state = match self.states.get_mut(&part_key) {
                    Some(s) => s,
                    None => return vec![],
                };
                if state.pks.contains(&old_pk) {
                    if old_pk != pk {
                        state.pks.remove(&old_pk);
                        state.pks.insert(pk);
                    }
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Child { .. } => {
                let state = match self.states.get(&part_key) {
                    Some(s) => s,
                    None => return vec![],
                };
                if state.pks.contains(&pk) {
                    vec![change]
                } else {
                    vec![]
                }
            }
        }
    }

    fn op_type(&self) -> &'static str {
        "cap"
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

    #[test]
    fn test_cap_limits_fetch() {
        let nodes = vec![make_node(1), make_node(2), make_node(3), make_node(4)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);

        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(2));
    }

    #[test]
    fn test_cap_tracks_push_changes() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);

        // Initialize state via fetch
        let _ = op.fetch(&FetchRequest::default());

        // Add within limit
        let result = op.push(Change::Add(make_node(2)));
        assert_eq!(result.len(), 1);

        // Add at limit -> drop
        let result = op.push(Change::Add(make_node(3)));
        assert_eq!(result.len(), 0);

        // Remove tracked PK
        let result = op.push(Change::Remove(make_node(1)));
        assert_eq!(result.len(), 1);

        // Remove untracked PK -> drop
        let result = op.push(Change::Remove(make_node(99)));
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_cap_edit_tracked_pk() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);

        let _ = op.fetch(&FetchRequest::default());

        // Edit of tracked PK -> propagate
        let result = op.push(Change::Edit {
            node: make_node(1),
            old_node: make_node(1),
        });
        assert_eq!(result.len(), 1);

        // Edit of untracked PK -> drop
        let result = op.push(Change::Edit {
            node: make_node(99),
            old_node: make_node(99),
        });
        assert_eq!(result.len(), 0);
    }
}
