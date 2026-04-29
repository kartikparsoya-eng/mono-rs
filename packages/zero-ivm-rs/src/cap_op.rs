use std::collections::HashMap;

use crate::operator::Operator;
use crate::types::{Change, Constraint, FetchRequest, Node, Row};

struct CapState {
    size: usize,
    pks: Vec<String>,
}

pub struct CapOperator {
    input: Box<dyn Operator>,
    limit: usize,
    primary_key: Vec<String>,
    partition_key: Option<Vec<String>>,
    states: HashMap<String, CapState>,
}

/// Matches TS getCapStateKey: JSON.stringify(['cap', ...partitionValues])
fn cap_state_key(partition_key: Option<&[String]>, row: &Row) -> String {
    let mut parts: Vec<serde_json::Value> = vec![serde_json::Value::String("cap".to_string())];
    if let Some(pk) = partition_key {
        for k in pk {
            parts.push(row.get(k).cloned().unwrap_or(serde_json::Value::Null));
        }
    }
    serde_json::to_string(&parts).unwrap_or_default()
}

/// Matches TS serializePK: JSON.stringify(primaryKey.map(k => row[k]))
fn serialize_pk(primary_key: &[String], row: &Row) -> String {
    let vals: Vec<serde_json::Value> = primary_key
        .iter()
        .map(|k| row.get(k).cloned().unwrap_or(serde_json::Value::Null))
        .collect();
    serde_json::to_string(&vals).unwrap_or_default()
}

/// Build a single-key constraint for partition-scoped fetches.
fn partition_constraint(partition_key: Option<&[String]>, row: &Row) -> Option<Constraint> {
    let pk = partition_key?;
    if pk.is_empty() {
        return None;
    }
    Some(Constraint::from_pairs(
        pk.iter().map(|k| (k.clone(), row.get(k).cloned().unwrap_or(serde_json::Value::Null)))
    ))
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
}

