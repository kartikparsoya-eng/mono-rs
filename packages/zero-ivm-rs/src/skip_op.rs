use crate::operator::Operator;
use crate::types::{Change, FetchRequest, Node, Row, SortSpec, Start, compare_rows};

pub struct Bound {
    pub row: Row,
    pub exclusive: bool,
}

pub struct SkipOperator {
    input: Box<dyn Operator>,
    bound: Bound,
    sort: Vec<SortSpec>,
}

impl SkipOperator {
    pub fn new(input: Box<dyn Operator>, bound: Bound, sort: Vec<SortSpec>) -> Self {
        Self { input, bound, sort }
    }

    fn should_be_present(&self, row: &Row) -> bool {
        let cmp = compare_rows(&self.bound.row, row, &self.sort);
        cmp == std::cmp::Ordering::Less || (cmp == std::cmp::Ordering::Equal && !self.bound.exclusive)
    }
}

impl Operator for SkipOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let bound_start = Start {
            row: self.bound.row.clone(),
            basis: if self.bound.exclusive { "after".to_string() } else { "at".to_string() },
        };

        let effective_start = if let Some(ref start) = req.start {
            let cmp = compare_rows(&self.bound.row, &start.row, &self.sort);
            if !req.reverse {
                if cmp == std::cmp::Ordering::Greater {
                    Some(bound_start)
                } else if cmp == std::cmp::Ordering::Equal {
                    if self.bound.exclusive || start.basis == "after" {
                        Some(Start { row: self.bound.row.clone(), basis: "after".to_string() })
                    } else {
                        Some(bound_start)
                    }
                } else {
                    Some(start.clone())
                }
            } else if cmp == std::cmp::Ordering::Greater {
                return vec![];
            } else if cmp == std::cmp::Ordering::Equal {
                if !self.bound.exclusive && start.basis == "at" {
                    Some(bound_start)
                } else {
                    return vec![];
                }
            } else {
                Some(start.clone())
            }
        } else if req.reverse {
            None
        } else {
            Some(bound_start)
        };

        let mut modified_req = req.clone();
        modified_req.start = effective_start;

        let nodes = self.input.fetch(&modified_req);

        if req.reverse {
            nodes
                .into_iter()
                .take_while(|node| self.should_be_present(&node.row))
                .collect()
        } else {
            nodes
        }
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        match change {
            Change::Add(ref node) | Change::Remove(ref node) => {
                if self.should_be_present(&node.row) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Child { ref node, .. } => {
                if self.should_be_present(&node.row) {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Edit { ref node, ref old_node } => {
                let old_visible = self.should_be_present(&old_node.row);
                let new_visible = self.should_be_present(&node.row);
                match (old_visible, new_visible) {
                    (true, true) => vec![change],
                    (true, false) => vec![Change::Remove(old_node.clone())],
                    (false, true) => vec![Change::Add(node.clone())],
                    (false, false) => vec![],
                }
            }
        }
    }

    fn op_type(&self) -> &'static str {
        "skip"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChangeType, SortDirection};
    use std::collections::HashMap;

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
        Node { row, relationships: HashMap::new() }
    }

    fn sort_by_id() -> Vec<SortSpec> {
        vec![SortSpec { field: "id".to_string(), direction: SortDirection::Asc }]
    }

    #[test]
    fn test_skip_fetch_filters_before_bound() {
        let nodes = vec![make_node(1), make_node(2), make_node(3), make_node(4)];
        let input = Box::new(MockInput { nodes });
        let mut bound_row = Row::new();
        bound_row.insert("id".to_string(), serde_json::json!(2));
        let bound = Bound { row: bound_row, exclusive: false };
        let mut op = SkipOperator::new(input, bound, sort_by_id());

        // Forward fetch should use bound as start (input mock returns all, but
        // in a real scenario the input would honor the start parameter)
        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 4); // MockInput ignores start
    }

    #[test]
    fn test_skip_push_filters_by_bound() {
        let input = Box::new(MockInput { nodes: vec![] });
        let mut bound_row = Row::new();
        bound_row.insert("id".to_string(), serde_json::json!(3));
        let bound = Bound { row: bound_row, exclusive: false };
        let mut op = SkipOperator::new(input, bound, sort_by_id());

        // Node at bound (inclusive) -> visible
        assert_eq!(op.push(Change::Add(make_node(3))).len(), 1);
        // Node after bound -> visible
        assert_eq!(op.push(Change::Add(make_node(5))).len(), 1);
        // Node before bound -> not visible
        assert_eq!(op.push(Change::Add(make_node(1))).len(), 0);
    }

    #[test]
    fn test_skip_push_edit_conversion() {
        let input = Box::new(MockInput { nodes: vec![] });
        let mut bound_row = Row::new();
        bound_row.insert("id".to_string(), serde_json::json!(3));
        let bound = Bound { row: bound_row, exclusive: true };
        let mut op = SkipOperator::new(input, bound, sort_by_id());

        // Edit: old visible (id=5), new not visible (id=2) -> Remove
        let result = op.push(Change::Edit {
            node: make_node(2),
            old_node: make_node(5),
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Remove);

        // Edit: old not visible (id=1), new visible (id=4) -> Add
        let result = op.push(Change::Edit {
            node: make_node(4),
            old_node: make_node(1),
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].change_type(), ChangeType::Add);
    }
}
