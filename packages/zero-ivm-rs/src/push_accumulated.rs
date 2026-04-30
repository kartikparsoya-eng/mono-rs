//! TS spec: `packages/zql/src/ivm/push-accumulated.ts`.
//!
//! This module ports `pushAccumulatedChanges`, `mergeRelationships`,
//! and `makeAddEmptyRelationships` to Rust. These helpers are used by
//! both `FanIn` (filter-graph merge) and `UnionFanIn` (dedup merge with
//! relationship folding).
//!
//! # Semantics
//!
//! Given a list of accumulated `Change`s — one per branch that emitted —
//! produce at most one final Change to push downstream, respecting these
//! invariants:
//!
//! - `fanOutChangeType == REMOVE` → all entries must be REMOVE; emit one merged.
//! - `fanOutChangeType == ADD` → all entries must be ADD; emit one merged.
//! - `fanOutChangeType == EDIT` → entries can be ADD, REMOVE, EDIT.
//!   - If EDIT present, it supersedes; merge ADD/REMOVE relationships into it.
//!   - Else if ADD+REMOVE both present, reconstruct an EDIT.
//!   - Else single ADD or single REMOVE.
//! - `fanOutChangeType == CHILD` → entries can be ADD, REMOVE, CHILD.
//!   - If CHILD preserved, takes precedence.
//!   - Else single ADD or REMOVE (mutually exclusive).
//!
//! `mergeRelationships(left, right)` merges the relationships maps of two
//! Changes — the existing left wins on key collision, but right's keys are
//! added if missing. (TS: spread `{...right.relationships, ...left.relationships}`.)
//!
//! `makeAddEmptyRelationships(schema_relationship_names)` returns a closure
//! that pads any missing relationship key with an empty Vec.

use std::collections::HashMap;

use crate::types::{Change, ChangeType, ChildData, Node};

/// Merge the relationships of `right` into `left`, with `left`'s keys taking
/// precedence on collision. Returns a new Change with the merged relationships.
///
/// TS spec: `push-accumulated.ts:265-367` (`mergeRelationships`).
///
/// The TS implementation uses spread to merge into the *node*'s relationships
/// field. For most cases, both Changes have the same `change_type` (since the
/// fan-out forwarded the same input change). The exception is when
/// `fanOutChangeType == EDIT` and a branch converted it to ADD or REMOVE — in
/// that case, `left` is always EDIT and we merge right's relationships into
/// the appropriate side (NODE for ADD, OLD_NODE for REMOVE).
pub fn merge_relationships(left: &Change, right: &Change) -> Change {
    if left.change_type() == right.change_type() {
        return match left {
            Change::Add(left_node) => {
                let right_node = match right {
                    Change::Add(n) => n,
                    _ => unreachable!(),
                };
                Change::Add(merge_node_relationships(left_node, right_node))
            }
            Change::Remove(left_node) => {
                let right_node = match right {
                    Change::Remove(n) => n,
                    _ => unreachable!(),
                };
                Change::Remove(merge_node_relationships(left_node, right_node))
            }
            Change::Edit { node: left_node, old_node: left_old } => {
                let (right_node, right_old) = match right {
                    Change::Edit { node, old_node } => (node, old_node),
                    _ => unreachable!(),
                };
                Change::Edit {
                    node: merge_node_relationships(left_node, right_node),
                    old_node: merge_node_relationships(left_old, right_old),
                }
            }
            Change::Child { node: left_node, child } => {
                let right_node = match right {
                    Change::Child { node, .. } => node,
                    _ => unreachable!(),
                };
                Change::Child {
                    node: merge_node_relationships(left_node, right_node),
                    child: child.clone(),
                }
            }
        };
    }

    // Types differ — left must be EDIT (TS line 337-341).
    debug_assert_eq!(
        left.change_type(),
        ChangeType::Edit,
        "merge_relationships: types differ — left must be EDIT"
    );
    let (left_node, left_old) = match left {
        Change::Edit { node, old_node } => (node, old_node),
        _ => unreachable!(),
    };
    match right {
        Change::Add(right_node) => Change::Edit {
            node: merge_node_relationships(left_node, right_node),
            old_node: left_old.clone(),
        },
        Change::Remove(right_node) => Change::Edit {
            node: left_node.clone(),
            old_node: merge_node_relationships(left_old, right_node),
        },
        _ => unreachable!("merge_relationships: when types differ, right must be ADD or REMOVE"),
    }
}

