use std::collections::HashMap;

use crate::operator::Operator;
use crate::types::{compare_rows, Change, FetchRequest, Node, Row, SortDirection, SortSpec, Start};

struct TakeState {
    size: usize,
    bound: Option<Row>,
}

pub struct TakeOperator {
    input: Box<dyn Operator>,
    limit: usize,
    sort: Vec<SortSpec>,
    partition_key: Option<Vec<String>>,
    states: HashMap<String, TakeState>,
}

impl TakeOperator {
    pub fn new(
        input: Box<dyn Operator>,
        limit: usize,
        sort: Vec<SortSpec>,
        partition_key: Option<Vec<String>>,
    ) -> Self {
        Self {
            input,
            limit,
            sort,
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

    fn row_is_within_window(&self, row: &Row, bound: &Row) -> bool {
        compare_rows(row, bound, &self.sort) != std::cmp::Ordering::Greater
    }
}

impl Operator for TakeOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let part_key = req
            .constraint
            .as_ref()
            .map(|c| {
                let mut m = Row::new();
                m.insert(c.key.clone(), c.value.clone());
                self.partition_key_str(&m)
            })
            .unwrap_or_default();

        let all_nodes = self.input.fetch(req);
        let mut result = Vec::new();
        let mut count = 0;

        for node in all_nodes {
            if count >= self.limit {
                break;
            }
            result.push(node);
            count += 1;
        }

        let bound = result.last().map(|n| n.row.clone());
        self.states.insert(
            part_key,
            TakeState {
                size: result.len(),
                bound,
            },
        );

        result
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        let part_key = self.partition_key_str(&change.node().row);
        let state = match self.states.get(&part_key) {
            Some(s) => s,
            None => return vec![],
        };
        let current_size = state.size;
        let current_bound = state.bound.clone();

        match &change {
            Change::Add(node) => {
                if current_size < self.limit {
                    let new_bound = match &current_bound {
                        Some(b)
                            if compare_rows(&node.row, b, &self.sort)
                                != std::cmp::Ordering::Greater =>
                        {
                            Some(b.clone())
                        }
                        _ => Some(node.row.clone()),
                    };
                    self.states.insert(
                        part_key,
                        TakeState {
                            size: current_size + 1,
                            bound: new_bound,
                        },
                    );
                    vec![change]
                } else {
                    let bound = match &current_bound {
                        Some(b) => b,
                        None => return vec![],
                    };
                    if compare_rows(&node.row, bound, &self.sort) != std::cmp::Ordering::Less {
                        return vec![];
                    }
                    let displaced_node = Node {
                        row: bound.clone(),
                        relationships: HashMap::new(),
                    };
                    // Re-fetch to find new bound after displacement
                    let fetched = self.input.fetch(&FetchRequest {
                        constraint: req_constraint_for_partition(&self.partition_key, &node.row),
                        start: None,
                        reverse: false,
                    });
                    let taken: Vec<Node> = fetched.into_iter().take(self.limit).collect();
                    let actual_bound = taken.last().map(|n| n.row.clone());
                    self.states.insert(
                        part_key,
                        TakeState {
                            size: taken.len(),
                            bound: actual_bound,
                        },
                    );
                    vec![Change::Remove(displaced_node), change]
                }
            }
            Change::Remove(node) => {
                let bound = match &current_bound {
                    Some(b) => b,
                    None => return vec![],
                };
                if !self.row_is_within_window(&node.row, bound) {
                    return vec![];
                }
                let row_for_constraint = node.row.clone();
                // Try to fetch a replacement from after the bound
                let fetched = self.input.fetch(&FetchRequest {
                    constraint: req_constraint_for_partition(&self.partition_key, &row_for_constraint),
                    start: Some(Start {
                        row: bound.clone(),
                        basis: "after".to_string(),
                    }),
                    reverse: false,
                });
                let replacement = fetched.into_iter().next();
                let mut result = vec![change];
                if let Some(rep) = replacement {
                    let new_bound = Some(rep.row.clone());
                    self.states.insert(
                        part_key,
                        TakeState {
                            size: current_size,
                            bound: new_bound,
                        },
                    );
                    result.push(Change::Add(rep));
                } else {
                    // No replacement: shrink window
                    let fetched = self.input.fetch(&FetchRequest {
                        constraint: req_constraint_for_partition(
                            &self.partition_key,
                            &row_for_constraint,
                        ),
                        start: None,
                        reverse: false,
                    });
                    let taken: Vec<Node> = fetched.into_iter().take(self.limit).collect();
                    let new_bound = taken.last().map(|n| n.row.clone());
                    self.states.insert(
                        part_key,
                        TakeState {
                            size: taken.len(),
                            bound: new_bound,
                        },
                    );
                }
                result
            }
            Change::Edit { node, old_node } => {
                let bound = match &current_bound {
                    Some(b) => b,
                    None => return vec![],
                };
                let old_in = self.row_is_within_window(&old_node.row, bound);
                let new_in = self.row_is_within_window(&node.row, bound);
                if old_in && new_in {
                    vec![change]
                } else if old_in {
                    // Old in, new out: remove old, maybe add replacement
                    let mut result = vec![Change::Remove(old_node.clone())];
                    let fetched = self.input.fetch(&FetchRequest {
                        constraint: req_constraint_for_partition(
                            &self.partition_key,
                            &old_node.row,
                        ),
                        start: Some(Start {
                            row: bound.clone(),
                            basis: "after".to_string(),
                        }),
                        reverse: false,
                    });
                    if let Some(rep) = fetched.into_iter().next() {
                        let new_bound = Some(rep.row.clone());
                        self.states.insert(
                            part_key,
                            TakeState {
                                size: current_size,
                                bound: new_bound,
                            },
                        );
                        result.push(Change::Add(rep));
                    } else {
                        self.states.insert(
                            part_key,
                            TakeState {
                                size: current_size - 1,
                                bound: current_bound,
                            },
                        );
                    }
                    result
                } else if new_in {
                    // Old out, new in: displace bound
                    let displaced = Node {
                        row: bound.clone(),
                        relationships: HashMap::new(),
                    };
                    let fetched = self.input.fetch(&FetchRequest {
                        constraint: req_constraint_for_partition(&self.partition_key, &node.row),
                        start: None,
                        reverse: false,
                    });
                    let taken: Vec<Node> = fetched.into_iter().take(self.limit).collect();
                    let new_bound = taken.last().map(|n| n.row.clone());
                    self.states.insert(
                        part_key,
                        TakeState {
                            size: taken.len(),
                            bound: new_bound,
                        },
                    );
                    vec![Change::Remove(displaced), Change::Add(node.clone())]
                } else {
                    vec![]
                }
            }
            Change::Child { node, .. } => {
                let bound = match &current_bound {
                    Some(b) => b,
                    None => return vec![],
                };
                if self.row_is_within_window(&node.row, bound) {
                    vec![change]
                } else {
                    vec![]
                }
            }
        }
    }

