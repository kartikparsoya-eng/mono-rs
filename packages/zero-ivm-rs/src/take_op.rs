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
    max_bound: Option<Row>,
    /// During certain push operations (split edit → remove then add), we
    /// temporarily hide a row from fetch so downstream operators don't see it.
    /// Matches TS `#rowHiddenFromFetch`.
    row_hidden_from_fetch: Option<Row>,
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
            max_bound: None,
            row_hidden_from_fetch: None,
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

    fn take_state_key(&self, row: &Row) -> String {
        // Matches TS getTakeStateKey
        match &self.partition_key {
            Some(pk) => {
                let mut vals: Vec<serde_json::Value> =
                    vec![serde_json::Value::String("take".to_string())];
                for k in pk {
                    vals.push(row.get(k).cloned().unwrap_or(serde_json::Value::Null));
                }
                serde_json::to_string(&vals).unwrap_or_default()
            }
            None => "[\"take\"]".to_string(),
        }
    }

    fn compare_rows(&self, a: &Row, b: &Row) -> std::cmp::Ordering {
        compare_rows(a, b, &self.sort)
    }

    fn set_take_state(
        &mut self,
        key: String,
        size: usize,
        bound: Option<Row>,
    ) {
        if let Some(ref b) = bound {
            if self.max_bound.is_none()
                || self.compare_rows(b, self.max_bound.as_ref().unwrap())
                    == std::cmp::Ordering::Greater
            {
                self.max_bound = Some(b.clone());
            }
        }
        self.states.insert(key, TakeState { size, bound });
    }

    fn constraint_for_row(&self, row: &Row) -> Option<crate::types::Constraint> {
        self.partition_key.as_ref().and_then(|pk| {
            if pk.is_empty() {
                None
            } else {
                Some(crate::types::Constraint::from_pairs(
                    pk.iter().map(|k| (k.clone(), row.get(k).cloned().unwrap_or(serde_json::Value::Null)))
                ))
            }
        })
    }

    /// Push an Edit change through the Take operator.
    /// Matches TS `#pushEditChange` exactly — 6 cases based on how old/new
    /// rows compare to the current bound.
    fn push_edit(&mut self, node: Node, old_node: Node) -> Vec<Change> {
        let key = self.take_state_key(&old_node.row);
        let state = match self.states.get(&key) {
            Some(s) => s,
            None => return vec![],
        };
        let bound = match &state.bound {
            Some(b) => b.clone(),
            None => return vec![],
        };
        let current_size = state.size;

        let old_cmp = self.compare_rows(&old_node.row, &bound);
        let new_cmp = self.compare_rows(&node.row, &bound);

        use std::cmp::Ordering::*;

        // Case 1: The bound row was changed (oldCmp === 0)
        if old_cmp == Equal {
            if new_cmp == Equal {
                // New row is still the bound — forward edit, no state change
                return vec![Change::Edit {
                    node,
                    old_node,
                }];
            }

            if new_cmp == Less {
                // New row moved earlier than bound.
                if self.limit == 1 {
                    // With limit=1, new row becomes the new bound
                    self.set_take_state(key, current_size, Some(node.row.clone()));
                    return vec![Change::Edit { node, old_node }];
                }

                // Find the row just before the old bound to determine new bound
                let constraint = self.constraint_for_row(&old_node.row);
                let fetched = self.input.fetch(&FetchRequest {
                    constraint,
                    start: Some(Start {
                        row: bound.clone(),
                        basis: "after".to_string(),
                    }),
                    reverse: true,
                });
                let before_bound_node = fetched.into_iter().next();
                assert!(
                    before_bound_node.is_some(),
                    "Take: beforeBoundNode must be found during fetch"
                );
                let before_bound_row = before_bound_node.unwrap().row;
                self.set_take_state(key, current_size, Some(before_bound_row));
                return vec![Change::Edit { node, old_node }];
            }

            // newCmp > 0: bound row moved past the bound position
            assert!(new_cmp == Greater);
            let constraint = self.constraint_for_row(&old_node.row);
            // Find the first item at the old bound position — this becomes new bound
            let fetched = self.input.fetch(&FetchRequest {
                constraint,
                start: Some(Start {
                    row: bound.clone(),
                    basis: "at".to_string(),
                }),
                reverse: false,
            });
            let new_bound_node = fetched.into_iter().next();
            assert!(
                new_bound_node.is_some(),
                "Take: newBoundNode must be found during fetch"
            );
            let new_bound_node = new_bound_node.unwrap();

            // If the row at bound position IS the new row, use edit
            if self.compare_rows(&new_bound_node.row, &node.row) == Equal {
                self.set_take_state(key, current_size, Some(node.row.clone()));
                return vec![Change::Edit { node, old_node }];
            }

            // New row is now outside bounds — remove old, add replacement
            self.set_take_state(key, current_size, Some(new_bound_node.row.clone()));
            // TS uses pushWithRowHiddenFromFetch here
            self.row_hidden_from_fetch = Some(new_bound_node.row.clone());
            let remove = Change::Remove(old_node);
            self.row_hidden_from_fetch = None;
            return vec![remove, Change::Add(new_bound_node)];
        }

        // Case 2: Old row was outside window (oldCmp > 0)
        if old_cmp == Greater {
            // Both outside
            if new_cmp == Greater || new_cmp == Equal {
                // TS: assert(newCmp !== 0, 'Invalid state. Row has duplicate primary key')
                debug_assert!(new_cmp != Equal, "Invalid state. Row has duplicate primary key");
                return vec![];
            }

            // Old outside, new inside — displace old bound
            assert!(new_cmp == Less);
            let constraint = self.constraint_for_row(&old_node.row);
            let fetched = self.input.fetch(&FetchRequest {
                constraint,
                start: Some(Start {
                    row: bound.clone(),
                    basis: "at".to_string(),
                }),
                reverse: true,
            });
            let mut iter = fetched.into_iter();
            let old_bound_node = iter.next();
            let new_bound_node = iter.next();
            assert!(
                old_bound_node.is_some(),
                "Take: oldBoundNode must be found during fetch"
            );
            assert!(
                new_bound_node.is_some(),
                "Take: newBoundNode must be found during fetch"
            );
            let old_bound_node = old_bound_node.unwrap();
            let new_bound_row = new_bound_node.unwrap().row;

            // Remove before add to maintain invariant: output size <= limit
            self.set_take_state(key, current_size, Some(new_bound_row));
            self.row_hidden_from_fetch = Some(node.row.clone());
            let remove = Change::Remove(old_bound_node);
            self.row_hidden_from_fetch = None;
            return vec![remove, Change::Add(node)];
        }

        // Case 3: Old row was inside window but not bound (oldCmp < 0)
        assert!(old_cmp == Less);

        // Both inside (not at bound)
        if new_cmp == Less {
            return vec![Change::Edit { node, old_node }];
        }

        // TS: assert(newCmp !== 0, 'Invalid state. Row has duplicate primary key')
        debug_assert!(new_cmp != Equal, "Invalid state. Row has duplicate primary key");

        // Old inside, new outside (newCmp > 0)
        assert!(new_cmp == Greater);

        // Find the row after the bound — use that or the new row as new bound
        let constraint = self.constraint_for_row(&old_node.row);
        let fetched = self.input.fetch(&FetchRequest {
            constraint,
            start: Some(Start {
                row: bound.clone(),
                basis: "after".to_string(),
            }),
            reverse: false,
        });
        let after_bound_node = fetched.into_iter().next();
        assert!(
            after_bound_node.is_some(),
            "Take: afterBoundNode must be found during fetch"
        );
        let after_bound_node = after_bound_node.unwrap();

        // If the row after bound IS the new row, use edit
        if self.compare_rows(&after_bound_node.row, &node.row) == Equal {
            self.set_take_state(key, current_size, Some(node.row.clone()));
            return vec![Change::Edit { node, old_node }];
        }

        // Otherwise: remove old, add the after-bound row as replacement
        let remove = Change::Remove(old_node);
        self.set_take_state(key, current_size, Some(after_bound_node.row.clone()));
        vec![remove, Change::Add(after_bound_node)]
    }
}