/// Merge two Nodes by combining their relationships maps. `left`'s keys
/// take precedence; `right`'s keys are added if missing.
///
/// TS pattern: `{...right.relationships, ...left.relationships}` — left wins.
fn merge_node_relationships(left: &Node, right: &Node) -> Node {
    let mut merged: HashMap<String, Vec<Node>> = right.relationships.clone();
    for (k, v) in &left.relationships {
        merged.insert(k.clone(), v.clone());
    }
    Node { row: left.row.clone(), relationships: merged }
}

/// Returns a closure that pads a Change's relationships map with empty
/// Vec<Node> for any relationship name in `schema_relationship_names` that
/// the Change does not already carry.
///
/// TS spec: `push-accumulated.ts:369-413` (`makeAddEmptyRelationships`).
///
/// When `fanOutChangeType == CHILD`, the returned closure passes the change
/// through unchanged (children only carry relationships along the path to
/// the change).
pub fn make_add_empty_relationships<'a>(
    schema_relationship_names: &'a [String],
) -> impl Fn(Change) -> Change + 'a {
    move |change: Change| {
        if schema_relationship_names.is_empty() {
            return change;
        }
        match change {
            Change::Add(node) => Change::Add(pad_node(node, schema_relationship_names)),
            Change::Remove(node) => {
                Change::Remove(pad_node(node, schema_relationship_names))
            }
            Change::Edit { node, old_node } => Change::Edit {
                node: pad_node(node, schema_relationship_names),
                old_node: pad_node(old_node, schema_relationship_names),
            },
            // Children only have relationships along the path to the change.
            Change::Child { .. } => change,
        }
    }
}

fn pad_node(mut node: Node, names: &[String]) -> Node {
    for name in names {
        node.relationships.entry(name.clone()).or_default();
    }
    node
}

/// Identity merge — used by `FanIn` (filter-graph variant) which has no
/// per-branch relationship folding.
pub fn identity_merge(left: &Change, _right: &Change) -> Change {
    left.clone()
}

/// Identity make — used by `FanIn` (filter-graph variant).
pub fn identity_make(c: Change) -> Change {
    c
}

/// Folds accumulated branch outputs into at most one final Change.
///
/// TS spec: `push-accumulated.ts:87-260` (`pushAccumulatedChanges`).
///
/// Returns:
/// - `Vec::new()` if `accumulated` is empty (no branches emitted).
/// - A single-element Vec containing the merged Change otherwise.
///
/// Panics on invariant violations (e.g. `fanOutChangeType == REMOVE` with
/// any non-REMOVE accumulated entry). These match TS `assert(...)` calls.
pub fn push_accumulated_changes<MergeFn, MakeFn>(
    accumulated: Vec<Change>,
    fan_out_change_type: ChangeType,
    merge_fn: MergeFn,
    make_fn: MakeFn,
) -> Vec<Change>
where
    MergeFn: Fn(&Change, &Change) -> Change,
    MakeFn: Fn(Change) -> Change,
{
    if accumulated.is_empty() {
        return Vec::new();
    }

    // Collapse to at most one Change per ChangeType, folding via merge_fn.
    let mut candidates: HashMap<u8, Change> = HashMap::new();
    for change in accumulated {
        let ct = change_type_id(&change.change_type());
        if fan_out_change_type == ChangeType::Child && change.change_type() != ChangeType::Child {
            assert!(
                !candidates.contains_key(&ct),
                "Fan-in:child expected at most one {:?} when fan-out is of type child",
                change.change_type()
            );
        }
        let merged = if let Some(existing) = candidates.get(&ct) {
            merge_fn(existing, &change)
        } else {
            change
        };
        candidates.insert(ct, merged);
    }

    let add_id = change_type_id(&ChangeType::Add);
    let remove_id = change_type_id(&ChangeType::Remove);
    let edit_id = change_type_id(&ChangeType::Edit);
    let child_id = change_type_id(&ChangeType::Child);

    match fan_out_change_type {
        ChangeType::Remove => {
            assert!(
                candidates.len() == 1 && candidates.contains_key(&remove_id),
                "Fan-in:remove expected all removes"
            );
            let c = candidates.remove(&remove_id).unwrap();
            vec![make_fn(c)]
        }
        ChangeType::Add => {
            assert!(
                candidates.len() == 1 && candidates.contains_key(&add_id),
                "Fan-in:add expected all adds"
            );
            let c = candidates.remove(&add_id).unwrap();
            vec![make_fn(c)]
        }
        ChangeType::Edit => {
            for k in candidates.keys() {
                assert!(
                    *k == add_id || *k == remove_id || *k == edit_id,
                    "Fan-in:edit expected all adds, removes, or edits"
                );
            }
            let mut add_change = candidates.remove(&add_id);
            let mut remove_change = candidates.remove(&remove_id);
            let mut edit_change = candidates.remove(&edit_id);

            if let Some(mut e) = edit_change.take() {
                if let Some(a) = add_change.take() {
                    e = merge_fn(&e, &a);
                }
                if let Some(r) = remove_change.take() {
                    e = merge_fn(&e, &r);
                }
                return vec![make_fn(e)];
            }

            if let (Some(a), Some(r)) = (add_change.take(), remove_change.take()) {
                let edit = match (a, r) {
                    (Change::Add(node), Change::Remove(old_node)) => Change::Edit { node, old_node },
                    _ => unreachable!("ADD/REMOVE must come from those variants"),
                };
                return vec![make_fn(edit)];
            }

            if let Some(a) = add_change {
                return vec![make_fn(a)];
            }
            if let Some(r) = remove_change {
                return vec![make_fn(r)];
            }
            unreachable!("at least one candidate must exist for EDIT case")
        }
        ChangeType::Child => {
            for k in candidates.keys() {
                assert!(
                    *k == add_id || *k == remove_id || *k == child_id,
                    "Fan-in:child expected all adds, removes, or children"
                );
            }
            assert!(
                candidates.len() <= 2,
                "Fan-in:child expected at most 2 types on a child change from fan-out"
            );

            // Child preserved → takes precedence (TS line 237-241).
            if let Some(c) = candidates.remove(&child_id) {
                return vec![c];
            }
            let add_change = candidates.remove(&add_id);
            let remove_change = candidates.remove(&remove_id);
            assert!(
                add_change.is_none() || remove_change.is_none(),
                "Fan-in:child expected either add or remove, not both"
            );
            if let Some(a) = add_change {
                return vec![make_fn(a)];
            }
            if let Some(r) = remove_change {
                return vec![make_fn(r)];
            }
            unreachable!("child case must yield ADD, REMOVE, or CHILD")
        }
    }
}

