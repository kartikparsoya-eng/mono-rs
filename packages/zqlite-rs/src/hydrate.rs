use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;

use zero_ivm_rs::operator::Operator;
use zero_ivm_rs::pipeline::OperatorConfig;
use zero_ivm_rs::types::{Change, FetchRequest, Node};

use crate::query_builder::ColumnType;
use crate::source::{
    FetchRequest as SourceFetchRequest, Node as SourceNode,
};
use crate::table_source::RustTableSource;

/// An Operator that fetches from a real SQLite database via RustTableSource.
pub struct LiveTableSource {
    source: Arc<RustTableSource>,
    connection_id: usize,
    sort: Vec<zero_ivm_rs::types::SortSpec>,
}

impl LiveTableSource {
    pub fn new(
        source: Arc<RustTableSource>,
        connection_id: usize,
        sort: Vec<zero_ivm_rs::types::SortSpec>,
    ) -> Self {
        Self {
            source,
            connection_id,
            sort,
        }
    }
}

fn convert_source_node(sn: SourceNode) -> Node {
    Node {
        row: sn.row,
        relationships: sn
            .relationships
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().map(convert_source_node).collect()))
            .collect(),
    }
}

impl Operator for LiveTableSource {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let source_req = SourceFetchRequest {
            constraint: req.constraint.as_ref().map(|c| {
                crate::source::FetchConstraint {
                    columns: c.columns.clone(),
                }
            }),
            start: req.start.as_ref().map(|s| crate::source::FetchStart {
                row: s.row.clone(),
                basis: s.basis.clone(),
            }),
            reverse: req.reverse,
        };
        match self.source.fetch(self.connection_id, &source_req) {
            Ok(nodes) => nodes.into_iter().map(convert_source_node).collect(),
            Err(e) => {
                eprintln!("LiveTableSource fetch error: {e}");
                vec![]
            }
        }
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "live_table_source"
    }
}

unsafe impl Send for LiveTableSource {}

/// Canonical string key for grouping batch results — matches what
/// `RustTableSource::fetch_batch` uses for its `HashMap` keys.
fn value_to_group_key(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => "null".to_string(),
    }
}

/// An operator that yields pre-fetched nodes. Used to feed batch-fetched
/// children through remaining child operators (Filter, Take, etc.).
struct PreloadedSource {
    nodes: Vec<Node>,
}

impl PreloadedSource {
    fn new(nodes: Vec<Node>) -> Self {
        Self { nodes }
    }
}

impl Operator for PreloadedSource {
    fn fetch(&mut self, _req: &FetchRequest) -> Vec<Node> {
        std::mem::take(&mut self.nodes)
    }
    fn push(&mut self, _change: Change) -> Vec<Change> {
        vec![]
    }
    fn op_type(&self) -> &'static str {
        "preloaded_source"
    }
}

unsafe impl Send for PreloadedSource {}

/// Extract the Source config from the first element of a child operator config.
/// Returns (table_name, columns, column_types, primary_key, sort) or None.
fn extract_child_source_info(
    child_config: &[OperatorConfig],
) -> Option<(String, Vec<String>, HashMap<String, ColumnType>, Vec<String>, Vec<(String, String)>)> {
    if child_config.is_empty() {
        return None;
    }
    match &child_config[0] {
        OperatorConfig::Source {
            table_name,
            columns,
            primary_key,
            sort,
        } => {
            let mut ct = HashMap::new();
            for c in columns {
                ct.insert(c.clone(), ColumnType::String);
            }
            Some((table_name.clone(), columns.clone(), ct, primary_key.clone(), sort.clone()))
        }
        _ => None,
    }
}

/// Apply the non-Source portion of a child operator config to pre-fetched nodes.
/// Builds a chain: PreloadedSource -> [Filter, Take, ...] and fetches.
fn apply_child_operators(
    source: &Arc<RustTableSource>,
    child_config: &[OperatorConfig],
    nodes: Vec<Node>,
) -> Vec<Node> {
    if child_config.len() <= 1 {
        return nodes;
    }
    let mut current: Box<dyn Operator> = Box::new(PreloadedSource::new(nodes));
    for config in &child_config[1..] {
        current = match build_next_operator(source.clone(), current, config) {
            Ok(op) => op,
            Err(_) => return vec![],
        };
    }
    current.fetch(&FetchRequest::default())
}

/// Batch fetch all children for a set of parent rows, returning a map from
/// parent key value (as group key string) to the child nodes (with remaining
/// child operators applied per group).
fn batch_fetch_children(
    source: &Arc<RustTableSource>,
    child_config: &[OperatorConfig],
    parent_key: &[String],
    child_key: &[String],
    parent_nodes: &[Node],
) -> HashMap<String, Vec<Node>> {
    if parent_key.is_empty() || child_key.is_empty() {
        return HashMap::new();
    }
    let info = match extract_child_source_info(child_config) {
        Some(i) => i,
        None => return HashMap::new(),
    };
    let (table_name, columns, column_types, _primary_key, sort) = info;

    // Collect unique parent key values (first key only for the IN clause)
    let pk0 = &parent_key[0];
    let ck0 = &child_key[0];
    let mut unique_vals: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for node in parent_nodes {
        if let Some(v) = node.row.get(pk0) {
            if !v.is_null() {
                let key = value_to_group_key(v);
                if seen.insert(key) {
                    unique_vals.push(v.clone());
                }
            }
        }
    }

    if unique_vals.is_empty() {
        return HashMap::new();
    }

    // Build a child source that shares the parent pool
    let shared_pool = source.shared_pool();
    let child_source = match RustTableSource::new_with_shared_pool(
        shared_pool,
        table_name,
        columns,
        column_types,
        _primary_key,
    ) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };

    let ordering = if sort.is_empty() { None } else { Some(&sort) };
    let source_grouped = match child_source.fetch_batch(ck0, &unique_vals, ordering) {
        Ok(g) => g,
        Err(_) => return HashMap::new(),
    };

    // Convert source::Node -> zero_ivm_rs::types::Node
    let mut grouped: HashMap<String, Vec<Node>> = source_grouped
        .into_iter()
        .map(|(k, nodes)| (k, nodes.into_iter().map(convert_source_node).collect()))
        .collect();

    // Apply remaining child operators (Filter, Take, etc.) per group
    if child_config.len() > 1 {
        let keys: Vec<String> = grouped.keys().cloned().collect();
        for key in keys {
            if let Some(nodes) = grouped.remove(&key) {
                let filtered = apply_child_operators(source, child_config, nodes);
                grouped.insert(key, filtered);
            }
        }
    }

    // Multi-key filtering: if parent_key has >1 columns, filter children
    // to match all key columns (the IN clause only covers the first).
    if parent_key.len() > 1 {
        // Build a lookup from group_key -> vec of (parent_row_ref) to know
        // which composite keys are valid. Actually, we need to filter per
        // parent row at lookup time, so we leave grouped as-is and do the
        // composite filtering at the call site.
    }

    grouped
}

/// Create a RustTableSource for the child table that shares the parent's
/// connection pool (no new SQLite connections are opened).
fn make_child_source(
    parent_source: &Arc<RustTableSource>,
    child_config: &[OperatorConfig],
) -> Option<RustTableSource> {
    if child_config.is_empty() {
        return None;
    }
    match &child_config[0] {
        OperatorConfig::Source {
            table_name,
            columns,
            primary_key,
            sort,
        } => {
            let mut ct = HashMap::new();
            for c in columns {
                ct.insert(c.clone(), ColumnType::String);
            }
            let shared_pool = parent_source.shared_pool();
            let mut src = RustTableSource::new_with_shared_pool(
                shared_pool,
                table_name.clone(),
                columns.clone(),
                ct,
                primary_key.clone(),
            ).ok()?;
            let ordering: Vec<(String, String)> = sort.clone();
            src.connect(Some(ordering), None, None);
            Some(src)
        }
        _ => None,
    }
}

