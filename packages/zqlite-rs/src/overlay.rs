use std::collections::HashMap;

use crate::source::{
    compare_rows, FetchConstraint, Node, Overlay, Overlays, Row, SortSpec, SourceChange,
};

fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    a == b
}

fn constraint_matches_row(constraint: &FetchConstraint, row: &Row) -> bool {
    let null = serde_json::Value::Null;
    let val = row.get(&constraint.key).unwrap_or(&null);
    values_equal(val, &constraint.value)
}

fn row_matches_pk(a: &Row, b: &Row, primary_key: &[String]) -> bool {
    let null = serde_json::Value::Null;
    primary_key.iter().all(|k| {
        let av = a.get(k).unwrap_or(&null);
        let bv = b.get(k).unwrap_or(&null);
        values_equal(av, bv)
    })
}

fn overlays_for_start_at(overlays: &mut Overlays, start_at: Option<&Row>, sort: &[SortSpec]) {
    if let Some(start) = start_at {
        if let Some(ref add) = overlays.add {
            if compare_rows(add, start, sort) == std::cmp::Ordering::Less {
                overlays.add = None;
            }
        }
        if let Some(ref remove) = overlays.remove {
            if compare_rows(remove, start, sort) == std::cmp::Ordering::Less {
                overlays.remove = None;
            }
        }
    }
}

fn overlays_for_constraint(overlays: &mut Overlays, constraint: Option<&FetchConstraint>) {
    if let Some(c) = constraint {
        if let Some(ref add) = overlays.add {
            if !constraint_matches_row(c, add) {
                overlays.add = None;
            }
        }
        if let Some(ref remove) = overlays.remove {
            if !constraint_matches_row(c, remove) {
                overlays.remove = None;
            }
        }
    }
}

fn overlays_for_filter(overlays: &mut Overlays, filter_predicate: Option<&dyn Fn(&Row) -> bool>) {
    if let Some(pred) = filter_predicate {
        if let Some(ref add) = overlays.add {
            if !pred(add) {
                overlays.add = None;
            }
        }
        if let Some(ref remove) = overlays.remove {
            if !pred(remove) {
                overlays.remove = None;
            }
        }
    }
}

pub fn compute_overlays(
    start_at: Option<&Row>,
    constraint: Option<&FetchConstraint>,
    overlay: Option<&Overlay>,
    last_pushed_epoch: u64,
    sort: &[SortSpec],
    filter_predicate: Option<&dyn Fn(&Row) -> bool>,
) -> Overlays {
    let mut result = Overlays::default();

    let overlay = match overlay {
        Some(o) if last_pushed_epoch >= o.epoch => o,
        _ => return result,
    };

    match &overlay.change {
        SourceChange::Add(row) => {
            result.add = Some(row.clone());
        }
        SourceChange::Remove(row) => {
            result.remove = Some(row.clone());
        }
        SourceChange::Edit { row, old_row } => {
            result.add = Some(row.clone());
            result.remove = Some(old_row.clone());
        }
    }

    overlays_for_start_at(&mut result, start_at, sort);
    overlays_for_constraint(&mut result, constraint);
    overlays_for_filter(&mut result, filter_predicate);

    result
}

fn make_node(row: Row) -> Node {
    Node {
        row,
        relationships: HashMap::new(),
    }
}

pub fn generate_with_overlay(rows: Vec<Row>, overlays: &Overlays, sort: &[SortSpec]) -> Vec<Node> {
    let mut result = Vec::with_capacity(rows.len() + 1);
    let mut add_yielded = false;

    for row in rows {
        if !add_yielded {
            if let Some(ref add) = overlays.add {
                if compare_rows(add, &row, sort) == std::cmp::Ordering::Less {
                    result.push(make_node(add.clone()));
                    add_yielded = true;
                }
            }
        }

        if let Some(ref remove) = overlays.remove {
            if compare_rows(remove, &row, sort) == std::cmp::Ordering::Equal {
                continue;
            }
        }

        result.push(make_node(row));
    }

    if !add_yielded {
        if let Some(ref add) = overlays.add {
            result.push(make_node(add.clone()));
        }
    }

    result
}