fn change_type_id(ct: &ChangeType) -> u8 {
    match ct {
        ChangeType::Add => 0,
        ChangeType::Remove => 1,
        ChangeType::Child => 2,
        ChangeType::Edit => 3,
    }
}

// Suppress unused-import warning for ChildData while Wave 1 lacks consumers.
#[allow(dead_code)]
fn _child_data_marker(_c: ChildData) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Node;

    fn make_node(id: i64) -> Node {
        Node {
            row: [("id".to_string(), serde_json::json!(id))]
                .into_iter()
                .collect(),
            relationships: HashMap::new(),
        }
    }

    fn make_node_with_rels(id: i64, rel: &str, children: Vec<Node>) -> Node {
        let mut n = make_node(id);
        n.relationships.insert(rel.to_string(), children);
        n
    }

    #[test]
    fn test_empty_returns_empty() {
        let out = push_accumulated_changes(
            vec![],
            ChangeType::Add,
            identity_merge,
            identity_make,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn test_single_add_passthrough() {
        let out = push_accumulated_changes(
            vec![Change::Add(make_node(1))],
            ChangeType::Add,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Add);
        assert_eq!(out[0].node().row.get("id").unwrap(), &serde_json::json!(1));
    }

    #[test]
    fn test_two_adds_merged_to_one() {
        let out = push_accumulated_changes(
            vec![Change::Add(make_node(1)), Change::Add(make_node(1))],
            ChangeType::Add,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn test_remove_invariant_passes() {
        let out = push_accumulated_changes(
            vec![Change::Remove(make_node(1)), Change::Remove(make_node(1))],
            ChangeType::Remove,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Remove);
    }

    #[test]
    #[should_panic(expected = "Fan-in:remove expected all removes")]
    fn test_remove_invariant_violation_panics() {
        push_accumulated_changes(
            vec![Change::Remove(make_node(1)), Change::Add(make_node(1))],
            ChangeType::Remove,
            identity_merge,
            identity_make,
        );
    }

    #[test]
    #[should_panic(expected = "Fan-in:add expected all adds")]
    fn test_add_invariant_violation_panics() {
        push_accumulated_changes(
            vec![Change::Add(make_node(1)), Change::Remove(make_node(1))],
            ChangeType::Add,
            identity_merge,
            identity_make,
        );
    }

    #[test]
    fn test_edit_with_add_and_remove_reconstructs_edit() {
        let new_node = make_node(2);
        let old_node = make_node(1);
        let out = push_accumulated_changes(
            vec![
                Change::Add(new_node.clone()),
                Change::Remove(old_node.clone()),
            ],
            ChangeType::Edit,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        match &out[0] {
            Change::Edit { node, old_node: o } => {
                assert_eq!(node.row.get("id").unwrap(), &serde_json::json!(2));
                assert_eq!(o.row.get("id").unwrap(), &serde_json::json!(1));
            }
            _ => panic!("expected edit, got {:?}", out[0].change_type()),
        }
    }

    #[test]
    fn test_edit_with_only_edit_passes_through() {
        let edit = Change::Edit { node: make_node(2), old_node: make_node(1) };
        let out = push_accumulated_changes(
            vec![edit],
            ChangeType::Edit,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Edit);
    }

    #[test]
    fn test_edit_supersedes_add_remove() {
        // EDIT is present → it wins; ADD/REMOVE relationships merged into it.
        let edit = Change::Edit { node: make_node(2), old_node: make_node(1) };
        let add = Change::Add(make_node(2));
        let out = push_accumulated_changes(
            vec![edit, add],
            ChangeType::Edit,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Edit);
    }

    #[test]
    fn test_child_preserved_takes_precedence() {
        let child_change = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "x".to_string(),
                change: Box::new(Change::Add(make_node(2))),
            },
        };
        let add = Change::Add(make_node(1));
        let out = push_accumulated_changes(
            vec![child_change, add],
            ChangeType::Child,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Child);
    }

    #[test]
    fn test_child_only_add_emitted() {
        let add = Change::Add(make_node(1));
        let out = push_accumulated_changes(
            vec![add],
            ChangeType::Child,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Add);
    }

    #[test]
    fn test_child_only_remove_emitted() {
        let rem = Change::Remove(make_node(1));
        let out = push_accumulated_changes(
            vec![rem],
            ChangeType::Child,
            identity_merge,
            identity_make,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change_type(), ChangeType::Remove);
    }

    #[test]
    fn test_merge_relationships_add_combines_relationships() {
        let left = Change::Add(make_node_with_rels(
            1,
            "rel_a",
            vec![make_node(10)],
        ));
        let right = Change::Add(make_node_with_rels(
            1,
            "rel_b",
            vec![make_node(20)],
        ));
        let merged = merge_relationships(&left, &right);
        let merged_node = merged.node();
        assert!(merged_node.relationships.contains_key("rel_a"));
        assert!(merged_node.relationships.contains_key("rel_b"));
    }

    #[test]
    fn test_merge_relationships_left_wins_on_collision() {
        let left = Change::Add(make_node_with_rels(
            1,
            "rel_a",
            vec![make_node(10)],
        ));
        let right = Change::Add(make_node_with_rels(
            1,
            "rel_a",
            vec![make_node(20)],
        ));
        let merged = merge_relationships(&left, &right);
        let merged_node = merged.node();
        assert_eq!(merged_node.relationships["rel_a"][0].row.get("id"), Some(&serde_json::json!(10)));
    }

    #[test]
    fn test_make_add_empty_relationships_pads_missing() {
        let names = vec!["rel_a".to_string(), "rel_b".to_string()];
        let f = make_add_empty_relationships(&names);
        let change = Change::Add(make_node_with_rels(
            1,
            "rel_a",
            vec![make_node(10)],
        ));
        let padded = f(change);
        let n = padded.node();
        assert!(n.relationships.contains_key("rel_a"));
        assert!(n.relationships.contains_key("rel_b"));
        assert_eq!(n.relationships["rel_b"].len(), 0);
    }

    #[test]
    fn test_make_add_empty_relationships_no_op_for_child() {
        let names = vec!["rel_a".to_string()];
        let f = make_add_empty_relationships(&names);
        let change = Change::Child {
            node: make_node(1),
            child: ChildData {
                relationship_name: "rel_a".to_string(),
                change: Box::new(Change::Add(make_node(2))),
            },
        };
        let padded = f(change);
        // Child is unchanged.
        assert_eq!(padded.change_type(), ChangeType::Child);
        assert!(padded.node().relationships.is_empty());
    }

    #[test]
    fn test_make_add_empty_relationships_empty_names_passthrough() {
        let names: Vec<String> = vec![];
        let f = make_add_empty_relationships(&names);
        let change = Change::Add(make_node(1));
        let padded = f(change.clone());
        assert!(padded.node().relationships.is_empty());
    }
}