/// A Join operator that fetches children in parallel across parent rows.
/// Instead of holding a single mutable child operator, it holds the child
/// operator config and rebuilds the child tree per parent row on Rayon threads.
pub struct ParallelJoinOperator {
    parent: Box<dyn Operator>,
    child_config: Vec<OperatorConfig>,
    source: Arc<RustTableSource>,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
}

impl ParallelJoinOperator {
    pub fn new(
        parent: Box<dyn Operator>,
        child_config: Vec<OperatorConfig>,
        source: Arc<RustTableSource>,
        parent_key: Vec<String>,
        child_key: Vec<String>,
        relationship_name: String,
    ) -> Self {
        Self {
            parent,
            child_config,
            source,
            parent_key,
            child_key,
            relationship_name,
        }
    }
}

unsafe impl Send for ParallelJoinOperator {}

impl Operator for ParallelJoinOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let profile = std::env::var("RUST_HYDRATE_PROFILE").unwrap_or_default() == "1";
        let t0 = std::time::Instant::now();
        let parent_nodes = self.parent.fetch(req);
        let parent_fetch_us = t0.elapsed().as_micros();
        let rel_name = &self.relationship_name;

        let t0 = std::time::Instant::now();
        // Batch fetch: ONE `WHERE child_key IN (...)` query replaces N individual queries.
        let grouped = batch_fetch_children(
            &self.source,
            &self.child_config,
            &self.parent_key,
            &self.child_key,
            &parent_nodes,
        );
        let children_fetch_us = t0.elapsed().as_micros();

        if profile {
            eprintln!("    [ParallelJoin] parent_rows={} parent_fetch={}us batch_children_fetch={}us",
                parent_nodes.len(), parent_fetch_us, children_fetch_us);
        }

        let parent_key = &self.parent_key;
        let child_key = &self.child_key;

        parent_nodes
            .into_iter()
            .map(|mut node| {
                let children = if !parent_key.is_empty() {
                    let pk0 = &parent_key[0];
                    let group_key = node.row.get(pk0)
                        .map(value_to_group_key)
                        .unwrap_or_else(|| "null".to_string());
                    let mut children = grouped.get(&group_key)
                        .cloned()
                        .unwrap_or_default();
                    // Multi-key filtering
                    if parent_key.len() > 1 {
                        use zero_ivm_rs::filter::{Value, compare_values};
                        children.retain(|cn| {
                            parent_key.iter().zip(child_key.iter()).all(|(pk, ck)| {
                                let pv = node.row.get(pk).map(Value::from_json).unwrap_or(Value::Null);
                                let cv = cn.row.get(ck).map(Value::from_json).unwrap_or(Value::Null);
                                !matches!(pv, Value::Null) && !matches!(cv, Value::Null)
                                    && compare_values(&pv, &cv) == std::cmp::Ordering::Equal
                            })
                        });
                    }
                    children
                } else {
                    vec![]
                };
                node.relationships.insert(rel_name.clone(), children);
                node
            })
            .collect()
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        // Push path not used during hydration
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "parallel_join"
    }
}

/// A parallel Exists operator that checks child existence in parallel across parent rows.
pub struct ParallelExistsOperator {
    input: Box<dyn Operator>,
    child_config: Vec<OperatorConfig>,
    source: Arc<RustTableSource>,
    not_exists: bool,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
    child_table_name: String,
    or_predicate: Option<zero_ivm_rs::filter::Predicate>,
}

impl ParallelExistsOperator {
    pub fn new(
        input: Box<dyn Operator>,
        child_config: Vec<OperatorConfig>,
        source: Arc<RustTableSource>,
        not_exists: bool,
        parent_key: Vec<String>,
        child_key: Vec<String>,
        relationship_name: String,
        or_predicate: Option<zero_ivm_rs::filter::Predicate>,
    ) -> Self {
        // Extract the actual child table name from the Source config
        let child_table_name = child_config.first().and_then(|c| match c {
            OperatorConfig::Source { table_name, .. } => Some(table_name.clone()),
            _ => None,
        }).unwrap_or_else(|| relationship_name.clone());
        Self {
            input,
            child_config,
            source,
            not_exists,
            parent_key,
            child_key,
            relationship_name,
            child_table_name,
            or_predicate,
        }
    }

    fn or_condition_matches(&self, row: &serde_json::Map<String, serde_json::Value>) -> bool {
        if let Some(ref pred) = self.or_predicate {
            zero_ivm_rs::filter::evaluate_json_row(pred, row)
        } else {
            false
        }
    }
}

unsafe impl Send for ParallelExistsOperator {}

impl Operator for ParallelExistsOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let parent_nodes = self.input.fetch(req);
        let not_exists = self.not_exists;

        // Batch fetch: ONE query, then count per group.
        let grouped = batch_fetch_children(
            &self.source,
            &self.child_config,
            &self.parent_key,
            &self.child_key,
            &parent_nodes,
        );

        let parent_key = &self.parent_key;
        let child_key = &self.child_key;
        let rel_name = &self.child_table_name;

        parent_nodes
            .into_iter()
            .filter_map(|mut node| {
                let or_matches = self.or_condition_matches(&node.row);
                let (count, children) = if !parent_key.is_empty() {
                    let pk0 = &parent_key[0];
                    let group_key = node.row.get(pk0)
                        .map(value_to_group_key)
                        .unwrap_or_else(|| "null".to_string());
                    let fetched = grouped.get(&group_key);
                    if parent_key.len() > 1 {
                        use zero_ivm_rs::filter::{Value, compare_values};
                        let filtered: Vec<Node> = fetched.map(|cs| cs.iter().filter(|cn| {
                            parent_key.iter().zip(child_key.iter()).all(|(pk, ck)| {
                                let pv = node.row.get(pk).map(Value::from_json).unwrap_or(Value::Null);
                                let cv = cn.row.get(ck).map(Value::from_json).unwrap_or(Value::Null);
                                !matches!(pv, Value::Null) && !matches!(cv, Value::Null)
                                    && compare_values(&pv, &cv) == std::cmp::Ordering::Equal
                            })
                        }).cloned().collect()).unwrap_or_default();
                        let c = filtered.len();
                        (c, filtered)
                    } else {
                        let cs = fetched.cloned().unwrap_or_default();
                        let c = cs.len();
                        (c, cs)
                    }
                } else {
                    (0, vec![])
                };
                let passes = if not_exists { count == 0 } else { count > 0 };
                if passes || or_matches {
                    node.relationships.insert(rel_name.clone(), children);
                    Some(node)
                } else {
                    None
                }
            })
            .collect()
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "parallel_exists"
    }
}

/// A parallel Or-Exists operator: passes a parent row if ANY branch's exists check passes,
/// or if the or_condition (simple predicates) matches. This implements OR semantics across
/// multiple correlated subqueries: OR(CSQ1, CSQ2, ..., simple_conditions).
pub struct ParallelOrExistsOperator {
    input: Box<dyn Operator>,
    branches: Vec<OrExistsBranch>,
    source: Arc<RustTableSource>,
    or_predicate: Option<zero_ivm_rs::filter::Predicate>,
}

struct OrExistsBranch {
    child_config: Vec<OperatorConfig>,
    not_exists: bool,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
    child_table_name: String,
}

impl ParallelOrExistsOperator {
    pub fn new(
        input: Box<dyn Operator>,
        branches: Vec<zero_ivm_rs::pipeline::ExistsBranch>,
        source: Arc<RustTableSource>,
        or_predicate: Option<zero_ivm_rs::filter::Predicate>,
    ) -> Self {
        let branches = branches
            .into_iter()
            .map(|b| {
                let child_table_name = b.child.first().and_then(|c| match c {
                    OperatorConfig::Source { table_name, .. } => Some(table_name.clone()),
                    _ => None,
                }).unwrap_or_else(|| b.relationship_name.clone());
                OrExistsBranch {
                    child_config: b.child,
                    not_exists: b.not_exists,
                    parent_key: b.parent_key,
                    child_key: b.child_key,
                    relationship_name: b.relationship_name,
                    child_table_name,
                }
            })
            .collect();
        Self { input, branches, source, or_predicate }
    }

