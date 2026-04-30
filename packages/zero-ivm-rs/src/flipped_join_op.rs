//! TS spec: `packages/zql/src/ivm/flipped-join.ts` (505 LOC) — child-driven
//! inner join. Closes deep-audit finding B7 and removes parity allow-list
//! entries `fuzz_00132` and `fuzz_00133`.
//!
//! See `.planning/phases/36-flipped-join-family/36-RESEARCH.md`
//! §"FlippedJoin TS↔Rust intent map" for the line-by-line port plan.
//!
//! # TS↔Rust port mapping
//!
//! | TS line                     | TS construct                                    | Rust intent |
//! | --------------------------- | ----------------------------------------------- | ----------- |
//! | `flipped-join.ts:52-105`    | constructor + schema merge                      | `FlippedJoinOperator::new` — store parent/child operators, parent_key/child_key, relationship_name. |
//! | `flipped-join.ts:74-78`     | `assert(parent !== child, ...)` + key-len assert | `assert!(parent_arc.id != child_arc.id)` + `assert_eq!(parent_key.len(), child_key.len())` at construction. |
//! | `flipped-join.ts:99-104`    | `parent.setOutput({push: pushParent})` etc.     | Rust uses synchronous `Operator::push` — no callback registration. Dispatch via push() vs push_child() (mirrors JoinOperator). |
//! | `flipped-join.ts:124-148`   | `*fetch(req)` — translate parent constraint     | `fn fetch(&self, req: &FetchRequest) -> Vec<Node>` — synchronous. Translate parent constraint to child constraint, fetch children, k-way merge parents. |
//! | `flipped-join.ts:158-165`   | overlay-undo: REMOVE in_progress splices node back | `if Change::Remove(node) = self.in_progress_child { binary_search insert into child_nodes }`. |
//! | `flipped-join.ts:166-205`   | open per-child parent iterators                 | `let parent_streams: Vec<Vec<Node>> = child_nodes.iter().map(...)` synchronous fetch. |
//! | `flipped-join.ts:207-296`   | k-way merge by parent's compareRows order       | `BinaryHeap<(Reverse<RowKey>, child_idx, node_idx)>` — pop min, advance cursors, emit min_parent with relationships. |
//! | `flipped-join.ts:247-284`   | overlay applied per emitted parent              | `apply_overlay_for_parent(in_progress_child, min_parent, related_child_nodes, child_key, parent_key)` — pure function returns Vec<Node>. |
//! | `flipped-join.ts:287-294`   | yield {minParentNode, relationships: {[relName]: () => overlaid}} | Eager materialization: `out.push(node_with_relationship(min_parent, rel_name, overlaid))`. |
//! | `flipped-join.ts:298-319`   | iterator throw/return cleanup                   | Not needed — Rust drops Vecs on scope exit. |
//! | `flipped-join.ts:322-344`   | `*pushChild(change)` dispatch                   | `fn push_child(&mut self, change: Change) -> Vec<Change>` — switch on change_type → `push_child_change(change, exists?)`. |
//! | `flipped-join.ts:346-425`   | `*pushChildChange` — for each parent matching child constraint | `let constraint = build_join_constraint(child_row, child_key, parent_key); for parent in parent.fetch(constraint) { emit child/add/remove }`. |
//! | `flipped-join.ts:373-388`   | "exists" check via secondary child fetch        | `fn other_child_exists(child, parent_row, parent_key, child_key, excluding) -> bool`. |
//! | `flipped-join.ts:389-420`   | emit makeChildChange or makeAdd/RemoveChange    | Direct mapping to Rust `Change::Child` / `Change::Add` / `Change::Remove`. |
//! | `flipped-join.ts:427-505`   | `*pushParent(change)` — fetch children for parent, emit with rel | `fn push_parent(&mut self, change: Change) -> Vec<Change>` — compute child constraint, fetch, emit flip(node) with relationships. |
//! | `flipped-join.ts:484-499`   | parent-edit must not change relationship key    | `assert!(row_equals_for_compound_key(&old.row, &new.row, &parent_key))`. |
//!
//! # Generator-to-synchronous translation
//!
//! - `function*` → `fn` returning `Vec<X>`.
//! - `yield 'yield'` → drop entirely.
//! - `yield val` → `out.push(val)`.
//! - `yield* sub_generator()` → `out.extend(sub_generator())`.
//! - In-progress overlay (`flipped-join.ts:158-284`) becomes a struct field
//!   `Option<InProgressOverlay>` with explicit `take()` in a guarded scope.
//!
//! # Implementation status
//!
//! Wave 0 stub — fields/struct skeleton only. Wave 3 implements `Operator` trait.