impl Operator for CapOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let all_nodes = self.input.fetch(req);
        let mut result = Vec::new();

        for node in all_nodes {
            let part_key = cap_state_key(self.partition_key.as_deref(), &node.row);
            let pk = serialize_pk(&self.primary_key, &node.row);

            let state = self.states.entry(part_key).or_insert_with(|| CapState {
                size: 0,
                pks: Vec::new(),
            });

            if state.size < self.limit {
                state.size += 1;
                state.pks.push(pk);
                result.push(node);
            }
        }

        result
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        match &change {
            Change::Add(node) => {
                let part_key = cap_state_key(self.partition_key.as_deref(), &node.row);
                let pk = serialize_pk(&self.primary_key, &node.row);
                let state = match self.states.get_mut(&part_key) {
                    Some(s) => s,
                    None => return vec![],
                };
                if state.size < self.limit {
                    state.size += 1;
                    state.pks.push(pk);
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Remove(node) => {
                let part_key = cap_state_key(self.partition_key.as_deref(), &node.row);
                let pk = serialize_pk(&self.primary_key, &node.row);

                // First: find and remove from state, collect what we need
                let (pk_index, pks_snapshot, new_size) = {
                    let state = match self.states.get_mut(&part_key) {
                        Some(s) => s,
                        None => return vec![],
                    };
                    let pk_index = match state.pks.iter().position(|p| p == &pk) {
                        Some(i) => i,
                        None => return vec![],
                    };
                    state.pks.remove(pk_index);
                    let new_size = state.size - 1;
                    state.size = new_size;
                    (pk_index, state.pks.clone(), new_size)
                };
                let _ = pk_index; // used above

                // Try to fetch a replacement row from input.
                // Exclude both remaining tracked PKs and the just-removed PK
                // (in production the removed row is gone from post-tx DB, but
                // during stateless replay the source may still return it).
                let mut pk_set: std::collections::HashSet<String> =
                    pks_snapshot.iter().cloned().collect();
                pk_set.insert(pk.clone());

                let fetch_constraint = partition_constraint(self.partition_key.as_deref(), &node.row);
                let replacement_nodes = self.input.fetch(&FetchRequest {
                    constraint: fetch_constraint,
                    start: None,
                    reverse: false,
                });

                let mut replacement: Option<Node> = None;
                for rn in replacement_nodes {
                    let rn_pk = serialize_pk(&self.primary_key, &rn.row);
                    if !pk_set.contains(&rn_pk) {
                        replacement = Some(rn);
                        break;
                    }
                }

                let mut result = vec![change];
                if let Some(rep) = replacement {
                    let rep_pk = serialize_pk(&self.primary_key, &rep.row);
                    let state = self.states.get_mut(&part_key).unwrap();
                    state.pks.push(rep_pk);
                    state.size = new_size + 1;
                    result.push(Change::Add(rep));
                }
                result
            }
            Change::Edit { old_node, .. } => {
                let old_part_key = cap_state_key(self.partition_key.as_deref(), &old_node.row);
                let new_part_key = cap_state_key(self.partition_key.as_deref(), &change.node().row);
                debug_assert_eq!(
                    old_part_key, new_part_key,
                    "Cap: partition key must not change on edit"
                );
                let part_key = old_part_key;
                let old_pk = serialize_pk(&self.primary_key, &old_node.row);
                let new_pk = serialize_pk(&self.primary_key, &change.node().row);
                let state = match self.states.get_mut(&part_key) {
                    Some(s) => s,
                    None => return vec![],
                };
                if let Some(idx) = state.pks.iter().position(|p| p == &old_pk) {
                    if old_pk != new_pk {
                        state.pks[idx] = new_pk;
                    }
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Child { .. } => {
                let part_key = cap_state_key(self.partition_key.as_deref(), &change.node().row);
                let pk = serialize_pk(&self.primary_key, &change.node().row);
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
    fn test_cap_add_within_and_at_limit() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Add(make_node(2)));
        assert_eq!(result.len(), 1);

        let result = op.push(Change::Add(make_node(3)));
        assert_eq!(result.len(), 0, "at limit, should drop");
    }

    #[test]
    fn test_cap_remove_untracked_drops() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Remove(make_node(99)));
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_cap_remove_fetches_replacement() {
        let nodes = vec![make_node(1), make_node(2), make_node(3)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);

        let fetched = op.fetch(&FetchRequest::default());
        assert_eq!(fetched.len(), 2);

        let result = op.push(Change::Remove(make_node(1)));
        assert_eq!(result.len(), 2, "should emit remove + replacement add");
        assert!(matches!(&result[0], Change::Remove(n) if n.row.get("id").unwrap() == &serde_json::json!(1)));
        assert!(matches!(&result[1], Change::Add(n) if n.row.get("id").unwrap() == &serde_json::json!(3)));

        let state = op.states.values().next().unwrap();
        assert_eq!(state.size, 2);
        assert_eq!(state.pks.len(), 2);
    }

    #[test]
    fn test_cap_remove_no_replacement_available() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 1, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Remove(make_node(1)));
        assert_eq!(result.len(), 1, "just the remove, no replacement");
        assert!(matches!(&result[0], Change::Remove(_)));

        let state = op.states.values().next().unwrap();
        assert_eq!(state.size, 0);
    }

    #[test]
    fn test_cap_edit_tracked_propagates() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Edit {
            node: make_node(1),
            old_node: make_node(1),
        });
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_cap_edit_untracked_drops() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Edit {
            node: make_node(99),
            old_node: make_node(99),
        });
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_cap_edit_updates_pk_in_place() {
        let nodes = vec![make_node(1), make_node(2)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Edit {
            node: make_node(10),
            old_node: make_node(1),
        });
        assert_eq!(result.len(), 1);

        let state = op.states.values().next().unwrap();
        let pk_10 = serde_json::to_string(&vec![serde_json::json!(10)]).unwrap();
        let pk_2 = serde_json::to_string(&vec![serde_json::json!(2)]).unwrap();
        assert!(state.pks.contains(&pk_10));
        assert!(state.pks.contains(&pk_2));
    }

    #[test]
    fn test_cap_child_tracked_propagates() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let child_change = Change::Child {
            node: make_node(1),
            child: crate::types::ChildData {
                relationship_name: "items".to_string(),
                change: Box::new(Change::Add(make_node(100))),
            },
        };
        assert_eq!(op.push(child_change).len(), 1);
    }

    #[test]
    fn test_cap_child_untracked_drops() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let child_change = Change::Child {
            node: make_node(99),
            child: crate::types::ChildData {
                relationship_name: "items".to_string(),
                change: Box::new(Change::Add(make_node(101))),
            },
        };
        assert_eq!(op.push(child_change).len(), 0);
    }

    #[test]
    fn test_cap_partition_key_str_matches_ts() {
        let mut row = Row::new();
        row.insert("group".to_string(), serde_json::json!("a"));
        let key = cap_state_key(Some(&["group".to_string()]), &row);
        assert_eq!(key, r#"["cap","a"]"#);
    }

    #[test]
    fn test_cap_partition_key_str_no_partition() {
        let row = Row::new();
        let key = cap_state_key(None, &row);
        assert_eq!(key, r#"["cap"]"#);
    }

    #[test]
    fn test_cap_pks_ordered() {
        let nodes = vec![make_node(3), make_node(1), make_node(2)];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(input, 3, vec!["id".to_string()], None);
        let _ = op.fetch(&FetchRequest::default());

        let state = op.states.values().next().unwrap();
        let pk_3 = serde_json::to_string(&vec![serde_json::json!(3)]).unwrap();
        let pk_1 = serde_json::to_string(&vec![serde_json::json!(1)]).unwrap();
        let pk_2 = serde_json::to_string(&vec![serde_json::json!(2)]).unwrap();
        assert_eq!(state.pks, vec![pk_3, pk_1, pk_2]);
    }

    #[test]
    fn test_cap_no_state_returns_empty() {
        let input = Box::new(MockInput { nodes: vec![] });
        let mut op = CapOperator::new(input, 2, vec!["id".to_string()], None);
        // No fetch → no state initialized
        assert_eq!(op.push(Change::Add(make_node(1))).len(), 0);
        assert_eq!(op.push(Change::Remove(make_node(1))).len(), 0);
    }

    #[test]
    fn test_partition_constraint_multi_column() {
        // c1: partition_constraint should use first column for DB constraint
        // but cap_state_key should use all columns for state keying.
        let row: Row = vec![
            ("region".to_string(), serde_json::json!("us")),
            ("tier".to_string(), serde_json::json!("gold")),
            ("id".to_string(), serde_json::json!(1)),
        ]
        .into_iter()
        .collect();
        let pk = vec!["region".to_string(), "tier".to_string()];

        // partition_constraint uses first column
        let c = partition_constraint(Some(&pk), &row).unwrap();
        assert_eq!(c.columns.get("region"), Some(&serde_json::json!("us")));
        assert_eq!(c.columns.get("tier"), Some(&serde_json::json!("gold")));

        // cap_state_key uses all columns
        let key = cap_state_key(Some(&pk), &row);
        assert!(key.contains("us"));
        assert!(key.contains("gold"));

        // Different tier → different state key
        let row2: Row = vec![
            ("region".to_string(), serde_json::json!("us")),
            ("tier".to_string(), serde_json::json!("silver")),
            ("id".to_string(), serde_json::json!(2)),
        ]
        .into_iter()
        .collect();
        let key2 = cap_state_key(Some(&pk), &row2);
        assert_ne!(key, key2);
    }

    // ===== AUDIT-03: Promoted assertion regression test =====
    // Confirms the partition-key invariant fires in release builds.
    // If anyone reverts `assert_eq!` back to `debug_assert_eq!`, this test
    // will fail under `cargo test --release`.

    fn make_node_with_region(id: i64, region: &str) -> Node {
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!(id));
        row.insert("region".to_string(), serde_json::json!(region));
        Node {
            row,
            relationships: HashMap::new(),
        }
    }

    #[test]
    #[should_panic(expected = "Cap: partition key must not change on edit")]
    fn test_cap_partition_key_change_on_edit_panics() {
        let nodes = vec![make_node_with_region(1, "us")];
        let input = Box::new(MockInput { nodes });
        let mut op = CapOperator::new(
            input,
            2,
            vec!["id".to_string()],
            Some(vec!["region".to_string()]),
        );
        // Prime state for region=us partition.
        let _ = op.fetch(&FetchRequest::default());

        // Edit moves the row from region=us to region=eu — partition key
        // changed → must panic.
        let _ = op.push(Change::Edit {
            node: make_node_with_region(1, "eu"),
            old_node: make_node_with_region(1, "us"),
        });
    }
}