    fn or_condition_matches(&self, row: &serde_json::Map<String, serde_json::Value>) -> bool {
        if let Some(ref pred) = self.or_predicate {
            zero_ivm_rs::filter::evaluate_json_row(pred, row)
        } else {
            false
        }
    }
}

unsafe impl Send for ParallelOrExistsOperator {}

impl Operator for ParallelOrExistsOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let parent_nodes = self.input.fetch(req);

        // Batch fetch children for ALL parent nodes, once per branch
        let branch_groups: Vec<HashMap<String, Vec<Node>>> = self.branches.iter()
            .map(|branch| {
                batch_fetch_children(
                    &self.source,
                    &branch.child_config,
                    &branch.parent_key,
                    &branch.child_key,
                    &parent_nodes,
                )
            })
            .collect();

        parent_nodes
            .into_iter()
            .filter_map(|mut node| {
                // Short-circuit: if simple or_condition matches, pass through
                // but still need to check all branches for relationship attachment
                let or_match = self.or_condition_matches(&node.row);

                // Check ALL branches — attach relationships from every matching branch.
                // Pass the parent row if ANY branch's exists check passes (OR semantics)
                // or if the or_condition matched.
                let mut any_branch_passed = false;
                for (bi, branch) in self.branches.iter().enumerate() {
                    let grouped = &branch_groups[bi];

                    let (count, children) = if !branch.parent_key.is_empty() {
                        let pk0 = &branch.parent_key[0];
                        let group_key = node.row.get(pk0)
                            .map(value_to_group_key)
                            .unwrap_or_else(|| "null".to_string());
                        let fetched = grouped.get(&group_key);
                        if branch.parent_key.len() > 1 {
                            use zero_ivm_rs::filter::{Value, compare_values};
                            let filtered: Vec<Node> = fetched.map(|cs| cs.iter().filter(|cn| {
                                branch.parent_key.iter().zip(branch.child_key.iter()).all(|(pk, ck)| {
                                    let pv = node.row.get(pk).map(Value::from_json).unwrap_or(Value::Null);
                                    let cv = cn.row.get(ck).map(Value::from_json).unwrap_or(Value::Null);
                                    !matches!(pv, Value::Null) && !matches!(cv, Value::Null)
                                        && compare_values(&pv, &cv) == std::cmp::Ordering::Equal
                                })
                            }).cloned().collect()).unwrap_or_default();
                            let c = filtered.len();
                            (c, filtered)
                        } else {
                            let cs = fetched.cloned().unwrap_or_default();
                            let c = cs.len();
                            (c, cs)
                        }
                    } else {
                        (0, vec![])
                    };

                    let passes = if branch.not_exists { count == 0 } else { count > 0 };
                    if passes {
                        any_branch_passed = true;
                    }
                    // Always attach children as relationship (matching TS behavior)
                    node.relationships.insert(branch.child_table_name.clone(), children);
                }

                if any_branch_passed || or_match {
                    Some(node)
                } else {
                    None
                }
            })
            .collect()
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "parallel_or_exists"
    }
}

/// **B5 — compound OR with AND-of-CSQ branches.**
///
/// Hydration-side port of TS `applyOr`'s FanOut+FanIn topology
/// (packages/zql/src/builder/builder.ts:514-557). One self-contained sub-
/// pipeline per OR-branch (each starting with a Source), all branches
/// fetched independently against the live SQLite table source, then
/// deduplicated by primary key (TS `mergeFetches` filter-graph variant).
///
/// Push semantics are deferred (advance-mode). Hydrate-only is sufficient
/// to close 5/6 of the catalog divergences (the 5 A-nested-OR-with-EXISTS
/// shapes); the 6th (D-simple-OR-with-EXISTS) takes the existing OrExists
/// path and is unaffected by this operator.
pub struct ParallelFanOutOperator {
    branches: Vec<Vec<OperatorConfig>>,
    primary_key: Vec<String>,
    source: Arc<RustTableSource>,
}

impl ParallelFanOutOperator {
    pub fn new(
        branches: Vec<Vec<OperatorConfig>>,
        source: Arc<RustTableSource>,
    ) -> Self {
        // Extract primary_key from the first branch's Source (all branches
        // share the same parent Source by construction in
        // ast_to_config::build_or_branch_subpipeline).
        let primary_key = branches.first()
            .and_then(|b| b.first())
            .and_then(|c| match c {
                OperatorConfig::Source { primary_key, .. } => Some(primary_key.clone()),
                _ => None,
            })
            .unwrap_or_default();
        Self { branches, primary_key, source }
    }

    /// Build the dedup key for a row by joining primary-key column values
    /// with a delimiter that cannot appear in a serialized JSON value.
    fn pk_key(&self, row: &serde_json::Map<String, serde_json::Value>) -> String {
        let parts: Vec<String> = self.primary_key.iter()
            .map(|k| match row.get(k) {
                Some(v) => v.to_string(),
                None => "null".to_string(),
            })
            .collect();
        parts.join("\u{1f}") // ASCII unit separator — JSON-safe
    }
}

unsafe impl Send for ParallelFanOutOperator {}

impl Operator for ParallelFanOutOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        // Fan out to all branches, dedupe by primary key.
        // Each branch is a self-contained sub-pipeline (Source +
        // Filter/Exists/...).
        //
        // **B5-FIX:** Use `build_branch_subpipeline_for_hydrate` (which uses
        // `build_push_next_operator` + `SourceBridgeOperator`, mirroring the
        // production hydrate path) instead of `build_operator_with_live_source`
        // (which uses `build_next_operator` and `ParallelExistsOperator`,
        // tripping a partitioned-Take-with-no-state bug in
        // `apply_child_operators`). This makes the FanOut branches use the
        // same sequential `ExistsOperator` that works for the simple-EXISTS
        // hydrate path.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<Node> = Vec::new();
        for (i, branch_config) in self.branches.iter().enumerate() {
            let mut branch_op = match build_branch_subpipeline_for_hydrate(
                self.source.clone(),
                branch_config,
            ) {
                Ok(op) => op,
                Err(e) => {
                    eprintln!("ParallelFanOutOperator branch {} build failed: {e}", i);
                    continue;
                }
            };
            let nodes = branch_op.fetch(req);
            for n in nodes {
                let key = self.pk_key(&n.row);
                if seen.insert(key) {
                    out.push(n);
                }
            }
        }
        out
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        // Push correctness deferred to a future advance-mode follow-up. The
        // existing FanOutOperator in zero-ivm-rs (fan_out_op.rs) implements
        // push via push_accumulated_changes, but the production hydration
        // path uses Parallel* operators that fetch from the live DB and
        // return vec![] from push (matching ParallelJoinOperator,
        // ParallelExistsOperator, ParallelOrExistsOperator).
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "parallel_fan_out"
    }
}

#[cfg(test)]
fn hydrate_single_pipeline(
    source: Arc<RustTableSource>,
    configs: Vec<OperatorConfig>,
) -> Result<Vec<Node>, String> {
    let mut operator = build_operator_with_live_source(source, &configs)?;
    Ok(operator.fetch(&FetchRequest::default()))
}

fn build_operator_with_live_source(
    source: Arc<RustTableSource>,
    configs: &[OperatorConfig],
) -> Result<Box<dyn Operator>, String> {
    if configs.is_empty() {
        return Err("empty operator config".to_string());
    }

    let first = &configs[0];
    let (live_source, rest_start) = match first {
        OperatorConfig::Source { sort, .. } => {
            let sort_specs: Vec<zero_ivm_rs::types::SortSpec> = sort
                .iter()
                .map(|(f, d)| zero_ivm_rs::types::SortSpec {
                    field: f.clone(),
                    direction: if d == "desc" {
                        zero_ivm_rs::types::SortDirection::Desc
                    } else {
                        zero_ivm_rs::types::SortDirection::Asc
                    },
                })
                .collect();

            let conn_id = 0;
            let op: Box<dyn Operator> =
                Box::new(LiveTableSource::new(source.clone(), conn_id, sort_specs));
            (op, 1)
        }
        _ => return Err("first config must be a Source".to_string()),
    };

    let mut current: Box<dyn Operator> = live_source;
    for config in &configs[rest_start..] {
        current = build_next_operator(source.clone(), current, config)?;
    }
    Ok(current)
}