use std::collections::HashMap;

use crate::operator::Operator;
use crate::types::{Change, Row};

/// In-progress overlay state set during a child push.
///
/// TS spec: `flipped-join.ts:158-165`, `flipped-join.ts:247-284`.
///
/// When `pushChild` is processing a change, fetch operations on the parent
/// stream may run before the change has been propagated to all interested
/// parents. The overlay restores the pre-change state for parents that
/// haven't yet seen the notification.
///
/// Wave 3 implements; Wave 0 declares the type.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct InProgressOverlay {
    /// The Change currently being processed by push_child.
    /// TS line 158-159: `this.#inprogressChildChange = change`.
    pub change: Change,
    /// The parent row at which we are currently in the merge cursor.
    /// TS line 161: `this.#inprogressChildChangePosition = parentNode.row`.
    pub position: Option<Row>,
}

/// FlippedJoin operator — child-driven inner join.
///
/// TS spec: `packages/zql/src/ivm/flipped-join.ts` lines 52-505.
///
/// Unlike a regular Join (parent-driven), FlippedJoin emits per-child output:
/// the planner chooses this when the child cardinality is small relative to
/// the parent, making child-driven evaluation cheaper.
///
/// Wave 0 stub. Wave 3 implements `Operator` trait + ≥50 unit tests.
#[allow(dead_code)]
pub struct FlippedJoinOperator {
    /// TS line 56: parent input. In Rust this is the "outer" Operator —
    /// fetched per-child during the k-way merge in fetch().
    pub(crate) parent: Box<dyn Operator>,
    /// TS line 57: child input. In Rust this is the "inner" Operator that
    /// drives the iteration order in fetch() and push_child.
    pub(crate) child: Box<dyn Operator>,
    /// TS line 58: parent key columns (e.g. ["id"]).
    pub(crate) parent_key: Vec<String>,
    /// TS line 59: child key columns (e.g. ["parent_id"]).
    pub(crate) child_key: Vec<String>,
    /// TS line 60: relationship name embedded into emitted Node.relationships.
    pub(crate) relationship_name: String,
    /// TS line 61: hidden flag (passed to merge_schemas).
    pub(crate) hidden: bool,
    /// TS line 62: system flag ("client" or "permissions").
    pub(crate) system: String,
    /// TS line 90: in-progress child change overlay state.
    /// Set at the start of push_child; cleared at the end.
    pub(crate) in_progress_child: Option<InProgressOverlay>,
}

impl FlippedJoinOperator {
    /// Constructor stub.
    ///
    /// TS spec: `flipped-join.ts:52-105`.
    ///
    /// Wave 3 will add schema merge and operator-output wiring; for Wave 0
    /// the constructor exists only to satisfy `cargo check` and pin the field
    /// layout.
    #[allow(dead_code)]
    pub fn new(
        parent: Box<dyn Operator>,
        child: Box<dyn Operator>,
        parent_key: Vec<String>,
        child_key: Vec<String>,
        relationship_name: String,
        hidden: bool,
        system: String,
    ) -> Self {
        // TS line 75: assert!(parent_key.len() == child_key.len()).
        assert_eq!(
            parent_key.len(),
            child_key.len(),
            "FlippedJoin: parent_key and child_key must have the same length"
        );
        Self {
            parent,
            child,
            parent_key,
            child_key,
            relationship_name,
            hidden,
            system,
            in_progress_child: None,
        }
    }

    /// Helper: build a `Vec<Node>` join-relationships map carrying
    /// `relationship_name → child_nodes`. Wave 3 uses this when emitting parent
    /// changes; Wave 0 just pins the helper signature.
    #[allow(dead_code)]
    pub(crate) fn make_relationships_map(
        &self,
        children: Vec<crate::types::Node>,
    ) -> HashMap<String, Vec<crate::types::Node>> {
        let mut m = HashMap::new();
        m.insert(self.relationship_name.clone(), children);
        m
    }
}

#[cfg(test)]
mod tests {
    // Wave 3 fills in ≥50 unit tests. Wave 0 leaves this empty.
}