    fn op_type(&self) -> &'static str {
        "take"
    }
}

fn req_constraint_for_partition(
    partition_key: &Option<Vec<String>>,
    row: &Row,
) -> Option<crate::types::Constraint> {
    partition_key.as_ref().and_then(|pk| {
        pk.first().map(|k| crate::types::Constraint {
            key: k.clone(),
            value: row.get(k).cloned().unwrap_or(serde_json::Value::Null),
        })
    })
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

    fn default_sort() -> Vec<SortSpec> {
        vec![SortSpec {
            field: "id".to_string(),
            direction: SortDirection::Asc,
        }]
    }

    #[test]
    fn test_take_limits_fetch_and_tracks_bound() {
        let nodes = vec![make_node(1), make_node(2), make_node(3), make_node(4)];
        let input = Box::new(MockInput { nodes });
        let mut op = TakeOperator::new(input, 2, default_sort(), None);

        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(2));

        let state = op.states.get("").unwrap();
        assert_eq!(state.size, 2);
        assert_eq!(
            state.bound.as_ref().unwrap().get("id").unwrap(),
            &serde_json::json!(2)
        );
    }

    #[test]
    fn test_take_push_add_within_limit_propagates() {
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput {
            nodes: nodes.clone(),
        });
        let mut op = TakeOperator::new(input, 3, default_sort(), None);

        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Add(make_node(2)));
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Add(_)));

        let state = op.states.get("").unwrap();
        assert_eq!(state.size, 2);
    }

    #[test]
    fn test_take_push_add_at_limit_displaces_bound() {
        let nodes = vec![make_node(1), make_node(2), make_node(3)];
        let input = Box::new(MockInput { nodes });
        let mut op = TakeOperator::new(input, 2, default_sort(), None);

        let _ = op.fetch(&FetchRequest::default());
        assert_eq!(op.states.get("").unwrap().size, 2);

        // Push add of id=0 (before bound=2) -> displaces bound row (id=2)
        let result = op.push(Change::Add(make_node(0)));
        assert!(result.len() >= 2);
        let has_remove = result
            .iter()
            .any(|c| matches!(c, Change::Remove(n) if n.row.get("id").unwrap() == &serde_json::json!(2)));
        assert!(has_remove, "Should remove displaced bound row");
    }

    #[test]
    fn test_take_push_remove_decrements() {
        let nodes = vec![make_node(1), make_node(2), make_node(3)];
        let input = Box::new(MockInput {
            nodes: nodes.clone(),
        });
        let mut op = TakeOperator::new(input, 2, default_sort(), None);

        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Remove(make_node(1)));
        assert!(!result.is_empty());
        assert!(
            matches!(&result[0], Change::Remove(n) if n.row.get("id").unwrap() == &serde_json::json!(1))
        );
    }

    #[test]
    fn test_take_push_child_within_window() {
        let nodes = vec![make_node(1), make_node(2)];
        let input = Box::new(MockInput { nodes });
        let mut op = TakeOperator::new(input, 2, default_sort(), None);

        let _ = op.fetch(&FetchRequest::default());

        let child_change = Change::Child {
            node: make_node(1),
            child: crate::types::ChildData {
                relationship_name: "items".to_string(),
                change: Box::new(Change::Add(make_node(10))),
            },
        };
        let result = op.push(child_change);
        assert_eq!(result.len(), 1);

        let child_outside = Change::Child {
            node: make_node(99),
            child: crate::types::ChildData {
                relationship_name: "items".to_string(),
                change: Box::new(Change::Add(make_node(10))),
            },
        };
        let result = op.push(child_outside);
        assert_eq!(result.len(), 0);
    }
}