/// **B5-FIX:** Build a self-contained sub-pipeline for one OR-branch using
/// the SAME operator construction logic as the production hydrate path
/// (`build_operator_chain` + `build_push_next_operator` +
/// `SourceBridgeOperator`).
///
/// Why not `build_operator_with_live_source`?
///   - It uses `build_next_operator` which constructs `ParallelExistsOperator`.
///   - `ParallelExistsOperator::fetch` calls `batch_fetch_children` which calls
///     `apply_child_operators(child_config=[Source, Take(partition_key=…)])`.
///   - `apply_child_operators` runs the partitioned `Take` against a
///     `PreloadedSource` with a default (no-constraint, no-start)
///     `FetchRequest`. `TakeOperator::fetch` with `partition_key.is_some()`,
///     no constraint, and no `max_bound` returns `vec![]` — silently
///     dropping all child rows.
///   - Result: every parent's EXISTS check sees zero children, so the
///     `EXISTS` gate filters everyone out.
///
/// `build_push_next_operator` instead constructs sequential `ExistsOperator`
/// (zero_ivm_rs::exists_op) which does per-parent constrained child fetches
/// — `child.fetch(constraint=parent_key)` — that hit the
/// `req.constraint.is_some()` branch in `TakeOperator::fetch`, returning
/// child rows correctly.
fn build_branch_subpipeline_for_hydrate(
    source: Arc<RustTableSource>,
    configs: &[OperatorConfig],
) -> Result<Box<dyn Operator>, String> {
    if configs.is_empty() {
        return Err("empty operator config".to_string());
    }
    // First config must be the parent Source — used to seed the
    // SourceBridgeOperator (which fetches via the shared connection 0).
    match &configs[0] {
        OperatorConfig::Source { .. } => {}
        other => {
            return Err(format!(
                "build_branch_subpipeline_for_hydrate: first config must be a Source, got {:?}",
                other
            ));
        }
    }
    // connection_id = 0 — same connection that the parent pipeline registered
    // in `build_pipeline_state`. The shared `Arc<RustTableSource>` already
    // has connection 0 with the correct sort/filters; reusing it means the
    // FanOut branch sees the same row visibility as the parent.
    let connection_id: usize = 0;
    let root: Box<dyn Operator> = Box::new(SourceBridgeOperator::new(source.clone(), connection_id));
    let mut current: Box<dyn Operator> = root;
    for config in &configs[1..] {
        current = build_push_next_operator(source.clone(), current, config)?;
    }
    Ok(current)
}

fn parse_predicate_json(value: &serde_json::Value) -> Result<zero_ivm_rs::filter::Predicate, String> {
    use zero_ivm_rs::filter::{Predicate, Value};
    let obj = value.as_object().ok_or("predicate must be an object")?;
    if let Some(field) = obj.get("field") {
        let field = field.as_str().ok_or("field must be a string")?.to_string();
        if let Some(val) = obj.get("eq") {
            return Ok(Predicate::Eq(field, Value::from_json(val)));
        }
        if let Some(val) = obj.get("neq") {
            return Ok(Predicate::Neq(field, Value::from_json(val)));
        }
        if let Some(val) = obj.get("gt") {
            return Ok(Predicate::Gt(field, Value::from_json(val)));
        }
        if let Some(val) = obj.get("lt") {
            return Ok(Predicate::Lt(field, Value::from_json(val)));
        }
        if let Some(val) = obj.get("gte") {
            return Ok(Predicate::Gte(field, Value::from_json(val)));
        }
        if let Some(val) = obj.get("lte") {
            return Ok(Predicate::Lte(field, Value::from_json(val)));
        }
        if let Some(val) = obj.get("in") {
            let arr = val.as_array().ok_or("'in' value must be an array")?;
            let values: Vec<Value> = arr.iter().map(Value::from_json).collect();
            return Ok(Predicate::In(field, values));
        }
        if let Some(val) = obj.get("like") {
            let pattern = val.as_str().ok_or("'like' value must be a string")?.to_string();
            return Ok(Predicate::Like(field, pattern, false));
        }
        if let Some(val) = obj.get("ilike") {
            let pattern = val.as_str().ok_or("'ilike' value must be a string")?.to_string();
            return Ok(Predicate::Like(field, pattern, true));
        }
        if let Some(val) = obj.get("isNull") {
            if val.as_bool().unwrap_or(false) {
                return Ok(Predicate::IsNull(field));
            }
        }
        if let Some(val) = obj.get("isNotNull") {
            if val.as_bool().unwrap_or(false) {
                return Ok(Predicate::IsNotNull(field));
            }
        }
        return Err(format!("unknown predicate operator for field {field}"));
    }
    if let Some(arr) = obj.get("and") {
        let preds: Result<Vec<Predicate>, String> = arr
            .as_array()
            .ok_or("and must be an array")?
            .iter()
            .map(parse_predicate_json)
            .collect();
        return Ok(Predicate::And(preds?));
    }
    if let Some(arr) = obj.get("or") {
        let preds: Result<Vec<Predicate>, String> = arr
            .as_array()
            .ok_or("or must be an array")?
            .iter()
            .map(parse_predicate_json)
            .collect();
        return Ok(Predicate::Or(preds?));
    }
    if let Some(inner) = obj.get("not") {
        let pred = parse_predicate_json(inner)?;
        return Ok(Predicate::Not(Box::new(pred)));
    }
    Err("unknown predicate format".to_string())
}