impl Operator for TakeOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        // Determine if we have a matching partition state
        let part_key = if self.partition_key.is_some() {
            if let Some(c) = &req.constraint {
                let m: Row = c.columns.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                self.take_state_key(&m)
            } else {
                // No constraint but we have partition key — use maxBound
                let max_bound = match &self.max_bound {
                    Some(b) => b.clone(),
                    None => return vec![],
                };
                // Fetch from input, filter by per-partition bounds up to maxBound
                let all_nodes = self.input.fetch(req);
                let mut result = Vec::new();
                for node in all_nodes {
                    if self.compare_rows(&node.row, &max_bound) == std::cmp::Ordering::Greater {
                        break;
                    }
                    let pk = self.take_state_key(&node.row);
                    if let Some(state) = self.states.get(&pk) {
                        if let Some(ref b) = state.bound {
                            if self.compare_rows(b, &node.row) != std::cmp::Ordering::Less {
                                if let Some(ref hidden) = self.row_hidden_from_fetch {
                                    if self.compare_rows(hidden, &node.row) == std::cmp::Ordering::Equal {
                                        continue;
                                    }
                                }
                                result.push(node);
                            }
                        }
                    }
                }
                return result;
            }
        } else if let Some(c) = &req.constraint {
            // No explicit partition_key, but a constraint is present (e.g. child
            // Take inside a Join — each parent passes a different constraint).
            // Derive a per-constraint state key so each parent gets its own
            // take state instead of sharing a single global bound.
            let mut vals: Vec<serde_json::Value> = vec![serde_json::Value::String("take".to_string())];
            let mut keys: Vec<&String> = c.columns.keys().collect();
            keys.sort();
            for k in keys {
                vals.push(serde_json::Value::String(k.clone()));
                vals.push(c.columns[k].clone());
            }
            serde_json::to_string(&vals).unwrap_or_default()
        } else {
            self.take_state_key(&Row::new())
        };

        // Check if we already have state for this partition
        if let Some(state) = self.states.get(&part_key) {
            let bound = match &state.bound {
                Some(b) => b.clone(),
                None => return vec![],
            };
            let all_nodes = self.input.fetch(req);
            let mut result = Vec::new();
            for node in all_nodes {
                let cmp = self.compare_rows(&bound, &node.row);
                if cmp == std::cmp::Ordering::Less {
                    break;
                }
                if let Some(ref hidden) = self.row_hidden_from_fetch {
                    if self.compare_rows(hidden, &node.row) == std::cmp::Ordering::Equal {
                        continue;
                    }
                }
                result.push(node);
            }
            return result;
        }

        // Initial fetch — no state yet
        if self.limit == 0 {
            self.set_take_state(part_key, 0, None);
            return vec![];
        }

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
        self.set_take_state(part_key, result.len(), bound);

        result
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        // Edit has its own handler matching TS #pushEditChange
        if let Change::Edit { ref node, ref old_node } = change {
            return self.push_edit(node.clone(), old_node.clone());
        }

        let key = self.take_state_key(&change.node().row);
        let (current_size, current_bound) = match self.states.get(&key) {
            Some(s) => (s.size, s.bound.clone()),
            None => return vec![],
        };

        match &change {
            Change::Add(node) => {
                if current_size < self.limit {
                    let new_bound = if current_bound.is_none()
                        || self.compare_rows(&node.row, current_bound.as_ref().unwrap())
                            == std::cmp::Ordering::Greater
                    {
                        Some(node.row.clone())
                    } else {
                        current_bound
                    };
                    self.set_take_state(key, current_size + 1, new_bound);
                    vec![change]
                } else {
                    // size === limit
                    let bound = match &current_bound {
                        Some(b) => b.clone(),
                        None => return vec![],
                    };
                    if self.compare_rows(&node.row, &bound) != std::cmp::Ordering::Less {
                        return vec![];
                    }
                    // added row < bound — displace bound row
                    let constraint = self.constraint_for_row(&node.row);
                    let (bound_node, before_bound_node) = if self.limit == 1 {
                        let fetched = self.input.fetch(&FetchRequest {
                            constraint,
                            start: Some(Start {
                                row: bound.clone(),
                                basis: "at".to_string(),
                            }),
                            reverse: false,
                        });
                        (fetched.into_iter().next(), None)
                    } else {
                        let fetched = self.input.fetch(&FetchRequest {
                            constraint,
                            start: Some(Start {
                                row: bound.clone(),
                                basis: "at".to_string(),
                            }),
                            reverse: true,
                        });
                        let mut iter = fetched.into_iter();
                        let first = iter.next(); // bound node
                        let second = iter.next(); // before bound node
                        (first, second)
                    };

                    assert!(
                        bound_node.is_some(),
                        "Take: boundNode must be found during fetch"
                    );
                    let bound_node = bound_node.unwrap();
                    let remove_change = Change::Remove(bound_node);

                    // New bound = max(addedRow, beforeBoundNode) if beforeBoundNode exists
                    let new_bound = match before_bound_node {
                        Some(ref bbn)
                            if self.compare_rows(&node.row, &bbn.row)
                                != std::cmp::Ordering::Greater =>
                        {
                            bbn.row.clone()
                        }
                        _ => node.row.clone(),
                    };

                    // Remove before add to maintain invariant: output size <= limit
                    self.set_take_state(key, current_size, Some(new_bound));
                    self.row_hidden_from_fetch = Some(node.row.clone());
                    let remove = remove_change;
                    self.row_hidden_from_fetch = None;
                    vec![remove, change]
                }
            }
            Change::Remove(node) => {
                let bound = match &current_bound {
                    Some(b) => b.clone(),
                    None => return vec![],
                };
                let comp_to_bound = self.compare_rows(&node.row, &bound);
                if comp_to_bound == std::cmp::Ordering::Greater {
                    // change is after bound
                    return vec![];
                }

                let constraint = self.constraint_for_row(&node.row);

                // Try to find a row just after the current bound (reverse search)
                let fetched = self.input.fetch(&FetchRequest {
                    constraint: constraint.clone(),
                    start: Some(Start {
                        row: bound.clone(),
                        basis: "after".to_string(),
                    }),
                    reverse: true,
                });
                let before_bound_node = fetched.into_iter().next();

                // Determine new bound
                let mut new_bound: Option<(Node, bool)> = None;
                if let Some(ref bbn) = before_bound_node {
                    let push = self.compare_rows(&bbn.row, &bound) == std::cmp::Ordering::Greater;
                    new_bound = Some((bbn.clone(), push));
                }

                if new_bound.as_ref().map_or(true, |(_, push)| !push) {
                    // Search forward from bound for a replacement
                    let fetched = self.input.fetch(&FetchRequest {
                        constraint,
                        start: Some(Start {
                            row: bound.clone(),
                            basis: "at".to_string(),
                        }),
                        reverse: false,
                    });
                    for fnode in fetched {
                        let push =
                            self.compare_rows(&fnode.row, &bound) == std::cmp::Ordering::Greater;
                        new_bound = Some((fnode, push));
                        if push {
                            break;
                        }
                    }
                }

                if let Some((ref nb, true)) = new_bound {
                    // Found a replacement to push
                    let mut result = vec![change];
                    self.set_take_state(key, current_size, Some(nb.row.clone()));
                    result.push(Change::Add(nb.clone()));
                    return result;
                }

                // No replacement to push — shrink window
                self.set_take_state(
                    key,
                    current_size - 1,
                    new_bound.map(|(nb, _)| nb.row),
                );
                vec![change]
            }
            Change::Child { node, .. } => {
                let bound = match &current_bound {
                    Some(b) => b,
                    None => return vec![],
                };
                if self.compare_rows(&node.row, bound) != std::cmp::Ordering::Greater {
                    vec![change]
                } else {
                    vec![]
                }
            }
            Change::Edit { .. } => {
                unreachable!("Edit handled above")
            }
        }
    }

    fn op_type(&self) -> &'static str {
        "take"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockInput {
        nodes: Arc<Mutex<Vec<Node>>>,
    }

    impl MockInput {
        fn new(nodes: Vec<Node>) -> Self {
            Self {
                nodes: Arc::new(Mutex::new(nodes)),
            }
        }

        fn data(&self) -> Arc<Mutex<Vec<Node>>> {
            self.nodes.clone()
        }
    }

    impl Operator for MockInput {
        fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
            let mut result = self.nodes.lock().unwrap().clone();

            // Handle constraint filtering
            if let Some(ref c) = req.constraint {
                result.retain(|n| {
                    c.columns.iter().all(|(k, v)| n.row.get(k).cloned().unwrap_or(serde_json::Value::Null) == *v)
                });
            }

            // Handle start + basis
            if let Some(ref start) = req.start {
                // Sort first so we can filter by position
                let sort = vec![SortSpec {
                    field: "id".to_string(),
                    direction: SortDirection::Asc,
                }];
                result.sort_by(|a, b| compare_rows(&a.row, &b.row, &sort));

                if req.reverse {
                    result.reverse();
                }

                if start.basis == "after" {
                    if req.reverse {
                        // "after" + reverse: keep rows before the start row
                        result.retain(|n| {
                            compare_rows(&n.row, &start.row, &sort)
                                == std::cmp::Ordering::Less
                        });
                    } else {
                        // "after" + forward: keep rows after the start row
                        result.retain(|n| {
                            compare_rows(&n.row, &start.row, &sort)
                                == std::cmp::Ordering::Greater
                        });
                    }
                } else if start.basis == "at" {
                    if req.reverse {
                        // "at" + reverse: keep rows <= start row, in reverse order
                        result.retain(|n| {
                            compare_rows(&n.row, &start.row, &sort)
                                != std::cmp::Ordering::Greater
                        });
                    } else {
                        // "at" + forward: keep rows >= start row
                        result.retain(|n| {
                            compare_rows(&n.row, &start.row, &sort)
                                != std::cmp::Ordering::Less
                        });
                    }
                }
            }

            result
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

    fn make_take(nodes: Vec<Node>, limit: usize) -> (TakeOperator, Arc<Mutex<Vec<Node>>>) {
        let mock = MockInput::new(nodes);
        let data = mock.data();
        let op = TakeOperator::new(Box::new(mock), limit, default_sort(), None);
        (op, data)
    }

    // ===== FETCH TESTS =====

    #[test]
    fn test_fetch_limits_and_tracks_bound() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3), make_node(4)], 2);
        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row["id"], serde_json::json!(1));
        assert_eq!(result[1].row["id"], serde_json::json!(2));

        let key = op.take_state_key(&Row::new());
        let state = op.states.get(&key).unwrap();
        assert_eq!(state.size, 2);
        assert_eq!(state.bound.as_ref().unwrap()["id"], serde_json::json!(2));
    }

    #[test]
    fn test_fetch_sets_max_bound() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());
        assert_eq!(op.max_bound.as_ref().unwrap()["id"], serde_json::json!(2));
    }

    #[test]
    fn test_fetch_subsequent_uses_bound() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());
        // Second fetch should use saved bound, not re-hydrate
        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
    }

    // ===== ADD TESTS =====

    #[test]
    fn test_add_below_limit_propagates() {
        let (mut op, _data) = make_take(vec![make_node(1)], 3);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Add(make_node(2)));
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Add(n) if n.row["id"] == serde_json::json!(2)));

        let key = op.take_state_key(&Row::new());
        assert_eq!(op.states.get(&key).unwrap().size, 2);
    }

    #[test]
    fn test_add_at_limit_outside_bound_ignored() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());

        // Add id=5, which is > bound(2) → ignored
        let result = op.push(Change::Add(make_node(5)));
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_add_at_limit_displaces_bound() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());

        // Add id=0 (< bound=2) → displace bound
        let result = op.push(Change::Add(make_node(0)));
        assert!(result.len() >= 2);
        assert!(matches!(&result[0], Change::Remove(n) if n.row["id"] == serde_json::json!(2)));
        assert!(matches!(&result[1], Change::Add(n) if n.row["id"] == serde_json::json!(0)));
    }

    // ===== REMOVE TESTS =====

    #[test]
    fn test_remove_outside_bound_ignored() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Remove(make_node(5)));
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_remove_within_window_with_replacement() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Remove(make_node(1)));
        assert!(!result.is_empty());
        assert!(matches!(&result[0], Change::Remove(n) if n.row["id"] == serde_json::json!(1)));
        // Should have a replacement add from row 3
        if result.len() > 1 {
            assert!(matches!(&result[1], Change::Add(n) if n.row["id"] == serde_json::json!(3)));
        }
    }

    #[test]
    fn test_remove_shrinks_when_no_replacement() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2)], 2);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Remove(make_node(1)));
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Remove(n) if n.row["id"] == serde_json::json!(1)));

        let key = op.take_state_key(&Row::new());
        assert_eq!(op.states.get(&key).unwrap().size, 1);
    }

    // ===== EDIT TESTS (6 cases) =====

    #[test]
    fn test_edit_bound_to_bound_forwards() {
        // Case: oldCmp=0, newCmp=0 — bound row edited but stays at same position
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2)], 2);
        let _ = op.fetch(&FetchRequest::default());

        // Edit bound row (id=2), change non-sort field (stays at same position)
        let mut new_row = Row::new();
        new_row.insert("id".to_string(), serde_json::json!(2));
        new_row.insert("name".to_string(), serde_json::json!("changed"));
        let new_node = Node {
            row: new_row,
            relationships: HashMap::new(),
        };

        let result = op.push(Change::Edit {
            node: new_node.clone(),
            old_node: make_node(2),
        });
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Edit { .. }));
    }

    #[test]
    fn test_edit_bound_moves_earlier() {
        // Case: oldCmp=0, newCmp<0 — bound row edited to sort earlier
        // Rows: [1, 3] with limit 2, bound=3
        // Edit id=3 → id=2 (moves earlier, still in window, need new bound)
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());
        // bound is id=2

        // Edit bound (id=2) to id=0 — moves before id=1
        let result = op.push(Change::Edit {
            node: make_node(0),
            old_node: make_node(2),
        });
        // Should forward edit, and update bound to beforeBoundNode (id=1)
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Edit { .. }));

        let key = op.take_state_key(&Row::new());
        let state = op.states.get(&key).unwrap();
        // New bound should be id=1 (the row just before old bound position)
        assert_eq!(state.bound.as_ref().unwrap()["id"], serde_json::json!(1));
    }

    #[test]
    fn test_edit_bound_moves_past_bound() {
        // Case: oldCmp=0, newCmp>0 — bound row edited to sort after old bound
        // Rows: [1, 2, 3] with limit 2, bound=2
        // Edit id=2 → id=4 (moves past bound)
        let (mut op, data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());

        // Simulate source applying edit: remove id=2, add id=4
        {
            let mut d = data.lock().unwrap();
            d.retain(|n| n.row["id"] != serde_json::json!(2));
            d.push(make_node(4));
        }

        let result = op.push(Change::Edit {
            node: make_node(4),
            old_node: make_node(2),
        });
        // Fetching at old bound position now finds id=3 (id=2 is gone).
        // id=3 != id=4 so: remove(old_node=2), add(newBoundNode=3)
        assert!(result.len() >= 2);
        assert!(matches!(&result[0], Change::Remove(n) if n.row["id"] == serde_json::json!(2)));
        assert!(matches!(&result[1], Change::Add(n) if n.row["id"] == serde_json::json!(3)));
    }

    #[test]
    fn test_edit_outside_both_ignored() {
        // Case: oldCmp>0, newCmp>0 — both outside window
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(5), make_node(6)], 2);
        let _ = op.fetch(&FetchRequest::default());
        // bound=2

        let result = op.push(Change::Edit {
            node: make_node(6),
            old_node: make_node(5),
        });
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_edit_outside_to_inside_displaces() {
        // Case: oldCmp>0, newCmp<0 — old was outside, new is inside
        // Rows: [1, 2, 5] with limit 2, bound=2
        // Edit id=5 → id=0 (enters window, displaces bound)
        let (mut op, _data) = make_take(vec![make_node(0), make_node(1), make_node(2), make_node(5)], 2);
        let _ = op.fetch(&FetchRequest::default());
        // bound=1 (first 2 are id=0, id=1)

        let result = op.push(Change::Edit {
            node: make_node(-1),
            old_node: make_node(5),
        });
        // Old outside, new inside → displace old bound
        // Should: remove(old_bound_node), add(new_node)
        assert!(result.len() >= 2);
        assert!(matches!(&result[0], Change::Remove(_)));
        assert!(matches!(&result[1], Change::Add(n) if n.row["id"] == serde_json::json!(-1)));
    }

    #[test]
    fn test_edit_inside_both_forwards() {
        // Case: oldCmp<0, newCmp<0 — both inside window (not bound)
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 3);
        let _ = op.fetch(&FetchRequest::default());
        // bound=3

        // Edit id=1 → id=0 (both < bound=3)
        let result = op.push(Change::Edit {
            node: make_node(0),
            old_node: make_node(1),
        });
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Change::Edit { .. }));
    }

    #[test]
    fn test_edit_inside_to_outside() {
        // Case: oldCmp<0, newCmp>0 — old inside, new outside
        // Rows: [1, 2, 3] with limit 2, bound=2
        // Edit id=1 → id=5 (moves outside window)
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2), make_node(3)], 2);
        let _ = op.fetch(&FetchRequest::default());

        let result = op.push(Change::Edit {
            node: make_node(5),
            old_node: make_node(1),
        });
        // Should: remove(old=1), add(afterBoundNode=3)
        assert!(result.len() >= 2);
        assert!(matches!(&result[0], Change::Remove(n) if n.row["id"] == serde_json::json!(1)));
        assert!(matches!(&result[1], Change::Add(n) if n.row["id"] == serde_json::json!(3)));
    }

    // ===== CHILD TESTS =====

    #[test]
    fn test_child_within_window_propagates() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2)], 2);
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
    }

    #[test]
    fn test_child_outside_window_dropped() {
        let (mut op, _data) = make_take(vec![make_node(1), make_node(2)], 2);
        let _ = op.fetch(&FetchRequest::default());

        let child_change = Change::Child {
            node: make_node(99),
            child: crate::types::ChildData {
                relationship_name: "items".to_string(),
                change: Box::new(Change::Add(make_node(10))),
            },
        };
        let result = op.push(child_change);
        assert_eq!(result.len(), 0);
    }

    // ===== MAX BOUND TESTS =====

    #[test]
    fn test_max_bound_updated_across_partitions() {
        let nodes = vec![
            {
                let mut row = Row::new();
                row.insert("id".to_string(), serde_json::json!(1));
                row.insert("group".to_string(), serde_json::json!("a"));
                Node { row, relationships: HashMap::new() }
            },
            {
                let mut row = Row::new();
                row.insert("id".to_string(), serde_json::json!(5));
                row.insert("group".to_string(), serde_json::json!("b"));
                Node { row, relationships: HashMap::new() }
            },
        ];
        let input = Box::new(MockInput::new(nodes));
        let mut op = TakeOperator::new(
            input,
            1,
            default_sort(),
            Some(vec!["group".to_string()]),
        );

        // Fetch partition a
        let _ = op.fetch(&FetchRequest {
            constraint: Some(crate::types::Constraint::single(
                "group".to_string(),
                serde_json::json!("a"),
            )),
            start: None,
            reverse: false,
        });
        assert_eq!(op.max_bound.as_ref().unwrap()["id"], serde_json::json!(1));

        // Fetch partition b — maxBound should update to 5
        let _ = op.fetch(&FetchRequest {
            constraint: Some(crate::types::Constraint::single(
                "group".to_string(),
                serde_json::json!("b"),
            )),
            start: None,
            reverse: false,
        });
        assert_eq!(op.max_bound.as_ref().unwrap()["id"], serde_json::json!(5));
    }

    // ===== UNKNOWN PARTITION =====

    #[test]
    fn test_push_to_unknown_partition_ignored() {
        let (mut op, _data) = make_take(vec![make_node(1)], 2);
        let _ = op.fetch(&FetchRequest::default());

        // Push with a row that maps to a different partition key
        // For no partition_key, all map to same key, so this won't trigger.
        // But with partition_key set:
        let nodes = vec![make_node(1)];
        let input = Box::new(MockInput::new(nodes));
        let mut op = TakeOperator::new(
            input,
            2,
            default_sort(),
            Some(vec!["group".to_string()]),
        );
        // Don't fetch any partition — push should be ignored
        let result = op.push(Change::Add(make_node(1)));
        assert_eq!(result.len(), 0);
    }

    // ===== AUDIT-03: Promoted assertion regression tests =====
    // Confirms duplicate-primary-key invariants panic in release builds.
    // If anyone reverts `assert!` back to `debug_assert!`, these tests
    // will fail under `cargo test --release`.
    //
    // The Take operator uses the sort-key (id) as its compare-key in these
    // tests, so `new_cmp == Equal` against the bound row means a duplicate
    // primary key — which the framework guarantees never happens for an Edit.

    #[test]
    #[should_panic(expected = "Invalid state. Row has duplicate primary key")]
    fn test_take_duplicate_pk_in_inside_outside_branch_panics() {
        // Hits the line-246 site: old_cmp == Less (old inside non-bound),
        // new_cmp == Equal (new has same compare-key as bound) — must panic.
        // Setup: rows [1, 2, 3] with limit=2 → bound = id=2.
        let (mut op, _data) = make_take(
            vec![make_node(1), make_node(2), make_node(3)],
            2,
        );
        let _ = op.fetch(&FetchRequest::default());
        // Edit id=1 → id=2: old_cmp(1 vs 2) = Less, new_cmp(2 vs 2) = Equal.
        let _ = op.push(Change::Edit {
            node: make_node(2),
            old_node: make_node(1),
        });
    }

    #[test]
    #[should_panic(expected = "Invalid state. Row has duplicate primary key")]
    fn test_take_duplicate_pk_in_outside_outside_branch_panics() {
        // Hits the line-200 site: old_cmp == Greater (old outside),
        // new_cmp == Equal (new equals bound) — must panic.
        // Setup: rows [1, 2, 5] with limit=2 → bound = id=2.
        let (mut op, _data) = make_take(
            vec![make_node(1), make_node(2), make_node(5)],
            2,
        );
        let _ = op.fetch(&FetchRequest::default());
        // Edit id=5 → id=2: old_cmp(5 vs 2) = Greater, new_cmp(2 vs 2) = Equal.
        let _ = op.push(Change::Edit {
            node: make_node(2),
            old_node: make_node(5),
        });
    }
}