pub fn generate_with_overlay_unordered(
    rows: Vec<Row>,
    overlays: &Overlays,
    primary_key: &[String],
) -> Vec<Node> {
    let mut result = Vec::with_capacity(rows.len() + 1);

    if let Some(ref add) = overlays.add {
        result.push(make_node(add.clone()));
    }

    for row in rows {
        if let Some(ref remove) = overlays.remove {
            if row_matches_pk(remove, &row, primary_key) {
                continue;
            }
        }
        result.push(make_node(row));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SortDirection;
    use serde_json::json;

    fn row(pairs: &[(&str, serde_json::Value)]) -> Row {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn sort_by_id() -> Vec<SortSpec> {
        vec![SortSpec {
            field: "id".to_string(),
            direction: SortDirection::Asc,
        }]
    }

    #[test]
    fn test_compute_overlays_add() {
        let overlay = Overlay {
            epoch: 1,
            change: SourceChange::Add(row(&[("id", json!(5))])),
        };
        let result = compute_overlays(None, None, Some(&overlay), 1, &sort_by_id(), None);
        assert!(result.add.is_some());
        assert_eq!(result.add.unwrap().get("id").unwrap(), &json!(5));
        assert!(result.remove.is_none());
    }

    #[test]
    fn test_compute_overlays_remove() {
        let overlay = Overlay {
            epoch: 1,
            change: SourceChange::Remove(row(&[("id", json!(3))])),
        };
        let result = compute_overlays(None, None, Some(&overlay), 1, &sort_by_id(), None);
        assert!(result.add.is_none());
        assert!(result.remove.is_some());
        assert_eq!(result.remove.unwrap().get("id").unwrap(), &json!(3));
    }

    #[test]
    fn test_compute_overlays_edit() {
        let overlay = Overlay {
            epoch: 2,
            change: SourceChange::Edit {
                row: row(&[("id", json!(1)), ("name", json!("new"))]),
                old_row: row(&[("id", json!(1)), ("name", json!("old"))]),
            },
        };
        let result = compute_overlays(None, None, Some(&overlay), 2, &sort_by_id(), None);
        assert!(result.add.is_some());
        assert!(result.remove.is_some());
        assert_eq!(result.add.unwrap().get("name").unwrap(), &json!("new"));
        assert_eq!(result.remove.unwrap().get("name").unwrap(), &json!("old"));
    }

    #[test]
    fn test_compute_overlays_epoch_not_reached() {
        let overlay = Overlay {
            epoch: 5,
            change: SourceChange::Add(row(&[("id", json!(1))])),
        };
        let result = compute_overlays(None, None, Some(&overlay), 3, &sort_by_id(), None);
        assert!(result.add.is_none());
        assert!(result.remove.is_none());
    }

    #[test]
    fn test_generate_with_overlay_add_middle() {
        let rows = vec![row(&[("id", json!(1))]), row(&[("id", json!(3))])];
        let overlays = Overlays {
            add: Some(row(&[("id", json!(2))])),
            remove: None,
        };
        let nodes = generate_with_overlay(rows, &overlays, &sort_by_id());
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].row.get("id").unwrap(), &json!(1));
        assert_eq!(nodes[1].row.get("id").unwrap(), &json!(2));
        assert_eq!(nodes[2].row.get("id").unwrap(), &json!(3));
    }

    #[test]
    fn test_generate_with_overlay_add_end() {
        let rows = vec![row(&[("id", json!(1))]), row(&[("id", json!(2))])];
        let overlays = Overlays {
            add: Some(row(&[("id", json!(5))])),
            remove: None,
        };
        let nodes = generate_with_overlay(rows, &overlays, &sort_by_id());
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[2].row.get("id").unwrap(), &json!(5));
    }

    #[test]
    fn test_generate_with_overlay_remove() {
        let rows = vec![
            row(&[("id", json!(1))]),
            row(&[("id", json!(2))]),
            row(&[("id", json!(3))]),
        ];
        let overlays = Overlays {
            add: None,
            remove: Some(row(&[("id", json!(2))])),
        };
        let nodes = generate_with_overlay(rows, &overlays, &sort_by_id());
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].row.get("id").unwrap(), &json!(1));
        assert_eq!(nodes[1].row.get("id").unwrap(), &json!(3));
    }

    #[test]
    fn test_generate_with_overlay_unordered_add_remove() {
        let rows = vec![row(&[("id", json!(1))]), row(&[("id", json!(2))])];
        let overlays = Overlays {
            add: Some(row(&[("id", json!(99))])),
            remove: Some(row(&[("id", json!(1))])),
        };
        let pk = vec!["id".to_string()];
        let nodes = generate_with_overlay_unordered(rows, &overlays, &pk);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].row.get("id").unwrap(), &json!(99));
        assert_eq!(nodes[1].row.get("id").unwrap(), &json!(2));
    }

    #[test]
    fn test_overlays_for_start_at() {
        let overlay = Overlay {
            epoch: 1,
            change: SourceChange::Add(row(&[("id", json!(1))])),
        };
        let start = row(&[("id", json!(5))]);
        let result =
            compute_overlays(Some(&start), None, Some(&overlay), 1, &sort_by_id(), None);
        assert!(result.add.is_none());
    }

    #[test]
    fn test_overlays_for_constraint() {
        let overlay = Overlay {
            epoch: 1,
            change: SourceChange::Add(row(&[("id", json!(1)), ("color", json!("red"))])),
        };
        let constraint = FetchConstraint {
            key: "color".to_string(),
            value: json!("blue"),
        };
        let result = compute_overlays(
            None,
            Some(&constraint),
            Some(&overlay),
            1,
            &sort_by_id(),
            None,
        );
        assert!(result.add.is_none());
    }
}