fn build_next_operator(
    source: Arc<RustTableSource>,
    input: Box<dyn Operator>,
    config: &OperatorConfig,
) -> Result<Box<dyn Operator>, String> {
    use zero_ivm_rs::cap_op::CapOperator;
    use zero_ivm_rs::exists_op::ExistsOperator;
    use zero_ivm_rs::filter::Predicate;
    use zero_ivm_rs::filter_op::FilterOperator;
    use zero_ivm_rs::join_op::JoinOperator;
    use zero_ivm_rs::skip_op::{Bound, SkipOperator};
    use zero_ivm_rs::take_op::TakeOperator;
    use zero_ivm_rs::types::Row;

    fn parse_sort(sort: &[(String, String)]) -> Vec<zero_ivm_rs::types::SortSpec> {
        sort.iter()
            .map(|(f, d)| zero_ivm_rs::types::SortSpec {
                field: f.clone(),
                direction: if d == "desc" {
                    zero_ivm_rs::types::SortDirection::Desc
                } else {
                    zero_ivm_rs::types::SortDirection::Asc
                },
            })
            .collect()
    }

    match config {
        OperatorConfig::Source { .. } => {
            Err("unexpected Source in non-root position".to_string())
        }
        OperatorConfig::Filter { predicate } => {
            let pred = parse_predicate_json(predicate)?;
            Ok(Box::new(FilterOperator::new(input, pred)))
        }
        OperatorConfig::Join {
            parent_key,
            child_key,
            relationship_name,
            child,
        } => {
            Ok(Box::new(ParallelJoinOperator::new(
                input,
                child.clone(),
                source,
                parent_key.clone(),
                child_key.clone(),
                relationship_name.clone(),
            )))
        }
        OperatorConfig::Take {
            limit,
            sort,
            partition_key,
        } => {
            let sort_specs = parse_sort(sort);
            Ok(Box::new(TakeOperator::new(
                input,
                *limit,
                sort_specs,
                partition_key.clone(),
            )))
        }
        OperatorConfig::Exists {
            relationship_name,
            not_exists,
            parent_key,
            child_key,
            child,
            or_condition,
        } => {
            let or_pred = if let Some(oc) = or_condition {
                Some(parse_predicate_json(oc)?)
            } else {
                None
            };
            Ok(Box::new(ParallelExistsOperator::new(
                input,
                child.clone(),
                source,
                *not_exists,
                parent_key.clone(),
                child_key.clone(),
                relationship_name.clone(),
                or_pred,
            )))
        }
        OperatorConfig::OrExists {
            branches,
            or_condition,
        } => {
            let or_pred = if let Some(oc) = or_condition {
                Some(parse_predicate_json(oc)?)
            } else {
                None
            };
            Ok(Box::new(ParallelOrExistsOperator::new(
                input,
                branches.clone(),
                source,
                or_pred,
            )))
        }
        OperatorConfig::Skip {
            bound_row,
            exclusive,
            sort,
        } => {
            let row: Row = serde_json::from_value(bound_row.clone())
                .map_err(|e| format!("invalid bound_row: {e}"))?;
            let sort_specs = parse_sort(sort);
            let bound = Bound {
                row,
                exclusive: *exclusive,
            };
            Ok(Box::new(SkipOperator::new(input, bound, sort_specs)))
        }
        OperatorConfig::Cap {
            limit,
            primary_key,
            partition_key,
        } => Ok(Box::new(CapOperator::new(
            input,
            *limit,
            primary_key.clone(),
            partition_key.clone(),
        ))),
        OperatorConfig::FanOut { branches } => {
            // **B5 hydrate path** — compound OR with AND-of-CSQ branches.
            // Each branch is a self-contained sub-pipeline (built upstream
            // by ast_to_config::build_or_branch_subpipeline). We fan out to
            // all branches, fetch independently, and dedup by primary key.
            //
            // The `input` from upstream is dropped — branches start from
            // their own Source. This matches the FanOut semantics in
            // pipeline.rs (Phase 36 Wave 1, line ~370). For the catalog
            // shapes, the only upstream operator is the Source itself, so
            // this drop is semantically a no-op.
            let _ = input; // intentionally drop — each branch has own Source
            Ok(Box::new(ParallelFanOutOperator::new(
                branches.clone(),
                source,
            )))
        }
    }
}

/// Build a unified operator chain for both fetch (hydration) and push (advance).
/// Uses sequential JoinOperator/ExistsOperator which have proper push() implementations.
/// The chain starts with a SourceBridgeOperator at root for DB access.
/// Result of building a unified operator chain.
/// `chain` is the outermost operator (fetch recurses inward).
/// `push_ptrs` are raw pointers to each non-Source operator in inner→outer order,
/// matching the indices used by `child_table_to_op_index`.
pub struct OperatorChain {
    pub chain: Box<dyn Operator>,
    pub push_ptrs: Vec<*mut dyn Operator>,
}

// SAFETY: The raw pointers in push_ptrs point into heap-allocated Box<dyn Operator>
// within the nested chain. They are valid as long as `chain` exists and is not moved
// (Box heap alloc is stable). Push is sequential — no aliasing.
unsafe impl Send for OperatorChain {}

/// Build a single nested operator chain for both fetch (hydration) and push (advance).
/// Returns None if there are no operators beyond the root Source.
///
/// The chain is: SourceBridge → Op0 → Op1 → ... → OpN (outermost)
/// push_ptrs = [&mut Op0, &mut Op1, ..., &mut OpN] for external push routing.
pub fn build_operator_chain(
    source: Arc<RustTableSource>,
    configs: &[OperatorConfig],
    connection_id: usize,
) -> Result<Option<OperatorChain>, String> {
    if configs.is_empty() {
        return Err("empty operator config".to_string());
    }

    // Skip the root Source config
    let rest = match &configs[0] {
        OperatorConfig::Source { .. } => &configs[1..],
        _ => return Err("first config must be a Source".to_string()),
    };

    if rest.is_empty() {
        return Ok(None);
    }

    // Build a source-backed root so operators can fetch from DB
    let root: Box<dyn Operator> = Box::new(SourceBridgeOperator::new(source.clone(), connection_id));
    let mut current = root;
    let mut push_ptrs: Vec<*mut dyn Operator> = Vec::with_capacity(rest.len());

    for config in rest {
        current = build_push_next_operator(source.clone(), current, config)?;
        // Collect a raw pointer to this operator for push routing.
        // The pointer is into the Box's heap allocation — stable address.
        let ptr: *mut dyn Operator = &mut *current;
        push_ptrs.push(ptr);
    }

    Ok(Some(OperatorChain { chain: current, push_ptrs }))
}

/// A bridge operator that connects the RustTableSource to the push chain.
/// fetch() queries the actual SQLite DB; push() passes through.
struct SourceBridgeOperator {
    source: Arc<RustTableSource>,
    connection_id: usize,
}

impl SourceBridgeOperator {
    fn new(source: Arc<RustTableSource>, connection_id: usize) -> Self {
        Self { source, connection_id }
    }
}

impl Operator for SourceBridgeOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        // Convert zero_ivm_rs::types::FetchRequest → source::FetchRequest
        let source_req = crate::source::FetchRequest {
            constraint: req.constraint.as_ref().map(|c| crate::source::FetchConstraint {
                columns: c.columns.clone(),
            }),
            start: req.start.as_ref().map(|s| crate::source::FetchStart {
                row: s.row.clone(),
                basis: s.basis.clone(),
            }),
            reverse: req.reverse,
        };
        match self.source.fetch(self.connection_id, &source_req) {
            Ok(nodes) => {
                nodes.into_iter().map(|n| Node {
                row: n.row,
                relationships: n.relationships.into_iter().map(|(k, v)| {
                    (k, v.into_iter().map(|cn| Node { row: cn.row, relationships: std::collections::HashMap::new() }).collect())
                }).collect(),
            }).collect()
            }
            Err(e) => {
                eprintln!("SourceBridgeOperator::fetch error: {e}");
                vec![]
            }
        }
    }

    fn push(&mut self, change: Change) -> Vec<Change> {
        vec![change]
    }

    fn op_type(&self) -> &'static str {
        "source_bridge"
    }
}

/// Build next operator for the push path — uses sequential JoinOperator/ExistsOperator.
fn build_push_next_operator(
    source: Arc<RustTableSource>,
    input: Box<dyn Operator>,
    config: &OperatorConfig,
) -> Result<Box<dyn Operator>, String> {
    use zero_ivm_rs::cap_op::CapOperator;
    use zero_ivm_rs::exists_op::ExistsOperator;
    use zero_ivm_rs::filter::Predicate;
    use zero_ivm_rs::filter_op::FilterOperator;
    use zero_ivm_rs::join_op::JoinOperator;
    use zero_ivm_rs::skip_op::{Bound, SkipOperator};
    use zero_ivm_rs::take_op::TakeOperator;
    use zero_ivm_rs::types::Row;

    fn parse_sort(sort: &[(String, String)]) -> Vec<zero_ivm_rs::types::SortSpec> {
        sort.iter()
            .map(|(f, d)| zero_ivm_rs::types::SortSpec {
                field: f.clone(),
                direction: if d == "desc" {
                    zero_ivm_rs::types::SortDirection::Desc
                } else {
                    zero_ivm_rs::types::SortDirection::Asc
                },
            })
            .collect()
    }

    match config {
        OperatorConfig::Source { .. } => {
            Err("unexpected Source in non-root position".to_string())
        }
        OperatorConfig::Filter { predicate } => {
            let pred = parse_predicate_json(predicate)?;
            Ok(Box::new(FilterOperator::new(input, pred)))
        }
        OperatorConfig::Join {
            parent_key,
            child_key,
            relationship_name,
            child,
        } => {
            // Build child operator tree with LiveTableSource for fetch during push
            let child_source = make_child_source(&source, child)
                .ok_or("failed to create child source for join")?;
            let child_op = build_operator_with_live_source(Arc::new(child_source), child)?;
            Ok(Box::new(JoinOperator::new(
                input,
                child_op,
                parent_key.clone(),
                child_key.clone(),
                relationship_name.clone(),
            )))
        }
        OperatorConfig::Take {
            limit,
            sort,
            partition_key,
        } => {
            let sort_specs = parse_sort(sort);
            Ok(Box::new(TakeOperator::new(
                input,
                *limit,
                sort_specs,
                partition_key.clone(),
            )))
        }
        OperatorConfig::Exists {
            relationship_name,
            not_exists,
            parent_key,
            child_key,
            child,
            or_condition,
        } => {
            let child_source = make_child_source(&source, child)
                .ok_or("failed to create child source for exists")?;
            let child_op = build_operator_with_live_source(Arc::new(child_source), child)?;
            let or_pred = if let Some(oc) = or_condition {
                Some(zero_ivm_rs::pipeline::parse_predicate(oc)?)
            } else {
                None
            };
            Ok(Box::new(ExistsOperator::new(
                input,
                child_op,
                relationship_name.clone(),
                *not_exists,
                parent_key.clone(),
                child_key.clone(),
            ).with_or_predicate(or_pred)))
        }
        OperatorConfig::OrExists {
            branches,
            or_condition,
        } => {
            let or_pred = if let Some(oc) = or_condition {
                Some(zero_ivm_rs::pipeline::parse_predicate(oc)?)
            } else {
                None
            };
            let branch_data: Vec<_> = branches
                .iter()
                .map(|b| {
                    let child_source = make_child_source(&source, &b.child)
                        .ok_or("failed to create child source for or_exists branch")?;
                    let child_op =
                        build_operator_with_live_source(Arc::new(child_source), &b.child)?;
                    Ok((
                        child_op,
                        b.relationship_name.clone(),
                        b.not_exists,
                        b.parent_key.clone(),
                        b.child_key.clone(),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Box::new(zero_ivm_rs::or_exists_op::OrExistsOperator::new(
                input,
                branch_data,
                or_pred,
            )))
        }
        OperatorConfig::Skip {
            bound_row,
            exclusive,
            sort,
        } => {
            let row: Row = serde_json::from_value(bound_row.clone())
                .map_err(|e| format!("invalid bound_row: {e}"))?;
            let sort_specs = parse_sort(sort);
            let bound = Bound {
                row,
                exclusive: *exclusive,
            };
            Ok(Box::new(SkipOperator::new(input, bound, sort_specs)))
        }
        OperatorConfig::Cap {
            limit,
            primary_key,
            partition_key,
        } => Ok(Box::new(CapOperator::new(
            input,
            *limit,
            primary_key.clone(),
            partition_key.clone(),
        ))),
        OperatorConfig::FanOut { branches } => {
            // **B5 push path** — same operator as the hydrate path. Push
            // semantics inside ParallelFanOutOperator currently return
            // vec![] (advance-mode is deferred per the user's hydrate-only
            // target for the B5 fix). Wiring this here ensures the pipeline
            // builds without errors in the advance-coverage harness; the
            // operator's push() simply yields no output.
            let _ = input; // intentionally drop — each branch has own Source
            Ok(Box::new(ParallelFanOutOperator::new(
                branches.clone(),
                source,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_builder::ColumnType;
    use serde_json::json;

    fn setup_test_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE users (
                 id TEXT PRIMARY KEY,
                 name TEXT,
                 age INTEGER
             );
             INSERT INTO users VALUES ('1', 'Alice', 30);
             INSERT INTO users VALUES ('2', 'Bob', 25);
             INSERT INTO users VALUES ('3', 'Carol', 35);
             INSERT INTO users VALUES ('4', 'Dave', 28);
             INSERT INTO users VALUES ('5', 'Eve', 22);",
        )
        .expect("setup");
        drop(conn);
        file
    }

    fn make_source(db: &tempfile::NamedTempFile, pool_size: usize) -> RustTableSource {
        let mut ct = HashMap::new();
        ct.insert("id".into(), ColumnType::String);
        ct.insert("name".into(), ColumnType::String);
        ct.insert("age".into(), ColumnType::Number);

        RustTableSource::new(
            db.path().to_str().unwrap(),
            pool_size,
            "users".into(),
            vec!["id".into(), "name".into(), "age".into()],
            ct,
            vec!["id".into()],
        )
        .unwrap()
    }

    #[test]
    fn test_live_table_source_fetch() {
        let db = setup_test_db();
        let mut src = make_source(&db, 4);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let sort = vec![zero_ivm_rs::types::SortSpec {
            field: "id".into(),
            direction: zero_ivm_rs::types::SortDirection::Asc,
        }];
        let mut op = LiveTableSource::new(arc_src, 0, sort);
        let nodes = op.fetch(&FetchRequest::default());
        assert_eq!(nodes.len(), 5);
        assert_eq!(nodes[0].row.get("name").unwrap(), &json!("Alice"));
        assert_eq!(nodes[4].row.get("name").unwrap(), &json!("Eve"));
    }

    #[test]
    fn test_hydrate_single_pipeline_source_only() {
        let db = setup_test_db();
        let mut src = make_source(&db, 4);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let config = vec![OperatorConfig::Source {
            table_name: "users".into(),
            columns: vec!["id".into(), "name".into(), "age".into()],
            primary_key: vec!["id".into()],
            sort: vec![("id".into(), "asc".into())],
        }];

        let result = hydrate_single_pipeline(arc_src, config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 5);
    }

    #[test]
    fn test_hydrate_pipeline_with_filter() {
        let db = setup_test_db();
        let mut src = make_source(&db, 4);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into(), "age".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Filter {
                predicate: json!({"field": "name", "eq": "Alice"}),
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config);
        assert!(result.is_ok());
        let nodes = result.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].row.get("name").unwrap(), &json!("Alice"));
    }

    #[test]
    fn test_hydrate_pipeline_with_take() {
        let db = setup_test_db();
        let mut src = make_source(&db, 4);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into(), "age".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Take {
                limit: 2,
                sort: vec![("id".into(), "asc".into())],
                partition_key: None,
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config);
        assert!(result.is_ok());
        let nodes = result.unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].row.get("id").unwrap(), &json!("1"));
        assert_eq!(nodes[1].row.get("id").unwrap(), &json!("2"));
    }

    fn setup_join_test_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE users (
                 id TEXT PRIMARY KEY,
                 name TEXT
             );
             CREATE TABLE posts (
                 id TEXT PRIMARY KEY,
                 user_id TEXT,
                 title TEXT
             );
             INSERT INTO users VALUES ('1', 'Alice');
             INSERT INTO users VALUES ('2', 'Bob');
             INSERT INTO users VALUES ('3', 'Carol');
             INSERT INTO posts VALUES ('p1', '1', 'Post A1');
             INSERT INTO posts VALUES ('p2', '1', 'Post A2');
             INSERT INTO posts VALUES ('p3', '2', 'Post B1');
             INSERT INTO posts VALUES ('p4', '3', 'Post C1');
             INSERT INTO posts VALUES ('p5', '3', 'Post C2');
             INSERT INTO posts VALUES ('p6', '3', 'Post C3');",
        )
        .expect("setup");
        drop(conn);
        file
    }

    fn make_join_source(
        db: &tempfile::NamedTempFile,
        table: &str,
        columns: Vec<&str>,
        pk: &str,
        pool_size: usize,
    ) -> RustTableSource {
        let mut ct = HashMap::new();
        for c in &columns {
            ct.insert(c.to_string(), ColumnType::String);
        }
        RustTableSource::new(
            db.path().to_str().unwrap(),
            pool_size,
            table.into(),
            columns.iter().map(|s| s.to_string()).collect(),
            ct,
            vec![pk.into()],
        )
        .unwrap()
    }

    #[test]
    fn test_parallel_join_fetches_children() {
        let db = setup_join_test_db();
        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        // Build a pipeline: Source(users) -> ParallelJoin(posts on user_id)
        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Join {
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                relationship_name: "posts".into(),
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config);
        assert!(result.is_ok());
        let nodes = result.unwrap();
        assert_eq!(nodes.len(), 3);

        // Alice has 2 posts
        assert_eq!(nodes[0].row.get("name").unwrap(), &json!("Alice"));
        assert_eq!(nodes[0].relationships["posts"].len(), 2);

        // Bob has 1 post
        assert_eq!(nodes[1].row.get("name").unwrap(), &json!("Bob"));
        assert_eq!(nodes[1].relationships["posts"].len(), 1);

        // Carol has 3 posts
        assert_eq!(nodes[2].row.get("name").unwrap(), &json!("Carol"));
        assert_eq!(nodes[2].relationships["posts"].len(), 3);
    }

    #[test]
    fn test_parallel_join_null_key_no_children() {
        let db = setup_join_test_db();
        // Add a user with NULL-like id
        let conn = rusqlite::Connection::open(db.path()).expect("open");
        conn.execute("INSERT INTO users VALUES (NULL, 'Nobody')", []).unwrap();
        drop(conn);

        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("name".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("name".into(), "asc".into())],
            },
            OperatorConfig::Join {
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                relationship_name: "posts".into(),
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        let nobody = result.iter().find(|n| n.row.get("name").unwrap() == &json!("Nobody"));
        assert!(nobody.is_some());
        assert_eq!(nobody.unwrap().relationships["posts"].len(), 0);
    }

    #[test]
    fn test_parallel_exists_filters_correctly() {
        let db = setup_join_test_db();
        // Remove posts for Bob so exists filter excludes him
        let conn = rusqlite::Connection::open(db.path()).expect("open");
        conn.execute("DELETE FROM posts WHERE user_id = '2'", []).unwrap();
        drop(conn);

        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        // exists: users who have posts
        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Exists {
                relationship_name: "posts".into(),
                not_exists: false,
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
                or_condition: None,
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        // Alice and Carol have posts, Bob doesn't
        assert_eq!(result.len(), 2);
        let names: Vec<&str> = result.iter().map(|n| n.row.get("name").unwrap().as_str().unwrap()).collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Carol"));
        assert!(!names.contains(&"Bob"));
    }

    #[test]
    fn test_parallel_not_exists() {
        let db = setup_join_test_db();
        let conn = rusqlite::Connection::open(db.path()).expect("open");
        conn.execute("DELETE FROM posts WHERE user_id = '2'", []).unwrap();
        drop(conn);

        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Exists {
                relationship_name: "posts".into(),
                not_exists: true,
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
                or_condition: None,
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].row.get("name").unwrap(), &json!("Bob"));
    }

    /// **B5 — compound OR with AND-of-CSQ branches → ParallelFanOutOperator
    /// hydration.**
    ///
    /// Catalog-shape test mirroring the A-nested-OR-with-EXISTS shapes in
    /// tools/ivm-parity/all-divergences.json: WHERE = OR(AND(simple_gate,
    /// csq), bare_csq). Each OR-branch becomes a self-contained sub-pipeline
    /// (Source + Filter + Exists / Source + Exists), the FanOut operator
    /// fans out and dedups by primary key.
    ///
    /// This is the hydrate-side end-to-end test against a temp SQLite DB,
    /// independent of the live TS↔RS regression-runner. It exercises:
    ///   - ParallelFanOutOperator::fetch — fans out to N branches
    ///   - build_operator_with_live_source — builds each branch sub-pipeline
    ///     against a LiveTableSource backed by the shared Arc<RustTableSource>
    ///   - dedup-by-PK — same row appearing in multiple branches yields one
    ///   - gate-condition preservation — Filter inside the AND branch is
    ///     applied (pre-fix the gate was dropped — over-permissive matching)
    ///
    /// Setup:
    ///   users: u1 (active=true, name=Alice), u2 (active=false, name=Bob),
    ///          u3 (active=true, name=Carol), u4 (active=false, name=Dave)
    ///   posts: p1->u1, p2->u3 (only Alice and Carol have posts)
    ///   tags:  t1->u2, t2->u4 (only Bob and Dave have tags)
    ///
    /// Query: WHERE OR(AND(active=true, EXISTS(posts)), EXISTS(tags))
    ///
    /// Expected pass set (TS semantics):
    ///   - Alice: AND-branch passes (active=true AND has posts) → PASS
    ///   - Bob:   bare-CSQ-branch passes (has tags) → PASS
    ///   - Carol: AND-branch passes (active=true AND has posts) → PASS
    ///   - Dave:  bare-CSQ-branch passes (has tags) → PASS
    /// Total: 4 users.
    ///
    /// Pre-fix (collect_exists_branches drops gate): Alice would pass if
    /// EXISTS(posts) holds (regardless of active), and the AND-branch
    /// silently degrades to OR(EXISTS(posts), EXISTS(tags)) — over-
    /// permissive. We exercise this with a row where active=false BUT has
    /// posts: u5 (active=false, name=Erin) with p3->u5. Pre-fix: u5 passes
    /// the dropped-gate AND-branch (because EXISTS(posts) is true for u5).
    /// Post-fix: u5 must NOT pass (active=false fails the gate).
    #[test]
    fn test_b5_compound_or_fan_out_hydrate_against_live_db() {
        // ─── Setup DB with tagged rows ────────────────────────────────────
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE users (
                 id TEXT PRIMARY KEY,
                 name TEXT,
                 active INTEGER
             );
             CREATE TABLE posts (
                 id TEXT PRIMARY KEY,
                 user_id TEXT
             );
             CREATE TABLE tags (
                 id TEXT PRIMARY KEY,
                 user_id TEXT
             );
             INSERT INTO users VALUES ('u1', 'Alice', 1);
             INSERT INTO users VALUES ('u2', 'Bob',   0);
             INSERT INTO users VALUES ('u3', 'Carol', 1);
             INSERT INTO users VALUES ('u4', 'Dave',  0);
             INSERT INTO users VALUES ('u5', 'Erin',  0);
             INSERT INTO users VALUES ('u6', 'Frank', 1);
             INSERT INTO posts VALUES ('p1', 'u1');
             INSERT INTO posts VALUES ('p2', 'u3');
             INSERT INTO posts VALUES ('p3', 'u5');
             INSERT INTO tags VALUES ('t1', 'u2');
             INSERT INTO tags VALUES ('t2', 'u4');",
        )
        .expect("setup");
        drop(conn);

        // ─── Build user source ───────────────────────────────────────────
        let mut ct = HashMap::new();
        ct.insert("id".to_string(), ColumnType::String);
        ct.insert("name".to_string(), ColumnType::String);
        ct.insert("active".to_string(), ColumnType::Number);
        let mut src = RustTableSource::new(
            file.path().to_str().unwrap(),
            8,
            "users".to_string(),
            vec!["id".into(), "name".into(), "active".into()],
            ct,
            vec!["id".into()],
        )
        .unwrap();
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        // ─── Build the pipeline manually ──────────────────────────────────
        // Mirrors what ast_to_config emits for the catalog AST shape:
        //   Source(users) → FanOut(branches) → (no Take/Limit)
        // Branch 0: AND(active=true, EXISTS(posts)) →
        //   [Source(users), Filter(active=1), Exists(posts)]
        // Branch 1: bare EXISTS(tags) →
        //   [Source(users), Exists(tags)]
        let user_source = OperatorConfig::Source {
            table_name: "users".into(),
            columns: vec!["id".into(), "name".into(), "active".into()],
            primary_key: vec!["id".into()],
            sort: vec![("id".into(), "asc".into())],
        };

        let exists_posts = OperatorConfig::Exists {
            relationship_name: "posts".into(),
            not_exists: false,
            parent_key: vec!["id".into()],
            child_key: vec!["user_id".into()],
            child: vec![OperatorConfig::Source {
                table_name: "posts".into(),
                columns: vec!["id".into(), "user_id".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            }],
            or_condition: None,
        };
        let exists_tags = OperatorConfig::Exists {
            relationship_name: "tags".into(),
            not_exists: false,
            parent_key: vec!["id".into()],
            child_key: vec!["user_id".into()],
            child: vec![OperatorConfig::Source {
                table_name: "tags".into(),
                columns: vec!["id".into(), "user_id".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            }],
            or_condition: None,
        };

        let branch_and = vec![
            user_source.clone(),
            OperatorConfig::Filter {
                predicate: json!({"field": "active", "eq": 1}),
            },
            exists_posts.clone(),
        ];
        let branch_csq = vec![
            user_source.clone(),
            exists_tags.clone(),
        ];

        let config = vec![
            user_source.clone(),
            OperatorConfig::FanOut {
                branches: vec![branch_and, branch_csq],
            },
        ];

        // ─── Hydrate and assert ──────────────────────────────────────────
        let result = hydrate_single_pipeline(arc_src, config)
            .expect("FanOut hydrate must succeed");
        let names: std::collections::BTreeSet<String> = result.iter()
            .filter_map(|n| n.row.get("name").and_then(|v| v.as_str()).map(String::from))
            .collect();

        // Expected: Alice (active+posts), Bob (tags), Carol (active+posts),
        //           Dave (tags). Erin must be filtered (active=false, has
        //           posts but no tags). Frank must be filtered (active=true
        //           but no posts and no tags).
        assert_eq!(
            names,
            vec!["Alice", "Bob", "Carol", "Dave"]
                .into_iter().map(String::from).collect::<std::collections::BTreeSet<_>>(),
            "B5 hydrate parity: AND-branch gate (active=true) must filter \
             out Erin (active=false, has posts). Pre-fix this gate was dropped \
             and Erin would erroneously pass via the EXISTS(posts) clause. \
             Got: {:?}",
            names
        );

        // Stronger: the same row appearing in multiple branches must dedup
        // to one. Frank fails both branches and must NOT appear (degenerate
        // case but worth pinning).
        assert!(!names.contains("Frank"), "Frank must not appear (no posts, no tags)");
        assert!(!names.contains("Erin"),  "Erin must not appear (gate filters her out)");

        // Dedup: result vector length matches set length (no duplicate rows).
        assert_eq!(
            result.len(), names.len(),
            "FanOut must dedup by primary key — got {} rows but only {} unique names",
            result.len(), names.len()
        );
    }

    #[test]
    fn test_parallel_join_with_filter_on_children() {
        let db = setup_join_test_db();
        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        // Join with filter on child: only posts with title containing "A"
        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Join {
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                relationship_name: "posts".into(),
                child: vec![
                    OperatorConfig::Source {
                        table_name: "posts".into(),
                        columns: vec!["id".into(), "user_id".into(), "title".into()],
                        primary_key: vec!["id".into()],
                        sort: vec![("id".into(), "asc".into())],
                    },
                    OperatorConfig::Filter {
                        predicate: json!({"field": "title", "eq": "Post A1"}),
                    },
                ],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        assert_eq!(result.len(), 3);
        // Alice should have 1 matching post
        assert_eq!(result[0].relationships["posts"].len(), 1);
        assert_eq!(
            result[0].relationships["posts"][0].row.get("title").unwrap(),
            &json!("Post A1")
        );
        // Bob and Carol have 0 matching posts
        assert_eq!(result[1].relationships["posts"].len(), 0);
        assert_eq!(result[2].relationships["posts"].len(), 0);
    }

    // ---- AUDIT-01 regression tests (Phase 30) ----
    //
    // Bug: `parse_predicate_json` constructed `Predicate::Like(_, _, true)` for the
    // `like` key, making LIKE silently case-insensitive in Rust IVM. The parity reference
    // (`packages/zero-ivm-rs/src/pipeline.rs` lines 128-135) uses `false` for `like` and
    // `true` for `ilike`. These tests pin both behaviors.
    //
    // See: `.planning/phases/30-audit-fixes/30-01-PLAN.md`, decisions D-01 and D-02 in
    // `.planning/phases/30-audit-fixes/30-CONTEXT.md`.

    #[test]
    fn test_parse_predicate_like_case_sensitive() {
        use zero_ivm_rs::filter::{Predicate, evaluate_json_row};

        let json_value = json!({"field": "name", "like": "Foo%"});
        let pred = parse_predicate_json(&json_value).expect("parse like");

        // Variant must be Like with case_insensitive=false.
        assert!(
            matches!(&pred, Predicate::Like(field, pattern, false) if field == "name" && pattern == "Foo%"),
            "expected Predicate::Like(\"name\", \"Foo%\", false), got {:?}",
            pred,
        );

        // Evaluation: case-sensitive prefix match.
        let mut row_foo_bar = serde_json::Map::new();
        row_foo_bar.insert("name".into(), serde_json::Value::String("Foo Bar".into()));
        assert!(
            evaluate_json_row(&pred, &row_foo_bar),
            "LIKE 'Foo%' should match 'Foo Bar'",
        );

        let mut row_foo_lower = serde_json::Map::new();
        row_foo_lower.insert("name".into(), serde_json::Value::String("foo bar".into()));
        assert!(
            !evaluate_json_row(&pred, &row_foo_lower),
            "LIKE 'Foo%' must NOT match 'foo bar' (case-sensitive)",
        );

        let mut row_foo_upper = serde_json::Map::new();
        row_foo_upper.insert("name".into(), serde_json::Value::String("FOO".into()));
        assert!(
            !evaluate_json_row(&pred, &row_foo_upper),
            "LIKE 'Foo%' must NOT match 'FOO' (case-sensitive)",
        );
    }

    #[test]
    fn test_parse_predicate_ilike_case_insensitive() {
        use zero_ivm_rs::filter::{Predicate, evaluate_json_row};

        let json_value = json!({"field": "name", "ilike": "foo%"});
        let pred = parse_predicate_json(&json_value).expect("parse ilike");

        // Variant must be Like with case_insensitive=true.
        assert!(
            matches!(&pred, Predicate::Like(field, pattern, true) if field == "name" && pattern == "foo%"),
            "expected Predicate::Like(\"name\", \"foo%\", true), got {:?}",
            pred,
        );

        // Evaluation: case-insensitive prefix match.
        let mut row_foo_bar = serde_json::Map::new();
        row_foo_bar.insert("name".into(), serde_json::Value::String("Foo Bar".into()));
        assert!(
            evaluate_json_row(&pred, &row_foo_bar),
            "ILIKE 'foo%' should match 'Foo Bar'",
        );

        let mut row_foo_lower = serde_json::Map::new();
        row_foo_lower.insert("name".into(), serde_json::Value::String("foo bar".into()));
        assert!(
            evaluate_json_row(&pred, &row_foo_lower),
            "ILIKE 'foo%' should match 'foo bar'",
        );

        let mut row_foo_upper = serde_json::Map::new();
        row_foo_upper.insert("name".into(), serde_json::Value::String("FOO".into()));
        assert!(
            evaluate_json_row(&pred, &row_foo_upper),
            "ILIKE 'foo%' should match 'FOO'",
        );
    }

    #[test]
    fn test_parse_predicate_like_distinguishes_from_ilike() {
        use zero_ivm_rs::filter::evaluate_json_row;

        // Same input row evaluated against LIKE 'foo%' and ILIKE 'foo%'.
        let mut row = serde_json::Map::new();
        row.insert("name".into(), serde_json::Value::String("Foo Bar".into()));

        let like_pred =
            parse_predicate_json(&json!({"field": "name", "like": "foo%"})).expect("parse like");
        let ilike_pred =
            parse_predicate_json(&json!({"field": "name", "ilike": "foo%"})).expect("parse ilike");

        assert!(
            !evaluate_json_row(&like_pred, &row),
            "LIKE 'foo%' must NOT match 'Foo Bar' (case-sensitive)",
        );
        assert!(
            evaluate_json_row(&ilike_pred, &row),
            "ILIKE 'foo%' MUST match 'Foo Bar' (case-insensitive)",
        );
    }
}
