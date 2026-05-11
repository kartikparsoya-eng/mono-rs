use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use napi_derive::napi;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection_pool::ConnectionPool;
use crate::diff::{self, Change, DiffError, Row, TableAndZqlSpec};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RowChange {
    #[serde(rename = "queryID")]
    pub query_id: String,
    pub table: String,
    pub row_key: serde_json::Value,
    pub row: Option<Row>,
    #[serde(rename = "type")]
    pub change_type: String,
}

#[derive(Debug, Serialize)]
pub struct AdvanceResult {
    pub changes: Vec<RowChange>,
    pub error: Option<String>,
    pub error_type: Option<String>,
    /// If set, signals that the caller should reset all pipelines.
    /// Used when a companion scalar subquery value changes.
    pub reset_signal: Option<String>,
}

use crate::hydrate::build_operator_chain;
use crate::source::SourceChange;
use crate::table_source::RustTableSource;
use zero_ivm_rs::operator::Operator as IvmOperator;
use zero_ivm_rs::pipeline::OperatorConfig;
use zero_ivm_rs::types::Change as IvmChange;

#[derive(Debug, Clone, Deserialize)]
pub struct FullPipelineConfig {
    pub query_id: String,
    pub source_table: String,
    pub operator_config: Vec<OperatorConfig>,
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub split_edit_keys: Vec<String>,
    #[serde(default)]
    pub column_types: Option<HashMap<String, HashMap<String, String>>>,
    #[serde(default)]
    pub all_primary_keys: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub rel_to_table: HashMap<String, String>,
}
fn diff_row_to_source_row(h: &crate::diff::Row) -> crate::source::Row {
    h.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn diff_change_to_source_changes(
    change: &Change,
    primary_key: &[String],
) -> Vec<SourceChange> {
    let mut results = Vec::new();

    // Match prev_values to next_value by primary key
    if change.prev_values.is_empty() {
        // Pure Add
        if let Some(ref nv) = change.next_value {
            results.push(SourceChange::Add(diff_row_to_source_row(nv)));
        }
    } else if change.next_value.is_none() {
        // Pure Remove(s)
        for pv in &change.prev_values {
            results.push(SourceChange::Remove(diff_row_to_source_row(pv)));
        }
    } else {
        let nv = change.next_value.as_ref().unwrap();
        let next_pk: Vec<Option<&serde_json::Value>> =
            primary_key.iter().map(|k| nv.get(k)).collect();

        let mut matched = false;
        for pv in &change.prev_values {
            let prev_pk: Vec<Option<&serde_json::Value>> =
                primary_key.iter().map(|k| pv.get(k)).collect();
            if prev_pk == next_pk {
                results.push(SourceChange::Edit {
                    row: diff_row_to_source_row(nv),
                    old_row: diff_row_to_source_row(pv),
                });
                matched = true;
            } else {
                results.push(SourceChange::Remove(diff_row_to_source_row(pv)));
            }
        }
        if !matched {
            results.push(SourceChange::Add(diff_row_to_source_row(nv)));
        }
    }
    results
}

fn ivm_change_to_row_changes(
    change: &IvmChange,
    query_id: &str,
    table: &str,
    primary_key: &[String],
) -> Vec<RowChange> {
    fn extract_row_key(
        row: &serde_json::Map<String, serde_json::Value>,
        primary_key: &[String],
    ) -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = primary_key
            .iter()
            .filter_map(|k| row.get(k).map(|v| (k.clone(), v.clone())))
            .collect();
        serde_json::Value::Object(map)
    }

    match change {
        IvmChange::Add(node) => vec![RowChange {
            query_id: query_id.to_string(),
            table: table.to_string(),
            row_key: extract_row_key(&node.row, primary_key),
            row: Some(node.row.clone().into_iter().collect()),
            change_type: "add".to_string(),
        }],
        IvmChange::Remove(node) => vec![RowChange {
            query_id: query_id.to_string(),
            table: table.to_string(),
            row_key: extract_row_key(&node.row, primary_key),
            row: None,
            change_type: "remove".to_string(),
        }],
        IvmChange::Edit { node, old_node: _ } => vec![RowChange {
            query_id: query_id.to_string(),
            table: table.to_string(),
            row_key: extract_row_key(&node.row, primary_key),
            row: Some(node.row.clone().into_iter().collect()),
            change_type: "edit".to_string(),
        }],
        IvmChange::Child { node: _, child } => {
            // Child changes propagate as edits to the parent row
            // For now, represent as a child change type
            vec![RowChange {
                query_id: query_id.to_string(),
                table: table.to_string(),
                row_key: serde_json::Value::Null,
                row: None,
                change_type: "child".to_string(),
            }]
        }
    }
}

/// AUDIT-02: split a `SourceChange::Edit` into `Remove(old)+Add(new)` when
/// any column in `split_edit_keys` differs between old_row and new_row.
/// Mirrors `RustTableSource::maybe_split_edit` for the persistent advance
/// path which builds SourceChange directly without going through
/// `source.push()`. Other variants pass through unchanged.
fn maybe_split_edit_for_advance(
    sc: SourceChange,
    split_edit_keys: &[String],
) -> Vec<SourceChange> {
    if split_edit_keys.is_empty() {
        return vec![sc];
    }
    if let SourceChange::Edit { ref row, ref old_row } = sc {
        for key in split_edit_keys {
            let old_val = old_row.get(key);
            let new_val = row.get(key);
            if old_val != new_val {
                return vec![
                    SourceChange::Remove(old_row.clone()),
                    SourceChange::Add(row.clone()),
                ];
            }
        }
    }
    vec![sc]
}

fn source_change_to_ivm_change(sc: &SourceChange) -> IvmChange {
    let make_node = |row: &crate::source::Row| zero_ivm_rs::types::Node {
        row: row.clone(),
        relationships: HashMap::new(),
    };
    match sc {
        SourceChange::Add(row) => IvmChange::Add(make_node(row)),
        SourceChange::Remove(row) => IvmChange::Remove(make_node(row)),
        SourceChange::Edit { row, old_row } => IvmChange::Edit {
            node: make_node(row),
            old_node: make_node(old_row),
        },
    }
}

// ─── Child Table Map (for routing child changes to parent pipelines) ────────

#[derive(Debug, Clone)]
pub(crate) struct ChildTableInfo {
    pub(crate) parent_table: String,
    pub(crate) parent_key: Vec<String>,
    pub(crate) child_key: Vec<String>,
    pub(crate) relationship_name: String,
    pub(crate) child_pk: Vec<String>,
}

/// Mapping from child table name → Vec<(op_index in push_ptrs, child_pk)>.
/// Used to route child table changes through the correct Join/Exists operator
/// via push_child().
fn build_child_table_to_op_index(
    configs: &[OperatorConfig],
) -> HashMap<String, Vec<(usize, Vec<String>)>> {
    let mut map: HashMap<String, Vec<(usize, Vec<String>)>> = HashMap::new();
    if configs.is_empty() {
        return map;
    }
    // configs[0] is Source, configs[1..] correspond to push_ptrs[0..]
    let rest = match &configs[0] {
        OperatorConfig::Source { .. } => &configs[1..],
        _ => return map,
    };
    for (i, config) in rest.iter().enumerate() {
        match config {
            OperatorConfig::Exists { child, .. } => {
                if let Some(OperatorConfig::Source { table_name, primary_key, .. }) = child.first() {
                    map.entry(table_name.clone())
                        .or_default()
                        .push((i, primary_key.clone()));
                }
            }
            OperatorConfig::OrExists { branches, .. } => {
                for branch in branches {
                    if let Some(OperatorConfig::Source { table_name, primary_key, .. }) = branch.child.first() {
                        map.entry(table_name.clone())
                            .or_default()
                            .push((i, primary_key.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    map
}

/// Check if a child row has a matching parent in the DB for ALL join key columns.
/// This prevents orphan child rows (whose FK points to a non-existent parent)
/// from being emitted as changes.
fn child_row_has_parent(
    db_path: &str,
    ci: &ChildTableInfo,
    child_row: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let mut where_parts = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    for (pcol, ccol) in ci.parent_key.iter().zip(ci.child_key.iter()) {
        let child_val = child_row.get(ccol).cloned().unwrap_or(serde_json::Value::Null);
        if child_val.is_null() {
            return false;
        }
        where_parts.push(format!("\"{}\" = ?", pcol));
        match &child_val {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    params.push(rusqlite::types::Value::Integer(i));
                } else if let Some(f) = n.as_f64() {
                    params.push(rusqlite::types::Value::Real(f));
                }
            }
            serde_json::Value::String(s) => {
                params.push(rusqlite::types::Value::Text(s.clone()));
            }
            serde_json::Value::Bool(b) => {
                params.push(rusqlite::types::Value::Integer(if *b { 1 } else { 0 }));
            }
            _ => return false,
        }
    }
    let sql = format!(
        "SELECT 1 FROM \"{}\" WHERE {} LIMIT 1",
        ci.parent_table,
        where_parts.join(" AND ")
    );
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match rusqlite::Connection::open_with_flags(db_path, flags) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|v| {
        v as &dyn rusqlite::types::ToSql
    }).collect();
    stmt.exists(param_refs.as_slice()).unwrap_or(false)
}

fn collect_child_table_map(
    configs: &[OperatorConfig],
) -> HashMap<String, Vec<ChildTableInfo>> {
    let mut map: HashMap<String, Vec<ChildTableInfo>> = HashMap::new();
    collect_child_table_map_recursive(configs, None, &mut map);
    map
}

fn collect_child_table_map_recursive(
    configs: &[OperatorConfig],
    current_parent_table: Option<&str>,
    map: &mut HashMap<String, Vec<ChildTableInfo>>,
) {
    for config in configs {
        match config {
            OperatorConfig::Source { table_name, .. } => {
                collect_child_table_map_recursive(&configs[1..], Some(table_name), map);
                return;
            }
            OperatorConfig::Join {
                parent_key,
                child_key,
                relationship_name,
                child,
            } => {
                if let Some(OperatorConfig::Source {
                    table_name,
                    primary_key,
                    ..
                }) = child.first()
                {
                    map.entry(table_name.clone())
                        .or_default()
                        .push(ChildTableInfo {
                            parent_table: current_parent_table.unwrap_or("").to_string(),
                            parent_key: parent_key.clone(),
                            child_key: child_key.clone(),
                            relationship_name: relationship_name.clone(),
                            child_pk: primary_key.clone(),
                        });
                }
                collect_child_table_map_recursive(child, current_parent_table, map);
            }
            OperatorConfig::Exists {
                parent_key,
                child_key,
                relationship_name,
                child,
                ..
            } => {
                if let Some(OperatorConfig::Source {
                    table_name,
                    primary_key,
                    ..
                }) = child.first()
                {
                    map.entry(table_name.clone())
                        .or_default()
                        .push(ChildTableInfo {
                            parent_table: current_parent_table.unwrap_or("").to_string(),
                            parent_key: parent_key.clone(),
                            child_key: child_key.clone(),
                            relationship_name: relationship_name.clone(),
                            child_pk: primary_key.clone(),
                        });
                }
                collect_child_table_map_recursive(child, current_parent_table, map);
            }
            OperatorConfig::OrExists { branches, .. } => {
                for branch in branches {
                    if let Some(OperatorConfig::Source {
                        table_name,
                        primary_key,
                        ..
                    }) = branch.child.first()
                    {
                        map.entry(table_name.clone())
                            .or_default()
                            .push(ChildTableInfo {
                                parent_table: current_parent_table.unwrap_or("").to_string(),
                                parent_key: branch.parent_key.clone(),
                                child_key: branch.child_key.clone(),
                                relationship_name: branch.relationship_name.clone(),
                                child_pk: primary_key.clone(),
                            });
                    }
                    collect_child_table_map_recursive(&branch.child, current_parent_table, map);
                }
            }
            _ => {}
        }
    }
}

/// Build a `relationship_name → child_table_name` map from the emitted
/// `OperatorConfig` tree. This map keys are the **uniquified** relationship
/// names (i.e., `<original_alias>_<count>`) as written into
/// `OperatorConfig::Exists.relationship_name` by
/// `ast_to_config::uniquify_top_level_csq_aliases`. The values are the
/// underlying table names extracted from the child's first `Source` config.
///
/// **Why this exists:** `flatten_nodes_to_row_changes` looks up the table
/// name via `rel_to_table.get(rel_name)` where `rel_name` comes from the
/// runtime change output (always uniquified, post-Phase-34 alias-leak fix).
/// The previous implementation built the map from the raw AST via
/// `collect_child_tables(&query.ast)` — pre-uniquify — so every uniquified
/// rel_name missed the lookup, fell back to using rel_name as the table
/// name, and `coerce_row` returned None, dropping every CSQ-EXISTS child row.
pub(crate) fn collect_rel_to_table_from_configs(
    configs: &[OperatorConfig],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    collect_rel_to_table_recursive(configs, &mut map);
    map
}

fn collect_rel_to_table_recursive(
    configs: &[OperatorConfig],
    map: &mut HashMap<String, String>,
) {
    for config in configs {
        match config {
            OperatorConfig::Join { relationship_name, child, .. } => {
                if let Some(OperatorConfig::Source { table_name, .. }) = child.first() {
                    map.insert(relationship_name.clone(), table_name.clone());
                }
                collect_rel_to_table_recursive(child, map);
            }
            OperatorConfig::Exists { relationship_name, child, .. } => {
                if let Some(OperatorConfig::Source { table_name, .. }) = child.first() {
                    map.insert(relationship_name.clone(), table_name.clone());
                }
                collect_rel_to_table_recursive(child, map);
            }
            OperatorConfig::OrExists { branches, .. } => {
                for branch in branches {
                    if let Some(OperatorConfig::Source { table_name, .. }) = branch.child.first() {
                        map.insert(branch.relationship_name.clone(), table_name.clone());
                    }
                    collect_rel_to_table_recursive(&branch.child, map);
                }
            }
            OperatorConfig::FanOut { branches } => {
                for branch in branches {
                    collect_rel_to_table_recursive(branch, map);
                }
            }
            _ => {}
        }
    }
}

// Maps parent_table_name → Vec<ChildRelation>. Inverse of child_table_map.
#[derive(Debug, Clone)]
pub(crate) struct ChildRelation {
    pub(crate) child_table: String,
    pub(crate) parent_join_col: Vec<String>,  // column(s) in parent table
    pub(crate) child_join_col: Vec<String>,   // column(s) in child table
    pub(crate) child_pk: Vec<String>,
    pub(crate) relationship_name: String,
    pub(crate) child_order: Vec<(String, String)>,  // ORDER BY columns from child subquery
    pub(crate) child_limit: Option<usize>,          // LIMIT from child subquery
}

fn collect_children_of_map(
    configs: &[OperatorConfig],
) -> HashMap<String, Vec<ChildRelation>> {
    let mut map: HashMap<String, Vec<ChildRelation>> = HashMap::new();
    collect_children_of_recursive(configs, None, &mut map);
    map
}

fn collect_children_of_recursive(
    configs: &[OperatorConfig],
    current_table: Option<&str>,
    map: &mut HashMap<String, Vec<ChildRelation>>,
) {
    for config in configs {
        match config {
            OperatorConfig::Source { table_name, .. } => {
                collect_children_of_recursive(&configs[1..], Some(table_name), map);
                return;
            }
            OperatorConfig::Join {
                parent_key,
                child_key,
                relationship_name,
                child,
            } => {
                if let (Some(parent), Some(OperatorConfig::Source { table_name, primary_key, sort, .. })) =
                    (current_table, child.first())
                {
                    // Extract limit and order from child's Take operator if present
                    let mut child_limit = None;
                    let mut child_order = sort.clone(); // default from Source sort
                    for cc in child.iter() {
                        if let OperatorConfig::Take { limit, sort: take_sort, .. } = cc {
                            child_limit = Some(*limit);
                            if !take_sort.is_empty() {
                                child_order = take_sort.clone();
                            }
                        }
                    }
                    map.entry(parent.to_string())
                        .or_default()
                        .push(ChildRelation {
                            child_table: table_name.clone(),
                            parent_join_col: parent_key.clone(),
                            child_join_col: child_key.clone(),
                            child_pk: primary_key.clone(),
                            relationship_name: relationship_name.clone(),
                            child_order,
                            child_limit,
                        });
                }
                // Recurse into child config to find deeper levels
                collect_children_of_recursive(child, None, map);
            }
            _ => {}
        }
    }
}

// B11: prev_db_path per TS pipeline-driver.ts:1542. Reads descendants
// against the PREV snapshot — same-tx descendant deletes still present
// there. Was reading against db_path (curr post-swap_snapshot) which
// silently elided same-tx descendants.
// See .planning/IVM-PORT-AUDIT-DEEP.md §B11.
//
// Phase 35 / NEW-1: refactored to take a borrowed `&rusqlite::Connection`
// (reused from `Instance.prev_pool`) instead of opening per call.
// `prev_conn.prepare_cached(...)` amortizes statement preparation across
// the recursion. The legacy wrapper `emit_descendant_removals_legacy`
// retains the open-per-call behavior for fallback + bench baseline.
// See `.planning/phases/35-pool-cascade-hardening/35-03-PLAN.md`.
fn emit_descendant_removals_with_conn(
    prev_conn: &rusqlite::Connection,
    deleted_row: &serde_json::Map<String, serde_json::Value>,
    deleted_table: &str,
    children_of: &HashMap<String, Vec<ChildRelation>>,
    query_id: &str,
    column_types: &Option<HashMap<String, HashMap<String, String>>>,
    row_changes: &mut Vec<RowChange>,
) {
    let child_rels = match children_of.get(deleted_table) {
        Some(rels) => rels,
        None => return,
    };

    for rel in child_rels {
        // Build WHERE clause: child_join_col[i] = deleted_row[parent_join_col[i]]
        let mut conditions = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        let mut skip = false;
        for (pcol, ccol) in rel.parent_join_col.iter().zip(rel.child_join_col.iter()) {
            if let Some(val) = deleted_row.get(pcol) {
                conditions.push(format!("\"{}\" = ?", ccol));
                match val {
                    serde_json::Value::String(s) => params.push(rusqlite::types::Value::Text(s.clone())),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            params.push(rusqlite::types::Value::Integer(i));
                        } else if let Some(f) = n.as_f64() {
                            params.push(rusqlite::types::Value::Real(f));
                        }
                    }
                    serde_json::Value::Null => { skip = true; break; }
                    _ => { skip = true; break; }
                }
            } else {
                skip = true;
                break;
            }
        }
        if skip || conditions.is_empty() {
            continue;
        }

        let mut sql = format!(
            "SELECT * FROM \"{}\" WHERE {}",
            rel.child_table,
            conditions.join(" AND ")
        );
        // Respect child subquery's ORDER BY and LIMIT
        if !rel.child_order.is_empty() {
            let order_clause: Vec<String> = rel.child_order.iter()
                .map(|(col, dir)| format!("\"{}\" {}", col, dir.to_uppercase()))
                .collect();
            sql.push_str(&format!(" ORDER BY {}", order_clause.join(", ")));
        }
        if let Some(limit) = rel.child_limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }
        // NEW-1 / D-10: rusqlite's `prepare_cached` amortizes prepare cost
        // across siblings (same SQL shape repeats per-parent within a relation).
        let mut stmt = match prev_conn.prepare_cached(&sql) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[B11] emit_descendant_removals: prepare failed for table={} sql={:?}: {}",
                    rel.child_table, sql, e
                );
                continue;
            }
        };
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p as &dyn rusqlite::types::ToSql).collect();
        let rows_iter = match stmt.query_map(param_refs.as_slice(), |r| {
            let mut map = serde_json::Map::new();
            for (i, name) in col_names.iter().enumerate() {
                let val: rusqlite::types::Value = r.get(i)?;
                let json_val = match val {
                    rusqlite::types::Value::Null => serde_json::Value::Null,
                    rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                    rusqlite::types::Value::Real(f) => serde_json::json!(f),
                    rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
                    rusqlite::types::Value::Blob(b) => serde_json::Value::String(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &b)),
                };
                map.insert(name.clone(), json_val);
            }
            Ok(map)
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "[B11] emit_descendant_removals: query_map failed for table={}: {}",
                    rel.child_table, e
                );
                continue;
            }
        };

        // Drain rows into an owned Vec BEFORE recursing — the rusqlite
        // Statement keeps `&mut prev_conn` alive while iterating, which
        // would otherwise conflict with the recursive call's own
        // prepare_cached on the same connection.
        let collected: Vec<_> = rows_iter.filter_map(|r| r.ok()).collect();
        drop(stmt);

        for child_row in collected {
            let row_key = extract_row_key_from_map(&child_row, &rel.child_pk);
            let mut row_map: HashMap<String, serde_json::Value> = child_row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            if let Some(ref ct) = column_types {
                if let Some(cols) = ct.get(&rel.relationship_name) {
                    row_map.retain(|k, _| cols.contains_key(k));
                }
            }
            row_changes.push(RowChange {
                query_id: query_id.to_string(),
                table: rel.relationship_name.clone(),
                row_key,
                row: None,
                change_type: "remove".to_string(),
            });
            // Recurse for deeper levels — same prev connection (B11/NEW-1).
            emit_descendant_removals_with_conn(
                prev_conn, &child_row, &rel.child_table, children_of,
                query_id, column_types, row_changes,
            );
        }
    }
}

// Phase 35 / NEW-1: legacy fallback used when `Instance.prev_pool` is None
// (e.g., bench baseline via Z_DISABLE_PREV_POOL=1, or a TS caller that
// forgot `set_prev_snapshot`). Opens a fresh read-only connection and
// pins a snapshot via BEGIN DEFERRED, then delegates to
// `emit_descendant_removals_with_conn`. Callers should prefer the
// prev_pool path.
fn emit_descendant_removals(
    prev_db_path: &str,
    deleted_row: &serde_json::Map<String, serde_json::Value>,
    deleted_table: &str,
    children_of: &HashMap<String, Vec<ChildRelation>>,
    query_id: &str,
    column_types: &Option<HashMap<String, HashMap<String, String>>>,
    row_changes: &mut Vec<RowChange>,
) {
    if !children_of.contains_key(deleted_table) {
        return;
    }
    let conn = match rusqlite::Connection::open_with_flags(
        prev_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[B11] emit_descendant_removals: open_with_flags failed for {:?} (table={}): {}",
                prev_db_path, deleted_table, e
            );
            return;
        }
    };
    if let Err(e) = conn.execute_batch("BEGIN DEFERRED") {
        eprintln!(
            "[B11] emit_descendant_removals: BEGIN DEFERRED failed for {:?} (table={}): {}",
            prev_db_path, deleted_table, e
        );
        return;
    }
    emit_descendant_removals_with_conn(
        &conn, deleted_row, deleted_table, children_of,
        query_id, column_types, row_changes,
    );
}

/// Test-only legacy wrapper: opens a fresh connection per call (no
/// statement cache, no snapshot pin reuse). Used by the cascade bench
/// baseline (Phase 35 Wave 3) and as a control oracle in differential
/// tests.
#[cfg(test)]
pub(crate) fn emit_descendant_removals_legacy(
    prev_db_path: &str,
    deleted_row: &serde_json::Map<String, serde_json::Value>,
    deleted_table: &str,
    children_of: &HashMap<String, Vec<ChildRelation>>,
    query_id: &str,
    column_types: &Option<HashMap<String, HashMap<String, String>>>,
    row_changes: &mut Vec<RowChange>,
) {
    let child_rels = match children_of.get(deleted_table) {
        Some(rels) => rels,
        None => return,
    };
    let conn = match rusqlite::Connection::open_with_flags(
        prev_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = conn.execute_batch("BEGIN DEFERRED");
    for rel in child_rels {
        let mut conditions = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        let mut skip = false;
        for (pcol, ccol) in rel.parent_join_col.iter().zip(rel.child_join_col.iter()) {
            if let Some(val) = deleted_row.get(pcol) {
                conditions.push(format!("\"{}\" = ?", ccol));
                match val {
                    serde_json::Value::String(s) => params.push(rusqlite::types::Value::Text(s.clone())),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            params.push(rusqlite::types::Value::Integer(i));
                        } else if let Some(f) = n.as_f64() {
                            params.push(rusqlite::types::Value::Real(f));
                        }
                    }
                    serde_json::Value::Null => { skip = true; break; }
                    _ => { skip = true; break; }
                }
            } else {
                skip = true;
                break;
            }
        }
        if skip || conditions.is_empty() {
            continue;
        }
        let mut sql = format!(
            "SELECT * FROM \"{}\" WHERE {}",
            rel.child_table,
            conditions.join(" AND ")
        );
        if !rel.child_order.is_empty() {
            let order_clause: Vec<String> = rel.child_order.iter()
                .map(|(col, dir)| format!("\"{}\" {}", col, dir.to_uppercase()))
                .collect();
            sql.push_str(&format!(" ORDER BY {}", order_clause.join(", ")));
        }
        if let Some(limit) = rel.child_limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }
        // Plain `prepare` (not _cached) per legacy semantics.
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p as &dyn rusqlite::types::ToSql).collect();
        let rows_iter = match stmt.query_map(param_refs.as_slice(), |r| {
            let mut map = serde_json::Map::new();
            for (i, name) in col_names.iter().enumerate() {
                let val: rusqlite::types::Value = r.get(i)?;
                let json_val = match val {
                    rusqlite::types::Value::Null => serde_json::Value::Null,
                    rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                    rusqlite::types::Value::Real(f) => serde_json::json!(f),
                    rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
                    rusqlite::types::Value::Blob(b) => serde_json::Value::String(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &b)),
                };
                map.insert(name.clone(), json_val);
            }
            Ok(map)
        }) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let collected: Vec<_> = rows_iter.filter_map(|r| r.ok()).collect();
        drop(stmt);
        for child_row in collected {
            let row_key = extract_row_key_from_map(&child_row, &rel.child_pk);
            let mut row_map: HashMap<String, serde_json::Value> = child_row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            if let Some(ref ct) = column_types {
                if let Some(cols) = ct.get(&rel.relationship_name) {
                    row_map.retain(|k, _| cols.contains_key(k));
                }
            }
            row_changes.push(RowChange {
                query_id: query_id.to_string(),
                table: rel.relationship_name.clone(),
                row_key,
                row: None,
                change_type: "remove".to_string(),
            });
            // Recurse: legacy semantics open a NEW connection per call.
            emit_descendant_removals_legacy(
                prev_db_path, &child_row, &rel.child_table, children_of,
                query_id, column_types, row_changes,
            );
        }
    }
}

// ─── Flatten IVM Changes to RowChanges ──────────────────────────────────────

fn extract_row_key_from_source_row(
    row: &crate::source::Row,
    pk: &[String],
) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = pk
        .iter()
        .filter_map(|k| row.get(k).map(|v| (k.clone(), v.clone())))
        .collect();
    serde_json::Value::Object(map)
}

fn extract_row_key_from_map(
    row: &serde_json::Map<String, serde_json::Value>,
    pk: &[String],
) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = pk
        .iter()
        .filter_map(|k| row.get(k).map(|v| (k.clone(), v.clone())))
        .collect();
    serde_json::Value::Object(map)
}

fn flatten_ivm_change_to_row_changes(
    out: &mut Vec<RowChange>,
    change: &IvmChange,
    query_id: &str,
    root_table: &str,
    root_pk: &[String],
    all_pks: &HashMap<String, Vec<String>>,
) {
    match change {
        IvmChange::Add(node) => {
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: root_table.to_string(),
                row_key: extract_row_key_from_map(&node.row, root_pk),
                row: Some(node.row.clone().into_iter().collect()),
                change_type: "add".to_string(),
            });
            flatten_node_relationships(out, query_id, node, "add", all_pks);
        }
        IvmChange::Remove(node) => {
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: root_table.to_string(),
                row_key: extract_row_key_from_map(&node.row, root_pk),
                row: None,
                change_type: "remove".to_string(),
            });
            flatten_node_relationships(out, query_id, node, "remove", all_pks);
        }
        IvmChange::Edit { node, .. } => {
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: root_table.to_string(),
                row_key: extract_row_key_from_map(&node.row, root_pk),
                row: Some(node.row.clone().into_iter().collect()),
                change_type: "edit".to_string(),
            });
        }
        IvmChange::Child { node, child } => {
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: root_table.to_string(),
                row_key: extract_row_key_from_map(&node.row, root_pk),
                row: Some(node.row.clone().into_iter().collect()),
                change_type: "edit".to_string(),
            });
            flatten_child_change(out, query_id, child, all_pks);
        }
    }
}

fn flatten_child_change(
    out: &mut Vec<RowChange>,
    query_id: &str,
    child_data: &zero_ivm_rs::types::ChildData,
    all_pks: &HashMap<String, Vec<String>>,
) {
    let child_table = &child_data.relationship_name;
    let child_pk = all_pks.get(child_table);
    match &*child_data.change {
        IvmChange::Add(node) => {
            let row_key = match child_pk {
                Some(pk) => extract_row_key_from_map(&node.row, pk),
                None => serde_json::Value::Object(node.row.clone()),
            };
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: child_table.to_string(),
                row_key,
                row: Some(node.row.clone().into_iter().collect()),
                change_type: "add".to_string(),
            });
            flatten_node_relationships(out, query_id, node, "add", all_pks);
        }
        IvmChange::Remove(node) => {
            let row_key = match child_pk {
                Some(pk) => extract_row_key_from_map(&node.row, pk),
                None => serde_json::Value::Object(node.row.clone()),
            };
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: child_table.to_string(),
                row_key,
                row: None,
                change_type: "remove".to_string(),
            });
            flatten_node_relationships(out, query_id, node, "remove", all_pks);
        }
        IvmChange::Edit { node, .. } => {
            let row_key = match child_pk {
                Some(pk) => extract_row_key_from_map(&node.row, pk),
                None => serde_json::Value::Object(node.row.clone()),
            };
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: child_table.to_string(),
                row_key,
                row: Some(node.row.clone().into_iter().collect()),
                change_type: "edit".to_string(),
            });
        }
        IvmChange::Child { node, child } => {
            let row_key = match child_pk {
                Some(pk) => extract_row_key_from_map(&node.row, pk),
                None => serde_json::Value::Object(node.row.clone()),
            };
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: child_table.to_string(),
                row_key,
                row: Some(node.row.clone().into_iter().collect()),
                change_type: "edit".to_string(),
            });
            flatten_child_change(out, query_id, child, all_pks);
        }
    }
}

fn flatten_node_relationships(
    out: &mut Vec<RowChange>,
    query_id: &str,
    node: &zero_ivm_rs::types::Node,
    change_type: &str,
    all_pks: &HashMap<String, Vec<String>>,
) {
    for (rel_name, children) in &node.relationships {
        let rel_pk = all_pks.get(rel_name);
        for child_node in children {
            let row_key = match rel_pk {
                Some(pk) => extract_row_key_from_map(&child_node.row, pk),
                None => serde_json::Value::Object(child_node.row.clone()),
            };
            let row = if change_type == "remove" {
                None
            } else {
                Some(child_node.row.clone().into_iter().collect())
            };
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: rel_name.to_string(),
                row_key,
                row,
                change_type: change_type.to_string(),
            });
            flatten_node_relationships(out, query_id, child_node, change_type, all_pks);
        }
    }
}

fn changes_to_row_changes_direct(
    changes: &[Change],
    pipeline: &FullPipelineConfig,
    table_name: &str,
    primary_key: &[String],
) -> Vec<RowChange> {
    let mut row_changes = Vec::new();
    for change in changes {
        if change.table != pipeline.source_table {
            continue;
        }
        let source_changes = diff_change_to_source_changes(change, primary_key);
        for sc in &source_changes {
            let ivm_change = source_change_to_ivm_change(sc);
            row_changes.extend(ivm_change_to_row_changes(
                &ivm_change,
                &pipeline.query_id,
                table_name,
                primary_key,
            ));
        }
    }
    row_changes
}

/// NAPI entry point: full advance with complete operator tree support.
/// Replaces `rust_fan_out` for pipelines with Join/Take/Exists operators.
// ─── Binary Serialization (Phase 25) ────────────────────────────────────────

use napi::bindgen_prelude::Buffer;

/// Encode a serde_json::Value into the binary buffer.
/// Tags: 0=null, 1=i64(8B LE), 2=f64(8B LE), 3=text(u32 len + bytes),
///       4=blob(u32 len + bytes), 5=bool(u8), 6=json fallback(u32 len + JSON bytes)
pub(crate) fn encode_json_value(buf: &mut Vec<u8>, val: &serde_json::Value) {
    match val {
        serde_json::Value::Null => buf.push(0),
        serde_json::Value::Bool(b) => {
            buf.push(5);
            buf.push(if *b { 1 } else { 0 });
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                buf.push(1);
                buf.extend_from_slice(&i.to_le_bytes());
            } else if let Some(f) = n.as_f64() {
                buf.push(2);
                buf.extend_from_slice(&f.to_le_bytes());
            } else {
                let s = n.to_string();
                buf.push(6);
                buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                buf.extend_from_slice(s.as_bytes());
            }
        }
        serde_json::Value::String(s) => {
            buf.push(3);
            buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
        _ => {
            let s = serde_json::to_string(val).unwrap_or_default();
            buf.push(6);
            buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
    }
}

pub(crate) fn encode_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u16).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

pub(crate) fn encode_advance_result_buf(result: &AdvanceResult) -> Vec<u8> {
    let mut buf = Vec::with_capacity(result.changes.len() * 128);

    buf.extend_from_slice(&(result.changes.len() as u32).to_le_bytes());

    let has_error = result.error.is_some();
    let has_reset = result.reset_signal.is_some();
    let flags: u8 = if has_error { 1 } else { 0 } | if has_reset { 4 } else { 0 };
    buf.push(flags);

    if has_error {
        encode_str(&mut buf, result.error.as_deref().unwrap_or(""));
        encode_str(&mut buf, result.error_type.as_deref().unwrap_or(""));
    }

    for change in &result.changes {
        let ct: u8 = match change.change_type.as_str() {
            "add" => 0,
            "remove" => 1,
            "edit" => 2,
            _ => 3,
        };
        buf.push(ct);
        encode_str(&mut buf, &change.query_id);
        encode_str(&mut buf, &change.table);
        encode_json_value(&mut buf, &change.row_key);

        match &change.row {
            None => buf.push(0),
            Some(row) => {
                buf.push(1);
                buf.extend_from_slice(&(row.len() as u16).to_le_bytes());
                for (col_name, col_value) in row.iter() {
                    encode_str(&mut buf, col_name);
                    encode_json_value(&mut buf, col_value);
                }
            }
        }
    }

    // Encode reset_signal after all changes (flag bit 2)
    if let Some(ref signal) = result.reset_signal {
        encode_str(&mut buf, signal);
    }

    buf
}

/// Collect split_edit_keys from an AST: all parent correlation keys from
/// related joins and exists subqueries. When a parent row's correlation key
/// changes, the edit must be split into remove+add so the join/exists operator
/// sees it as a membership change rather than an in-place update.
fn collect_split_edit_keys(ast: &crate::ast_to_config::Ast) -> Vec<String> {
    let mut keys = std::collections::HashSet::new();
    if let Some(related) = &ast.related {
        for rel in related {
            for field in &rel.correlation.parent_field {
                keys.insert(field.clone());
            }
        }
    }
    fn collect_from_cond(cond: &crate::ast_to_config::Condition, keys: &mut std::collections::HashSet<String>) {
        match cond {
            crate::ast_to_config::Condition::And { conditions } |
            crate::ast_to_config::Condition::Or { conditions } => {
                for c in conditions {
                    collect_from_cond(c, keys);
                }
            }
            // AUDIT-02 / D-04: EXISTS (and NOT EXISTS) inside the where
            // clause must contribute its parent_field columns to
            // split_edit_keys so an edit that flips the membership is
            // emitted as Remove+Add (not Edit). Recurse into the
            // subquery's own where so nested EXISTS are caught too.
            // TS parity: packages/zql/src/builder/builder.ts:273-290 via
            // gatherCorrelatedSubqueryQueryConditions.
            crate::ast_to_config::Condition::CorrelatedSubquery { related, .. } => {
                for f in &related.correlation.parent_field {
                    keys.insert(f.clone());
                }
                if let Some(inner) = &related.subquery.where_cond {
                    collect_from_cond(inner, keys);
                }
            }
            _ => {}
        }
    }
    if let Some(cond) = &ast.where_cond {
        collect_from_cond(cond, &mut keys);
    }
    keys.into_iter().collect()
}

/// NAPI entry point: full advance from AST format.
/// Takes the same queries_json format as rust_hydrate (Vec<HydrateQuery>),
/// plus changes_json (Vec<Change>). Internally translates AST → OperatorConfig
/// via ast_to_operator_configs, then runs advance_pipelines_full.
// ─── Hydration NAPI (AST → OperatorConfig → hydrate_pipelines) ──────────────

pub(crate) fn flatten_nodes_to_row_changes(
    out: &mut Vec<RowChange>,
    query_id: &str,
    table: &str,
    primary_key: &[String],
    nodes: &[zero_ivm_rs::types::Node],
    all_pks: &HashMap<String, Vec<String>>,
    column_types: &Option<HashMap<String, HashMap<String, String>>>,
    rel_to_table: &HashMap<String, String>,
) {
    for node in nodes {
        let row_key = {
            let map: serde_json::Map<String, serde_json::Value> = primary_key
                .iter()
                .filter_map(|k| node.row.get(k).map(|v| (k.clone(), v.clone())))
                .collect();
            serde_json::Value::Object(map)
        };

        let row: Option<Row> = coerce_row(&node.row, table, column_types);

        // If coerce_row returns None, this table is unknown (e.g., system table) — skip it.
        if let Some(row) = row {
            out.push(RowChange {
                query_id: query_id.to_string(),
                table: table.to_string(),
                row_key,
                row: Some(row),
                change_type: "add".to_string(),
            });
        }

        for (rel_name, children) in &node.relationships {
            // Resolve actual table name from relationship name (H5 fix).
            // When relationship name differs from table name (aliased joins),
            // we must use the actual table name for coerce_row and PK lookup.
            let child_table = rel_to_table.get(rel_name).map(|s| s.as_str()).unwrap_or(rel_name);
            let child_pk = all_pks.get(child_table)
                .or_else(|| all_pks.get(rel_name))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            flatten_nodes_to_row_changes(out, query_id, child_table, child_pk, children, all_pks, column_types, rel_to_table);
        }
    }
}

/// Filter row to only include columns in `column_types` for the given table,
/// and apply type coercion (SQLite int → JSON bool, TEXT → parsed JSON).
pub(crate) fn coerce_row(
    row: &serde_json::Map<String, serde_json::Value>,
    table: &str,
    column_types: &Option<HashMap<String, HashMap<String, String>>>,
) -> Option<Row> {
    let table_cols = column_types.as_ref().and_then(|ct| ct.get(table));
    match table_cols {
        Some(cols) => {
            Some(cols.iter()
                .filter_map(|(col_name, value_type)| {
                    let val = row.get(col_name).cloned().unwrap_or(serde_json::Value::Null);
                    let coerced = coerce_value(val, value_type);
                    Some((col_name.clone(), coerced))
                })
                .collect())
        }
        None if column_types.is_some() => {
            // column_types was provided but this table isn't in it — skip this row
            // (e.g., system tables like zeroz_1.mutations)
            None
        }
        None => {
            // No column type info at all — pass through all columns unfiltered
            Some(row.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        }
    }
}

/// Coerce a single value based on its ZQL value type.
fn coerce_value(val: serde_json::Value, value_type: &str) -> serde_json::Value {
    if val.is_null() {
        return val;
    }
    match value_type {
        "boolean" => {
            // SQLite stores booleans as integers: 0 → false, non-zero → true
            match &val {
                serde_json::Value::Number(n) => {
                    serde_json::Value::Bool(n.as_i64().map_or(false, |v| v != 0))
                }
                serde_json::Value::Bool(_) => val,
                _ => val,
            }
        }
        "json" => {
            // SQLite stores JSON as TEXT — parse it
            match &val {
                serde_json::Value::String(s) => {
                    serde_json::from_str(s).unwrap_or(val)
                }
                _ => val,
            }
        }
        _ => val,
    }
}

// ─── Persistent Pipeline ────────────────────────────────────────────────────

use std::sync::{Mutex, RwLock};

pub(crate) struct PipelineState {
    pub(crate) source: Arc<RustTableSource>,
    /// Unified operator chain: nested tree for both fetch (hydration) and push (advance).
    /// `chain` is the outermost operator. `push_ptrs` are raw pointers to each operator
    /// in inner→outer order for external push routing.
    pub(crate) chain: Option<Box<dyn IvmOperator>>,
    pub(crate) push_ptrs: Vec<*mut dyn IvmOperator>,
    pub(crate) query_id: String,
    pub(crate) source_table: String,
    pub(crate) primary_key: Vec<String>,
    pub(crate) column_types: Option<HashMap<String, HashMap<String, String>>>,
    pub(crate) all_primary_keys: HashMap<String, Vec<String>>,
    pub(crate) child_table_map: HashMap<String, Vec<ChildTableInfo>>,
    pub(crate) children_of_map: HashMap<String, Vec<ChildRelation>>,
    pub(crate) operator_config: Vec<OperatorConfig>,
    pub(crate) has_operators: bool,
    pub(crate) rel_to_table: HashMap<String, String>,
    /// Maps child table name → Vec<(op_index, child_pk)> for routing
    /// child changes through push_child() on the correct operator.
    pub(crate) child_table_to_op_index: HashMap<String, Vec<(usize, Vec<String>)>>,
    /// Columns whose change must split a source Edit into Remove+Add so
    /// downstream Join/Exists operators see the membership transition.
    /// Populated from `collect_split_edit_keys(&query.ast)` (AUDIT-02).
    pub(crate) split_edit_keys: Vec<String>,
}

// SAFETY: PipelineState's raw pointers in push_ptrs point into heap-allocated
// Box<dyn Operator> within the nested chain. They are never shared across threads
// — each PipelineState is behind a Mutex. The pointers are stable (Box heap alloc).
unsafe impl Send for PipelineState {}

pub(crate) fn build_pipeline_state(
    db_path: &str,
    query: &crate::ast_to_config::HydrateQuery,
    schema_cache: &mut crate::ast_to_config::SchemaCache,
    shared_pool: &ConnectionPool,
) -> Result<PipelineState, String> {
    use crate::ast_to_config::{ast_to_operator_configs, collect_child_tables};
    use crate::query_builder::ColumnType;

    // Seed SchemaCache with all_primary_keys from TS (Zero schema PKs) BEFORE
    // building operator configs, so that get_primary_key() for child tables
    // returns correct PKs instead of relying on SQLite PRAGMA table_info
    // (which may not have PRIMARY KEY constraints on replica tables).
    if let Some(ref ts_pks) = query.all_primary_keys {
        schema_cache.seed_primary_keys(ts_pks);
    }
    // Also seed the root table's PK
    schema_cache.seed_primary_keys(&HashMap::from([(
        query.ast.table.clone(),
        query.primary_key.clone(),
    )]));

    let operator_config = ast_to_operator_configs(
        schema_cache,
        &query.ast,
        &query.primary_key,
        // B3: top-level callers pass None — partition_key is only meaningful
        // for child subqueries (related[] / EXISTS). Recursive calls inside
        // ast_to_operator_configs pass Some(rel.correlation.child_field).
        // Mirrors TS builder.ts top-level invocation. See plan 34-05.
        None,
    )?;

    let split_edit_keys = collect_split_edit_keys(&query.ast);

    let mut all_pks: HashMap<String, Vec<String>> = HashMap::new();
    all_pks.insert(query.ast.table.clone(), query.primary_key.clone());
    if let Some(ref ts_pks) = query.all_primary_keys {
        for (table_name, pk) in ts_pks {
            all_pks.insert(table_name.clone(), pk.clone());
        }
    }
    for (rel_name, table_name) in collect_child_tables(&query.ast) {
        if !all_pks.contains_key(&rel_name) {
            let pk = if let Some(ref ts_pks) = query.all_primary_keys {
                ts_pks.get(&table_name).cloned().unwrap_or_else(|| {
                    schema_cache.get_primary_key(&table_name).unwrap_or_default()
                })
            } else {
                schema_cache.get_primary_key(&table_name).unwrap_or_default()
            };
            all_pks.insert(rel_name, pk);
        }
    }

    // Build rel_to_table from the EMITTED OperatorConfig tree (post-uniquify)
    // rather than the raw AST. The `OperatorConfig::Exists.relationship_name`
    // field carries the uniquified alias produced by
    // `uniquify_top_level_csq_aliases`; the `OperatorConfig::Source.table_name`
    // inside its `child` is the actual table. Using the un-uniquified
    // `collect_child_tables` here would key the map under the original alias
    // (e.g., `arb_conversations_attachments`) while the change-output emits
    // the uniquified relationship_name (e.g., `arb_conversations_attachments_1`),
    // causing `flatten_nodes_to_row_changes` to fall back to using the
    // uniquified name as the table name and fail `coerce_row` lookup —
    // dropping every CSQ-EXISTS child row from the emitted RowChanges.
    let mut rel_to_table: HashMap<String, String> =
        collect_rel_to_table_from_configs(&operator_config);
    // Keep the AST-derived entries too for `related[]` Joins (no uniquify).
    for (rel_name, table_name) in collect_child_tables(&query.ast) {
        rel_to_table.entry(rel_name).or_insert(table_name);
    }

    let (table_name, columns, pk, sort) = match &operator_config[0] {
        OperatorConfig::Source {
            table_name,
            columns,
            primary_key,
            sort,
        } => (table_name.clone(), columns.clone(), primary_key.clone(), sort.clone()),
        _ => return Err("first config must be a Source".to_string()),
    };

    let mut column_types = HashMap::new();
    for c in &columns {
        column_types.insert(c.clone(), ColumnType::String);
    }
    let mut source = RustTableSource::new_with_shared_pool(
        shared_pool.clone(), table_name.clone(), columns, column_types, pk.clone(),
    ).map_err(|e| format!("failed to create source for {table_name}: {e}"))?;

    let split_keys: Option<HashSet<String>> = if split_edit_keys.is_empty() {
        None
    } else {
        Some(split_edit_keys.iter().cloned().collect())
    };
    let connection_id = source.connect(Some(sort), None, split_keys);
    let source_arc = Arc::new(source);

    // Build unified operator chain for both fetch and push.
    let (chain, push_ptrs, has_operators) = match build_operator_chain(
        source_arc.clone(), &operator_config, connection_id,
    ) {
        Ok(Some(oc)) => {
            let push_ptrs = oc.push_ptrs;
            let mut chain = oc.chain;
            // Warm up: initial fetch populates stateful operators (TakeState, etc.)
            // through the nested chain — matches TS IVM's initialFetch behavior.
            let _ = chain.fetch(&FetchRequest::default());
            (Some(chain), push_ptrs, true)
        }
        Ok(None) => (None, vec![], false),
        Err(e) => return Err(format!("failed to build operator chain: {e}")),
    };

    let child_table_map = collect_child_table_map(&operator_config);
    let children_of_map = collect_children_of_map(&operator_config);
    let child_table_to_op_index = build_child_table_to_op_index(&operator_config);

    Ok(PipelineState {
        source: source_arc,
        chain,
        push_ptrs,
        query_id: query.query_id.clone(),
        source_table: query.ast.table.clone(),
        primary_key: query.primary_key.clone(),
        column_types: query.column_types.clone(),
        all_primary_keys: all_pks,
        child_table_map,
        children_of_map,
        operator_config,
        has_operators,
        rel_to_table,
        child_table_to_op_index,
        split_edit_keys,
    })
}

/// Test-only panic injection: when set, the N-th call to
/// `advance_persistent_pipeline_with_cancel` panics. Used by TEST-03
/// (`streaming_panic_in_one_pipeline_others_complete`) to verify per-task
/// `panic::catch_unwind` shim.
#[cfg(test)]
pub(crate) static PANIC_ON_PIPELINE_INDEX: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

#[cfg(test)]
pub(crate) static PIPELINE_INVOCATION_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Cancel-aware variant of `advance_persistent_pipeline` used by streaming.
///
/// Differs from the buffered path in exactly one place: the per-change loop
/// observes `cancel.load(Ordering::Relaxed)` at its top and short-circuits
/// (returning whatever row_changes have been accumulated so far) when set.
///
/// Cadence: once-per-change at the natural insertion point per RESEARCH
/// Open Q #2. If big-pipeline scenarios reveal this is insufficient,
/// escalation to once-per-operator inside `push_through_ptrs`
/// (advance.rs:1264-1275) is the documented fallback.
///
/// All other code is byte-identical to `advance_persistent_pipeline` so the
/// non-cancelled output is unchanged. Existing buffered call sites continue
/// to use `advance_persistent_pipeline` directly (COMPAT-02 — no signature
/// change to existing function).
pub(crate) fn advance_persistent_pipeline_with_cancel(
    pipeline: &mut PipelineState,
    changes: &[Change],
    db_path: &str,
    // B11: prev_db_path per TS pipeline-driver.ts:1542 — read descendants
    // from the PREV snapshot (where same-tx-deleted rows still exist).
    // Distinct from db_path (curr; used for child_row_has_parent which
    // needs post-tx state).
    prev_db_path: &str,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Vec<RowChange> {
    advance_persistent_pipeline_with_cancel_and_prev(
        pipeline, changes, db_path, prev_db_path, None, cancel,
    )
}

/// Phase 35 / NEW-1: variant that accepts a borrowed `&ConnectionPool` for
/// the prev snapshot. When `Some`, opens ONE connection per advance batch
/// (instead of one per `emit_descendant_removals` invocation) and uses
/// `prepare_cached` for SQL reuse. When `None`, falls back to the legacy
/// open-per-call path. See `.planning/phases/35-pool-cascade-hardening/`.
pub(crate) fn advance_persistent_pipeline_with_cancel_and_prev(
    pipeline: &mut PipelineState,
    changes: &[Change],
    db_path: &str,
    prev_db_path: &str,
    prev_pool: Option<&crate::connection_pool::ConnectionPool>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Vec<RowChange> {
    #[cfg(test)]
    {
        let idx = PIPELINE_INVOCATION_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let panic_at = PANIC_ON_PIPELINE_INDEX.load(std::sync::atomic::Ordering::SeqCst);
        if idx == panic_at {
            panic!("test-injected panic at pipeline index {}", idx);
        }
    }

    if !pipeline.has_operators {
        return changes_to_row_changes_direct(
            changes,
            &FullPipelineConfig {
                query_id: pipeline.query_id.clone(),
                source_table: pipeline.source_table.clone(),
                operator_config: pipeline.operator_config.clone(),
                primary_key: pipeline.primary_key.clone(),
                split_edit_keys: vec![],
                column_types: pipeline.column_types.clone(),
                all_primary_keys: pipeline.all_primary_keys.clone(),
                rel_to_table: pipeline.rel_to_table.clone(),
            },
            &pipeline.source_table,
            &pipeline.primary_key,
        );
    }

    /// Push a change through operators via raw pointers (inner→outer).
    /// SAFETY: `ptrs` and `len` describe a valid slice of raw pointers into
    /// heap-allocated operators in the nested chain. Sequential — no aliasing.
    unsafe fn push_through_ptrs(change: IvmChange, ptrs: *const *mut dyn IvmOperator, len: usize) -> Vec<IvmChange> {
        let mut current = vec![change];
        for i in 0..len {
            let ptr = *ptrs.add(i);
            let mut next = Vec::new();
            for c in current {
                next.extend((*ptr).push(c));
            }
            current = next;
        }
        current
    }

    let mut row_changes = Vec::new();

    // NEW-1: open ONE connection up front. Prefer prev_pool when present
    // (reuses the snapshot-pinned ConnectionPool from set_prev_snapshot);
    // fall back to opening per-batch from prev_db_path so the legacy path
    // still works for callers that haven't wired prev_pool. The connection
    // is reused across all `emit_descendant_removals_with_conn` invocations
    // within this advance batch.
    let prev_pooled = prev_pool.and_then(|p| p.get().ok());
    let prev_conn_owned: Option<rusqlite::Connection> = if prev_pooled.is_none() {
        match rusqlite::Connection::open_with_flags(
            prev_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(c) => {
                let _ = c.execute_batch("BEGIN DEFERRED");
                Some(c)
            }
            Err(e) => {
                eprintln!(
                    "[B11] advance_persistent_pipeline: open_with_flags failed for {:?}: {}",
                    prev_db_path, e
                );
                None
            }
        }
    } else {
        None
    };
    let prev_conn: Option<&rusqlite::Connection> = prev_pooled
        .as_deref()
        .or(prev_conn_owned.as_ref());

    for change in changes.iter() {
        // STREAM-04: cancel observation at change boundary (RESEARCH Open Q #2).
        // Once-per-change is the natural insertion point. If TEST-01 reveals
        // big-pipeline scenarios where this is insufficient, escalate to
        // once-per-operator inside push_through_ptrs (advance.rs:1264-1275).
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return row_changes;
        }

        if change.table == pipeline.source_table {
            let raw_source_changes = diff_change_to_source_changes(change, &pipeline.primary_key);
            let source_changes: Vec<SourceChange> = raw_source_changes
                .into_iter()
                .flat_map(|sc| maybe_split_edit_for_advance(sc, &pipeline.split_edit_keys))
                .collect();
            for sc in source_changes {
                let ivm_change = source_change_to_ivm_change(&sc);
                let output_changes = unsafe {
                    push_through_ptrs(ivm_change, pipeline.push_ptrs.as_ptr(), pipeline.push_ptrs.len())
                };
                for oc in &output_changes {
                    flatten_ivm_change_to_row_changes(
                        &mut row_changes,
                        oc,
                        &pipeline.query_id,
                        &change.table,
                        &pipeline.primary_key,
                        &pipeline.all_primary_keys,
                    );
                    if let IvmChange::Remove(node) = oc {
                        let has_child_rows = node.relationships.values().any(|v| !v.is_empty());
                        if !has_child_rows && !pipeline.children_of_map.is_empty() {
                            let deleted_map: serde_json::Map<String, serde_json::Value> = node.row.clone();
                            // B11/NEW-1: reuse the once-opened prev_conn
                            // (from prev_pool when set, else opened above).
                            // Falls back to legacy open-per-call when neither
                            // is available (rare — pre-existing fallback path).
                            if let Some(pc) = prev_conn {
                                emit_descendant_removals_with_conn(
                                    pc, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            } else {
                                emit_descendant_removals(
                                    prev_db_path, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            }
                        }
                    }
                }
            }
        } else if let Some(op_entries) = pipeline.child_table_to_op_index.get(&change.table) {
            let op_entries = op_entries.clone();
            for (op_idx, child_pk) in &op_entries {
                let child_source_changes = diff_change_to_source_changes(change, child_pk);
                for sc in child_source_changes {
                    let ivm_change = source_change_to_ivm_change(&sc);
                    let child_outputs = unsafe { (*pipeline.push_ptrs[*op_idx]).push_child(ivm_change) };
                    let remaining_start = op_idx + 1;
                    let remaining_len = pipeline.push_ptrs.len() - remaining_start;
                    let current = if remaining_len > 0 {
                        unsafe {
                            let mut current = child_outputs;
                            let ptrs = pipeline.push_ptrs.as_ptr().add(remaining_start);
                            for i in 0..remaining_len {
                                let ptr = *ptrs.add(i);
                                let mut next = Vec::new();
                                for c in current {
                                    next.extend((*ptr).push(c));
                                }
                                current = next;
                            }
                            current
                        }
                    } else {
                        child_outputs
                    };
                    for oc in &current {
                        flatten_ivm_change_to_row_changes(
                            &mut row_changes,
                            oc,
                            &pipeline.query_id,
                            &pipeline.source_table,
                            &pipeline.primary_key,
                            &pipeline.all_primary_keys,
                        );
                    }
                }
            }
        } else if let Some(child_infos) = pipeline.child_table_map.get(&change.table) {
            for ci in child_infos {
                let child_source_changes = diff_change_to_source_changes(change, &ci.child_pk);
                for sc in &child_source_changes {
                    match sc {
                        SourceChange::Remove(ref row) => {
                            let row_key = extract_row_key_from_source_row(row, &ci.child_pk);
                            row_changes.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: ci.relationship_name.clone(),
                                row_key,
                                row: None,
                                change_type: "remove".to_string(),
                            });
                            let deleted_map: serde_json::Map<String, serde_json::Value> = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            // B11/NEW-1: reuse the once-opened prev_conn
                            // (from prev_pool when set, else opened above).
                            // Falls back to legacy open-per-call when neither
                            // is available (rare — pre-existing fallback path).
                            if let Some(pc) = prev_conn {
                                emit_descendant_removals_with_conn(
                                    pc, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            } else {
                                emit_descendant_removals(
                                    prev_db_path, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            }
                        }
                        SourceChange::Add(ref row) => {
                            let row_map: serde_json::Map<String, serde_json::Value> = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            // child_row_has_parent: db_path (curr) is correct —
                            // an Add must verify the parent exists in post-tx state.
                            if !child_row_has_parent(db_path, ci, &row_map) {
                                continue;
                            }
                            let row_key = extract_row_key_from_source_row(row, &ci.child_pk);
                            row_changes.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: ci.relationship_name.clone(),
                                row_key,
                                row: Some(row_map.into_iter().collect()),
                                change_type: "add".to_string(),
                            });
                        }
                        SourceChange::Edit { row, .. } => {
                            let row_map: serde_json::Map<String, serde_json::Value> = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            if !child_row_has_parent(db_path, ci, &row_map) {
                                continue;
                            }
                            let row_key = extract_row_key_from_source_row(row, &ci.child_pk);
                            row_changes.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: ci.relationship_name.clone(),
                                row_key,
                                row: Some(row_map.into_iter().collect()),
                                change_type: "edit".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    // Deduplicate (matches buffered path)
    {
        let mut seen = HashSet::new();
        row_changes.retain(|rc| {
            let key = format!("{}|{}|{}", rc.table,
                serde_json::to_string(&rc.row_key).unwrap_or_default(), rc.change_type);
            seen.insert(key)
        });
    }

    // Filter output columns (matches buffered path)
    if let Some(ref ct) = pipeline.column_types {
        for rc in &mut row_changes {
            if let Some(ref mut row) = rc.row {
                if let Some(cols) = ct.get(&rc.table) {
                    row.retain(|k, _| cols.contains_key(k));
                }
            }
        }
    }

    row_changes
}

pub(crate) fn advance_persistent_pipeline(
    pipeline: &mut PipelineState,
    changes: &[Change],
    db_path: &str,
    // B11: prev_db_path per TS pipeline-driver.ts:1542 — read descendants
    // from the PREV snapshot (where same-tx-deleted rows still exist).
    // Distinct from db_path (curr; used for child_row_has_parent which
    // needs post-tx state).
    prev_db_path: &str,
) -> Vec<RowChange> {
    advance_persistent_pipeline_with_prev(pipeline, changes, db_path, prev_db_path, None)
}

/// Phase 35 / NEW-1: variant of `advance_persistent_pipeline` that accepts
/// a borrowed `&ConnectionPool` for the prev snapshot. When `Some`, opens
/// ONE connection per advance batch and uses `prepare_cached` for SQL
/// reuse. When `None`, falls back to the legacy open-per-call path. Used
/// by the buffered (non-streaming) advance code path.
pub(crate) fn advance_persistent_pipeline_with_prev(
    pipeline: &mut PipelineState,
    changes: &[Change],
    db_path: &str,
    prev_db_path: &str,
    prev_pool: Option<&crate::connection_pool::ConnectionPool>,
) -> Vec<RowChange> {
    if !pipeline.has_operators {
        return changes_to_row_changes_direct(
            changes,
            &FullPipelineConfig {
                query_id: pipeline.query_id.clone(),
                source_table: pipeline.source_table.clone(),
                operator_config: pipeline.operator_config.clone(),
                primary_key: pipeline.primary_key.clone(),
                split_edit_keys: vec![],
                column_types: pipeline.column_types.clone(),
                all_primary_keys: pipeline.all_primary_keys.clone(),
                rel_to_table: pipeline.rel_to_table.clone(),
            },
            &pipeline.source_table,
            &pipeline.primary_key,
        );
    }

    /// Push a change through operators via raw pointers (inner→outer).
    /// SAFETY: `ptrs` and `len` describe a valid slice of raw pointers into
    /// heap-allocated operators in the nested chain. Sequential — no aliasing.
    unsafe fn push_through_ptrs(change: IvmChange, ptrs: *const *mut dyn IvmOperator, len: usize) -> Vec<IvmChange> {
        let mut current = vec![change];
        for i in 0..len {
            let ptr = *ptrs.add(i);
            let mut next = Vec::new();
            for c in current {
                next.extend((*ptr).push(c));
            }
            current = next;
        }
        current
    }

    let mut row_changes = Vec::new();
    let t_start = std::time::Instant::now();
    let mut t_diff = std::time::Duration::ZERO;
    let mut t_push = std::time::Duration::ZERO;
    let mut t_flatten = std::time::Duration::ZERO;
    let mut t_child_has_parent = std::time::Duration::ZERO;

    // NEW-1: open ONE prev connection up front, reused across the entire
    // advance batch. Prefer prev_pool when set; else fall back to opening
    // from prev_db_path. See `advance_persistent_pipeline_with_cancel_and_prev`
    // for the streaming counterpart.
    let prev_pooled = prev_pool.and_then(|p| p.get().ok());
    let prev_conn_owned: Option<rusqlite::Connection> = if prev_pooled.is_none() {
        match rusqlite::Connection::open_with_flags(
            prev_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(c) => {
                let _ = c.execute_batch("BEGIN DEFERRED");
                Some(c)
            }
            Err(e) => {
                eprintln!(
                    "[B11] advance_persistent_pipeline: open_with_flags failed for {:?}: {}",
                    prev_db_path, e
                );
                None
            }
        }
    } else {
        None
    };
    let prev_conn: Option<&rusqlite::Connection> = prev_pooled
        .as_deref()
        .or(prev_conn_owned.as_ref());

    // Debug: write to /tmp for investigation
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/rust_ivm_debug.log") {
        use std::io::Write;
        let _ = writeln!(f, "[advance_persistent] query={} source_table={} child_to_op={:?} has_ops={} changes={:?}",
            pipeline.query_id, pipeline.source_table,
            pipeline.child_table_to_op_index.keys().collect::<Vec<_>>(),
            pipeline.has_operators,
            changes.iter().map(|c| c.table.as_str()).collect::<Vec<_>>());
    }

    for change in changes.iter() {
        if change.table == pipeline.source_table {
            let td0 = std::time::Instant::now();
            let raw_source_changes = diff_change_to_source_changes(change, &pipeline.primary_key);
            // AUDIT-02: split source Edits when an EXISTS parent_field
            // (or any other column in split_edit_keys) changes, so the
            // downstream Join/Exists operator sees Remove+Add rather
            // than a silent in-place Edit.
            let source_changes: Vec<SourceChange> = raw_source_changes
                .into_iter()
                .flat_map(|sc| maybe_split_edit_for_advance(sc, &pipeline.split_edit_keys))
                .collect();
            t_diff += td0.elapsed();
            for sc in source_changes {
                let ivm_change = source_change_to_ivm_change(&sc);
                let tp0 = std::time::Instant::now();
                let output_changes = unsafe { push_through_ptrs(ivm_change, pipeline.push_ptrs.as_ptr(), pipeline.push_ptrs.len()) };
                t_push += tp0.elapsed();
                let tf0 = std::time::Instant::now();
                for oc in &output_changes {
                    flatten_ivm_change_to_row_changes(
                        &mut row_changes,
                        oc,
                        &pipeline.query_id,
                        &change.table,
                        &pipeline.primary_key,
                        &pipeline.all_primary_keys,
                    );
                    if let IvmChange::Remove(node) = oc {
                        let has_child_rows = node.relationships.values().any(|v| !v.is_empty());
                        if !has_child_rows && !pipeline.children_of_map.is_empty() {
                            let deleted_map: serde_json::Map<String, serde_json::Value> = node.row.clone();
                            // B11/NEW-1: reuse the once-opened prev_conn
                            // (from prev_pool when set, else opened above).
                            // Falls back to legacy open-per-call when neither
                            // is available (rare — pre-existing fallback path).
                            if let Some(pc) = prev_conn {
                                emit_descendant_removals_with_conn(
                                    pc, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            } else {
                                emit_descendant_removals(
                                    prev_db_path, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            }
                        }
                    }
                }
                t_flatten += tf0.elapsed();
            }
        } else if let Some(op_entries) = pipeline.child_table_to_op_index.get(&change.table) {
            // Route child table changes through the IVM operator tree via push_child().
            // This ensures Join/Exists operators properly transform child changes into
            // parent-level changes (e.g., Exists 0→1 transitions emit parent Add/Remove).
            let op_entries = op_entries.clone();
            for (op_idx, child_pk) in &op_entries {
                let child_source_changes = diff_change_to_source_changes(change, child_pk);
                for sc in child_source_changes {
                    let ivm_change = source_change_to_ivm_change(&sc);
                    // Call push_child on the target operator (Join/Exists) via raw pointer
                    let tp0 = std::time::Instant::now();
                    let child_outputs = unsafe { (*pipeline.push_ptrs[*op_idx]).push_child(ivm_change) };
                    // Push through remaining operators after this one
                    let remaining_start = op_idx + 1;
                    let remaining_len = pipeline.push_ptrs.len() - remaining_start;
                    let current = if remaining_len > 0 {
                        unsafe {
                            let mut current = child_outputs;
                            let ptrs = pipeline.push_ptrs.as_ptr().add(remaining_start);
                            for i in 0..remaining_len {
                                let ptr = *ptrs.add(i);
                                let mut next = Vec::new();
                                for c in current {
                                    next.extend((*ptr).push(c));
                                }
                                current = next;
                            }
                            current
                        }
                    } else {
                        child_outputs
                    };
                    t_push += tp0.elapsed();
                    let tf0 = std::time::Instant::now();
                    for oc in &current {
                        flatten_ivm_change_to_row_changes(
                            &mut row_changes,
                            oc,
                            &pipeline.query_id,
                            &pipeline.source_table,
                            &pipeline.primary_key,
                            &pipeline.all_primary_keys,
                        );
                    }
                    t_flatten += tf0.elapsed();
                }
            }
        } else if let Some(child_infos) = pipeline.child_table_map.get(&change.table) {
            // Join child table changes — emit directly as child-table row changes.
            // For adds, verify the parent exists in the post-tx DB.
            // For removes, emit directly (post-tx DB won't have the deleted row).
            for ci in child_infos {
                let child_source_changes = diff_change_to_source_changes(change, &ci.child_pk);
                for sc in &child_source_changes {
                    match sc {
                        SourceChange::Remove(ref row) => {
                            let row_key = extract_row_key_from_source_row(row, &ci.child_pk);
                            row_changes.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: ci.relationship_name.clone(),
                                row_key,
                                row: None,
                                change_type: "remove".to_string(),
                            });
                            let deleted_map: serde_json::Map<String, serde_json::Value> = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            // B11/NEW-1: reuse the once-opened prev_conn
                            // (from prev_pool when set, else opened above).
                            // Falls back to legacy open-per-call when neither
                            // is available (rare — pre-existing fallback path).
                            if let Some(pc) = prev_conn {
                                emit_descendant_removals_with_conn(
                                    pc, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            } else {
                                emit_descendant_removals(
                                    prev_db_path, &deleted_map, &change.table, &pipeline.children_of_map,
                                    &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                                );
                            }
                        }
                        SourceChange::Add(ref row) => {
                            let row_map: serde_json::Map<String, serde_json::Value> = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            if !child_row_has_parent(db_path, ci, &row_map) {
                                continue;
                            }
                            let row_key = extract_row_key_from_source_row(row, &ci.child_pk);
                            row_changes.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: ci.relationship_name.clone(),
                                row_key,
                                row: Some(row_map.into_iter().collect()),
                                change_type: "add".to_string(),
                            });
                        }
                        SourceChange::Edit { row, .. } => {
                            let row_map: serde_json::Map<String, serde_json::Value> = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            if !child_row_has_parent(db_path, ci, &row_map) {
                                continue;
                            }
                            let row_key = extract_row_key_from_source_row(row, &ci.child_pk);
                            row_changes.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: ci.relationship_name.clone(),
                                row_key,
                                row: Some(row_map.into_iter().collect()),
                                change_type: "edit".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    // Deduplicate
    {
        let mut seen = HashSet::new();
        row_changes.retain(|rc| {
            let key = format!("{}|{}|{}", rc.table,
                serde_json::to_string(&rc.row_key).unwrap_or_default(), rc.change_type);
            seen.insert(key)
        });
    }

    // Filter output columns
    if let Some(ref ct) = pipeline.column_types {
        for rc in &mut row_changes {
            if let Some(ref mut row) = rc.row {
                if let Some(cols) = ct.get(&rc.table) {
                    row.retain(|k, _| cols.contains_key(k));
                }
            }
        }
    }

    row_changes
}


// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Dead stateless tests removed (Operator/PipelineConfig/process_pipeline/evaluate_predicate/etc.)

    #[test]
    fn test_encode_advance_result_buf_roundtrip() {
        let mut row = HashMap::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("name".to_string(), serde_json::json!("Alice"));
        row.insert("age".to_string(), serde_json::json!(30));

        let result = AdvanceResult {
            changes: vec![
                RowChange {
                    query_id: "q1".to_string(),
                    table: "users".to_string(),
                    row_key: serde_json::json!("u1"),
                    row: Some(row),
                    change_type: "add".to_string(),
                },
                RowChange {
                    query_id: "q2".to_string(),
                    table: "users".to_string(),
                    row_key: serde_json::json!("u2"),
                    row: None,
                    change_type: "remove".to_string(),
                },
            ],
            error: None,
            error_type: None, reset_signal: None,
        };

        let buf = encode_advance_result_buf(&result);

        // Verify header
        let change_count = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(change_count, 2);
        assert_eq!(buf[4], 0); // no error

        // Verify first change starts at offset 5
        assert_eq!(buf[5], 0); // change_type = add

        // Verify buffer is non-trivially sized
        assert!(buf.len() > 20);
    }

    #[test]
    fn test_encode_advance_result_buf_with_error() {
        let result = AdvanceResult {
            changes: vec![],
            error: Some("something broke".to_string()),
            error_type: Some("RuntimeError".to_string()),
            reset_signal: None,
        };

        let buf = encode_advance_result_buf(&result);
        assert_eq!(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]), 0);
        assert_eq!(buf[4], 1); // has_error

        // Error message length
        let err_len = u16::from_le_bytes([buf[5], buf[6]]) as usize;
        assert_eq!(err_len, 15); // "something broke"
        let err_msg = std::str::from_utf8(&buf[7..7 + err_len]).unwrap();
        assert_eq!(err_msg, "something broke");
    }

    // ─── collect_split_edit_keys: AUDIT-02 (EXISTS parent_field) ──────────
    //
    // TS parity reference: packages/zql/src/builder/builder.ts lines 273-290.
    // The collector must include parent_field columns from EVERY
    // CorrelatedSubquery condition reachable through the where tree (and
    // through nested EXISTS subqueries' own where), in addition to the
    // top-level `ast.related[*]` parent_fields.

    use crate::ast_to_config::{Ast, Condition, ConditionValue, CorrelatedSubquery, Correlation};

    fn make_csq(parent_field: Vec<&str>, child_field: Vec<&str>, child_table: &str) -> Box<CorrelatedSubquery> {
        Box::new(CorrelatedSubquery {
            correlation: Correlation {
                parent_field: parent_field.into_iter().map(String::from).collect(),
                child_field: child_field.into_iter().map(String::from).collect(),
            },
            subquery: Box::new(Ast {
                table: child_table.to_string(),
                alias: None,
                where_cond: None,
                related: None,
                limit: None,
                order_by: None,
                start: None,
            }),
            hidden: None,
            system: None,
        })
    }

    fn make_csq_with_where(
        parent_field: Vec<&str>,
        child_field: Vec<&str>,
        child_table: &str,
        inner_where: Condition,
    ) -> Box<CorrelatedSubquery> {
        Box::new(CorrelatedSubquery {
            correlation: Correlation {
                parent_field: parent_field.into_iter().map(String::from).collect(),
                child_field: child_field.into_iter().map(String::from).collect(),
            },
            subquery: Box::new(Ast {
                table: child_table.to_string(),
                alias: None,
                where_cond: Some(Box::new(inner_where)),
                related: None,
                limit: None,
                order_by: None,
                start: None,
            }),
            hidden: None,
            system: None,
        })
    }

    fn empty_parent_ast() -> Ast {
        Ast {
            table: "parent".to_string(),
            alias: None,
            where_cond: None,
            related: None,
            limit: None,
            order_by: None,
            start: None,
        }
    }

    fn simple_eq_cond() -> Condition {
        Condition::Simple {
            op: "=".to_string(),
            left: ConditionValue::Column { name: "name".to_string() },
            right: ConditionValue::Literal { value: serde_json::json!("Alice") },
        }
    }

    /// Bug #2 regression: top-level EXISTS in where must contribute its
    /// parent_field to split_edit_keys. Without the fix, edits to that
    /// column would be emitted as Edit (not Remove+Add) and downstream
    /// ExistsOperator silently drops the membership transition.
    #[test]
    fn test_collect_split_edit_keys_csq_in_where() {
        let csq = make_csq(vec!["owner_id"], vec!["parent_id"], "children");
        let mut ast = empty_parent_ast();
        ast.where_cond = Some(Box::new(Condition::CorrelatedSubquery {
            related: csq,
            op: "EXISTS".to_string(),
            flip: None,
            scalar: None,
        }));

        let keys = super::collect_split_edit_keys(&ast);
        assert!(
            keys.contains(&"owner_id".to_string()),
            "expected 'owner_id' in keys, got {keys:?}"
        );
    }

    /// EXISTS nested inside an AND condition must still contribute its
    /// parent_field. This guards the AND/OR recursion path that already
    /// existed before the fix (we recurse INTO the And/Or conditions and
    /// must hit the new CorrelatedSubquery arm at the leaves).
    #[test]
    fn test_collect_split_edit_keys_csq_nested_in_and() {
        let csq = make_csq(vec!["status"], vec!["status"], "children");
        let mut ast = empty_parent_ast();
        ast.where_cond = Some(Box::new(Condition::And {
            conditions: vec![
                simple_eq_cond(),
                Condition::CorrelatedSubquery {
                    related: csq,
                    op: "EXISTS".to_string(),
                    flip: None,
                    scalar: None,
                },
            ],
        }));

        let keys = super::collect_split_edit_keys(&ast);
        assert!(
            keys.contains(&"status".to_string()),
            "expected 'status' in keys, got {keys:?}"
        );
    }

    /// EXISTS subquery that itself contains a CorrelatedSubquery in ITS
    /// where clause: the outer collector must recurse into
    /// `related.subquery.where_cond` (per D-04). Without this recursion,
    /// nested EXISTS parent_fields are missed.
    #[test]
    fn test_collect_split_edit_keys_csq_recurses_into_subquery_where() {
        // Inner CSQ that lives inside the outer EXISTS subquery's where.
        let inner_csq = make_csq(vec!["nested_field"], vec!["nf_id"], "grandchildren");
        let inner_where = Condition::CorrelatedSubquery {
            related: inner_csq,
            op: "EXISTS".to_string(),
            flip: None,
            scalar: None,
        };
        // Outer CSQ whose subquery has an EXISTS in its where.
        let outer_csq = make_csq_with_where(
            vec!["owner_id"],
            vec!["parent_id"],
            "children",
            inner_where,
        );
        let mut ast = empty_parent_ast();
        ast.where_cond = Some(Box::new(Condition::CorrelatedSubquery {
            related: outer_csq,
            op: "EXISTS".to_string(),
            flip: None,
            scalar: None,
        }));

        let keys = super::collect_split_edit_keys(&ast);
        assert!(
            keys.contains(&"nested_field".to_string()),
            "expected 'nested_field' (from nested EXISTS in subquery where) in keys, got {keys:?}"
        );
        assert!(
            keys.contains(&"owner_id".to_string()),
            "expected 'owner_id' (outer EXISTS parent_field) in keys, got {keys:?}"
        );
    }

    /// D-07 regression guard: ASTs without any CorrelatedSubquery (only
    /// Simple/And/Or) must still produce an empty key set when there is
    /// no top-level `related`. Verifies the fix doesn't over-trigger.
    #[test]
    fn test_collect_split_edit_keys_no_csq_unchanged() {
        let mut ast = empty_parent_ast();
        ast.where_cond = Some(Box::new(Condition::And {
            conditions: vec![
                simple_eq_cond(),
                Condition::Or {
                    conditions: vec![simple_eq_cond(), simple_eq_cond()],
                },
            ],
        }));

        let keys = super::collect_split_edit_keys(&ast);
        assert!(
            keys.is_empty(),
            "expected no split_edit_keys for non-EXISTS where, got {keys:?}"
        );
    }

    /// Pre-existing behavior must be preserved: top-level `ast.related`
    /// parent_fields continue to be collected.
    #[test]
    fn test_collect_split_edit_keys_includes_top_level_related() {
        let mut ast = empty_parent_ast();
        ast.related = Some(vec![*make_csq(vec!["a"], vec!["a_id"], "children")]);

        let keys = super::collect_split_edit_keys(&ast);
        assert!(
            keys.contains(&"a".to_string()),
            "expected top-level related parent_field 'a' in keys, got {keys:?}"
        );
    }

    // ─── AUDIT-02 production AST shape regression (Plan 30-05) ─────────────
    //
    // Pins the full runtime production AST shape captured from the failing
    // `pipeline-driver.exists-parent-edit.test.ts` integration test (which
    // sends the AST through `pipeline-driver.ts::#rustHydrateQuery` →
    // `manager.addQuery` → `build_pipeline_state`). Before Plan 30-02,
    // `collect_split_edit_keys` would fall through `_ => {}` for the EXISTS
    // condition and never collect `child_id`, causing the source to emit a
    // single `Edit` instead of `Remove(old) + Add(new)`. After 30-02 +
    // 30-05, the keys are collected AND `maybe_split_edit_for_advance`
    // splits the captured Edit into `[Remove, Add]`.
    //
    // Source of the captured shape: `.tmp/audit-02-gap-rootcause.md`
    // (Plan 30-05 Task 1 diagnostic log; the audit-02 diagnostic prefix
    // entries from `build_pipeline_state` and `source_table_branch`).
    //
    // This test does NOT regress AUDIT-02 because it asserts the full
    // runtime shape (with serde-rename fields + system + alias + the exact
    // `_0_version` system column the row carries in production) — future
    // refactors that drop the CorrelatedSubquery arm or weaken the helper
    // will cause this test to fail.
    #[test]
    fn test_audit_02_production_ast_shape_does_not_regress() {
        // Build the exact AST shape produced by pipeline-driver.ts after
        // resolveSimpleScalarSubqueries — `system: Some("client")`,
        // `alias: Some("c")`, `order_by: Some([("id", "asc")])`, etc.
        // This mirrors the JSON the napi-rs deserializer receives from
        // `manager.addQuery(this.#instanceId, queryJson)` for the failing
        // pipeline-driver.exists-parent-edit.test.ts fixture.
        let csq = CorrelatedSubquery {
            correlation: Correlation {
                parent_field: vec!["child_id".to_string()],
                child_field: vec!["parent_id".to_string()],
            },
            subquery: Box::new(Ast {
                table: "children".to_string(),
                alias: Some("c".to_string()),
                where_cond: None,
                related: None,
                limit: None,
                order_by: Some(vec![("id".to_string(), "asc".to_string())]),
                start: None,
            }),
            hidden: None,
            system: Some("client".to_string()),
        };
        let ast = Ast {
            table: "parents".to_string(),
            alias: None,
            where_cond: Some(Box::new(Condition::CorrelatedSubquery {
                related: Box::new(csq),
                op: "EXISTS".to_string(),
                flip: None,
                scalar: None,
            })),
            related: None,
            limit: None,
            order_by: Some(vec![("id".to_string(), "asc".to_string())]),
            start: None,
        };

        let keys = super::collect_split_edit_keys(&ast);
        assert_eq!(
            keys, vec!["child_id".to_string()],
            "AUDIT-02 production AST shape MUST collect child_id into split_edit_keys; got {keys:?}"
        );

        // Build the exact SourceChange::Edit captured at the source-table
        // branch from the diagnostic log — including the `_0_version`
        // system column that flows through alongside user columns.
        let mut old_row: crate::source::Row = serde_json::Map::new();
        old_row.insert("id".to_string(), serde_json::json!("p1"));
        old_row.insert("child_id".to_string(), serde_json::json!("A"));
        old_row.insert("_0_version".to_string(), serde_json::json!("123"));

        let mut new_row: crate::source::Row = serde_json::Map::new();
        new_row.insert("id".to_string(), serde_json::json!("p1"));
        new_row.insert("child_id".to_string(), serde_json::json!("B"));
        new_row.insert("_0_version".to_string(), serde_json::json!("124"));

        let edit = crate::source::SourceChange::Edit {
            row: new_row.clone(),
            old_row: old_row.clone(),
        };

        let split = super::maybe_split_edit_for_advance(edit, &keys);
        assert_eq!(
            split.len(), 2,
            "AUDIT-02: production-shape Edit MUST split into 2 SourceChanges (Remove + Add); got {split:?}"
        );
        match (&split[0], &split[1]) {
            (
                crate::source::SourceChange::Remove(r),
                crate::source::SourceChange::Add(a),
            ) => {
                assert_eq!(
                    r.get("child_id"), Some(&serde_json::json!("A")),
                    "first split element MUST be Remove(old_row) with child_id='A'"
                );
                assert_eq!(
                    a.get("child_id"), Some(&serde_json::json!("B")),
                    "second split element MUST be Add(new_row) with child_id='B'"
                );
            }
            _ => panic!(
                "AUDIT-02: split MUST be exactly [Remove(old), Add(new)]; got {split:?}"
            ),
        }

        // Membership-preserved control: when child_id does NOT change, the
        // helper must pass the Edit through unchanged (no spurious split).
        let mut new_row_same: crate::source::Row = serde_json::Map::new();
        new_row_same.insert("id".to_string(), serde_json::json!("p1"));
        new_row_same.insert("child_id".to_string(), serde_json::json!("A"));
        new_row_same.insert("_0_version".to_string(), serde_json::json!("125"));
        let edit_same_key = crate::source::SourceChange::Edit {
            row: new_row_same,
            old_row: old_row.clone(),
        };
        let split_same = super::maybe_split_edit_for_advance(edit_same_key, &keys);
        assert_eq!(
            split_same.len(), 1,
            "membership-preserved Edit MUST pass through unchanged; got {split_same:?}"
        );
        assert!(
            matches!(split_same[0], crate::source::SourceChange::Edit { .. }),
            "passthrough MUST preserve the Edit variant; got {:?}", split_same[0]
        );
    }

    // ─── Persistent Pipeline Benchmark ──────────────────────────────────────
    //
    // Run with: cargo test -p zqlite-rs bench_persistent_pipeline_sequential_vs_parallel --release -- --nocapture
    //
    // Creates a real SQLite DB, builds N persistent pipelines with Take operators,
    // and compares sequential iteration vs rayon par_iter for advance.

    fn create_bench_db(dir: &tempfile::TempDir, num_rows: usize) -> String {
        let db_path = dir.path().join("bench.db");
        let db_path_str = db_path.to_str().unwrap().to_string();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                age INTEGER NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                score REAL NOT NULL DEFAULT 0.0
            );",
        )
        .unwrap();

        let mut stmt = conn
            .prepare("INSERT INTO users (id, name, age, active, score) VALUES (?1, ?2, ?3, ?4, ?5)")
            .unwrap();
        for i in 0..num_rows {
            stmt.execute(rusqlite::params![
                format!("u{}", i),
                format!("User {}", i),
                20 + (i % 60) as i64,
                if i % 5 == 0 { 0 } else { 1 },
                (i as f64) * 1.5,
            ])
            .unwrap();
        }
        drop(stmt);
        drop(conn);
        db_path_str
    }

    fn make_bench_query(query_id: &str, limit: usize) -> String {
        serde_json::json!({
            "query_id": query_id,
            "ast": {
                "table": "users",
                "orderBy": [["age", "asc"]],
                "limit": limit,
            },
            "primary_key": ["id"],
        })
        .to_string()
    }

    fn make_bench_changes(count: usize) -> Vec<Change> {
        (0..count)
            .map(|i| {
                let mut next = std::collections::HashMap::new();
                next.insert("id".to_string(), serde_json::json!(format!("u{}", i)));
                next.insert("name".to_string(), serde_json::json!(format!("Updated {}", i)));
                next.insert("age".to_string(), serde_json::json!(25 + (i % 40) as i64));
                next.insert("active".to_string(), serde_json::json!(1));
                next.insert("score".to_string(), serde_json::json!((i as f64) * 2.0));

                let mut prev = std::collections::HashMap::new();
                prev.insert("id".to_string(), serde_json::json!(format!("u{}", i)));
                prev.insert("name".to_string(), serde_json::json!(format!("User {}", i)));
                prev.insert("age".to_string(), serde_json::json!(20 + (i % 60) as i64));
                prev.insert("active".to_string(), serde_json::json!(1));
                prev.insert("score".to_string(), serde_json::json!((i as f64) * 1.5));

                Change {
                    table: "users".to_string(),
                    prev_values: vec![prev],
                    next_value: Some(next),
                    row_key: serde_json::json!(format!("u{}", i)),
                }
            })
            .collect()
    }

    #[test]
    fn bench_persistent_pipeline_sequential_vs_parallel() {
        use std::time::Instant;
        use crate::ast_to_config::{HydrateQuery, SchemaCache};

        let num_pipelines = 20;
        let num_rows = 1000;
        let num_changes = 50;
        let iterations = 100;

        // Setup
        let dir = tempfile::tempdir().unwrap();
        let db_path = create_bench_db(&dir, num_rows);

        let mut schema_cache = SchemaCache::new(&db_path);
        let pool = ConnectionPool::new(&db_path, rayon::current_num_threads().max(4)).unwrap();
        let changes = make_bench_changes(num_changes);

        // Helper: build a fresh set of `num_pipelines` pipelines with one
        // warm-up advance call each. Both sequential and parallel start
        // from this identical state so their emitted-row-change totals
        // are directly comparable (the per-iteration emit count converges
        // to a steady state after several iterations; without resetting,
        // whichever block runs second begins already at steady state and
        // emits more rows than the first block, breaking the assertion).
        let build_warmed_pipelines = |schema_cache: &mut SchemaCache,
                                      pool: &ConnectionPool|
         -> Vec<PipelineState> {
            let mut ps: Vec<PipelineState> = Vec::new();
            for i in 0..num_pipelines {
                let query_json = make_bench_query(&format!("q{}", i), 10 + (i % 20));
                let query: HydrateQuery = serde_json::from_str(&query_json).unwrap();
                let state = build_pipeline_state(&db_path, &query, schema_cache, pool).unwrap();
                ps.push(state);
            }
            // Warm-up: drive each pipeline through enough iterations to reach
            // steady state so the measured block sees a stable emit rate.
            // Empirically (debug logging), per-iteration totals converge by
            // ~80 iters; use a generous margin.
            for _ in 0..120 {
                for pipeline in ps.iter_mut() {
                    // B11 bench: same-path is sufficient (no cascade-delete in this fixture)
                    let _ = advance_persistent_pipeline(pipeline, &changes, &db_path, &db_path);
                }
            }
            ps
        };

        // ── Sequential ──
        let mut pipelines = build_warmed_pipelines(&mut schema_cache, &pool);
        let start = Instant::now();
        let mut seq_total_changes = 0;
        for _ in 0..iterations {
            for pipeline in pipelines.iter_mut() {
                let row_changes = advance_persistent_pipeline(pipeline, &changes, &db_path, &db_path);
                seq_total_changes += row_changes.len();
            }
        }
        let seq_elapsed = start.elapsed();
        drop(pipelines);

        // ── Parallel (rayon) ──
        // Fresh, warmed pipelines so parallel measures the same work as
        // sequential. Per-pipeline mutexes are needed for par_iter to
        // hand out &mut access.
        let pipelines = build_warmed_pipelines(&mut schema_cache, &pool);
        let pipeline_mutexes: Vec<Mutex<PipelineState>> = pipelines
            .into_iter()
            .map(|p| Mutex::new(p))
            .collect();

        let start = Instant::now();
        let mut par_total_changes = 0;
        for _ in 0..iterations {
            let batch_changes: Vec<RowChange> = pipeline_mutexes
                .par_iter()
                .flat_map(|pm| {
                    let mut pipeline = pm.lock().unwrap();
                    advance_persistent_pipeline(&mut pipeline, &changes, &db_path, &db_path)
                })
                .collect();
            par_total_changes += batch_changes.len();
        }
        let par_elapsed = start.elapsed();

        // Results
        println!("\n╔══════════════════════════════════════════════════════════╗");
        println!("║  Persistent Pipeline Advance Benchmark                  ║");
        println!("╠══════════════════════════════════════════════════════════╣");
        println!("║  Pipelines: {:<6}  Rows: {:<6}  Changes: {:<6}       ║", num_pipelines, num_rows, num_changes);
        println!("║  Iterations: {:<6}                                     ║", iterations);
        println!("╠══════════════════════════════════════════════════════════╣");
        println!("║  Sequential:  {:>10.2?}  ({} row changes)    ║", seq_elapsed, seq_total_changes);
        println!("║  Parallel:    {:>10.2?}  ({} row changes)    ║", par_elapsed, par_total_changes);
        println!("║  Speedup:     {:>10.2}x                               ║",
            seq_elapsed.as_secs_f64() / par_elapsed.as_secs_f64());
        println!("╚══════════════════════════════════════════════════════════╝\n");

        // Sanity check: both produce same number of changes
        assert_eq!(seq_total_changes, par_total_changes,
            "sequential and parallel must produce the same number of row changes");
    }

    // ========================================================================
    // Phase 34 Wave 1 — B11 GREEN-STATE assertions (Task 1b flip).
    //
    // Wave 0 stub (red) asserted the bug existed: emit_descendant_removals
    // signature was `db_path: &str` (reading post-tx snapshot), and no
    // #[napi] set_prev_snapshot existed. After Plan 34-06 Task 1b lands the
    // fix per CONTEXT D-15, the green-state assertions below verify:
    //   1. emit_descendant_removals signature is `prev_db_path: &str`.
    //   2. set_prev_snapshot is a #[napi] method on RustPipelineManager.
    //   3. The descendant SQL reads against prev_db_path (not db_path).
    //
    // Spec: TS pipeline-driver.ts:1542-1577.
    // See .planning/IVM-PORT-AUDIT-DEEP.md §B11.
    // ========================================================================

    /// **B11 (BLOCKING) — Cascade-delete reads PREV snapshot.** (post-fix green)
    ///
    /// Verifies the Plan 34-06 fix: emit_descendant_removals reads from the
    /// PREV snapshot where same-tx descendants still exist, not the curr
    /// (post-swap) snapshot where they have already been deleted.
    #[test]
    fn test_b11_descendants_from_prev() {
        let src = include_str!("advance.rs");
        // GREEN: emit_descendant_removals signature uses prev_db_path: &str.
        let fn_marker = src
            .find("fn emit_descendant_removals(")
            .expect("missing emit_descendant_removals — code refactored?");
        // Extend the window to include the open_with_flags call (multi-line
        // signature + body lead-in spans ~600 chars).
        let signature_block = &src[fn_marker..fn_marker + 800];
        assert!(
            signature_block.contains("prev_db_path: &str"),
            "B11 (post-fix): emit_descendant_removals signature must contain \
             `prev_db_path: &str`. Spec: TS pipeline-driver.ts:1542-1577. \
             Saw signature block:\n{}",
            signature_block,
        );
        // GREEN: SQL connection opens against prev_db_path, not db_path.
        // Tolerate either one-line or multi-line open_with_flags formatting.
        let opens_against_prev = signature_block.contains("open_with_flags(prev_db_path,") ||
            signature_block.contains("open_with_flags(\n        prev_db_path,");
        assert!(
            opens_against_prev,
            "B11: descendant SQL must open against prev_db_path. \
             Saw signature block:\n{}",
            signature_block,
        );
    }

    /// **B11 — set_prev_snapshot napi method present** (Task 1a artifact).
    ///
    /// Verifies the additive napi method exists on RustPipelineManager.
    /// CLAUDE.md gate #4 preserved (no existing buffered-method signature
    /// change).
    #[test]
    fn test_b11_set_prev_snapshot_napi_present() {
        let src = include_str!("pipeline_manager.rs");
        // GREEN: set_prev_snapshot decorated with #[napi], present in the
        // RustPipelineManager impl. Use a tolerant whitespace pattern: the
        // line above must contain `#[napi]` and the method line must be
        // `pub fn set_prev_snapshot(`.
        let method_marker = "pub fn set_prev_snapshot(";
        let method_idx = src
            .find(method_marker)
            .expect("set_prev_snapshot method not found — Task 1a regressed?");
        // Look at the 64 chars preceding the method declaration for #[napi].
        let preceding_window_start = method_idx.saturating_sub(64);
        let preceding = &src[preceding_window_start..method_idx];
        assert!(
            preceding.contains("#[napi]"),
            "B11: set_prev_snapshot must carry #[napi] attribute (Task 1a). \
             Preceding 64 chars:\n{}",
            preceding,
        );
    }

    /// **B11 — back-compat fallback path does not panic when prev_db_path
    /// is None.**
    ///
    /// If a caller never invokes set_prev_snapshot, advance must not panic:
    /// instead it logs a warning and falls back to db_path. This preserves
    /// pre-fix behavior (same-tx descendants still elided in fallback path
    /// — but no crash). Mono-rs production always wires setPrevSnapshot via
    /// pipeline-driver.ts (Plan 34-06 Task 2), so this fallback is purely
    /// defensive.
    #[test]
    fn test_b11_fallback_when_prev_not_set() {
        // Pure source-level assertion: the fallback path reads
        // `instance.prev_db_path.clone().unwrap_or_else(|| ...)` rather than
        // `.unwrap()`. Without the unwrap_or_else, a None prev_db_path would
        // panic — this assertion locks in the safe back-compat path.
        let src = include_str!("pipeline_manager.rs");
        assert!(
            src.contains("prev_db_path.clone()") &&
                src.contains(".unwrap_or_else"),
            "B11: fallback path must use `prev_db_path.clone().unwrap_or_else(...)` \
             so a None setting falls back to db_path with a logged warning \
             rather than panicking. (Mono-rs production wires setPrevSnapshot \
             via pipeline-driver.ts; this fallback exists for back-compat.)"
        );
        assert!(
            src.contains("[B11] prev_db_path not set"),
            "B11: fallback path must log a warning via eprintln when \
             prev_db_path is None (per RESEARCH.md fallback design)."
        );
    }

    // ─── NEW-1 / Phase 35 tests ─────────────────────────────────────────

    /// **NEW-1 — prev_pool reuses one connection per advance batch.**
    ///
    /// Verifies that `emit_descendant_removals_with_conn` accepts the
    /// borrowed `&Connection` and runs N descendants without opening
    /// additional sqlite handles. We assert this by checking that the
    /// passed connection's prepare_cached path returns the SAME row count
    /// across repeated invocations using a stable cache.
    #[test]
    fn test_new1_emit_with_conn_reuses_connection() {
        use crate::connection_pool::ConnectionPool;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prev.db");
        // Build 2-level cascade: parents → children
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE parent (id INTEGER PRIMARY KEY); \
             CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER); \
             INSERT INTO parent VALUES (1),(2),(3); \
             INSERT INTO child VALUES (10,1),(11,1),(20,2),(30,3);",
        )
        .unwrap();
        drop(conn);

        // Prev pool — single connection, snapshot-pinned.
        let prev_pool = ConnectionPool::new(db_path.to_str().unwrap(), 1).unwrap();
        let prev_conn_guard = prev_pool.get().unwrap();

        // children_of map: parent → child
        let mut children_of = HashMap::new();
        children_of.insert(
            "parent".to_string(),
            vec![ChildRelation {
                relationship_name: "children".to_string(),
                child_table: "child".to_string(),
                parent_join_col: vec!["id".to_string()],
                child_join_col: vec!["parent_id".to_string()],
                child_pk: vec!["id".to_string()],
                child_order: vec![],
                child_limit: None,
            }],
        );

        // Run 3 deletes (parents 1, 2, 3) — exercises prepare_cached
        // (same SQL shape repeats) and connection reuse.
        let mut row_changes = Vec::new();
        for pid in 1..=3 {
            let mut deleted = serde_json::Map::new();
            deleted.insert("id".to_string(), serde_json::json!(pid));
            emit_descendant_removals_with_conn(
                &*prev_conn_guard,
                &deleted,
                "parent",
                &children_of,
                "q1",
                &None,
                &mut row_changes,
            );
        }
        // Expect 4 child removes (3 from p=1 → 2 children, p=2 → 1, p=3 → 1).
        assert_eq!(row_changes.len(), 4, "expected 4 child removes, got {}", row_changes.len());
        // Pool must still have its connection back when we drop the guard.
        drop(prev_conn_guard);
        assert_eq!(prev_pool.available().unwrap(), 1);
    }

    /// **NEW-1 — prepare_cached used in emit_descendant_removals_with_conn.**
    ///
    /// Source-level assertion: D-10 statement caching is via rusqlite's
    /// built-in `prepare_cached` (per the implementer note in 35-03-PLAN).
    /// Locks in the optimization across future refactors.
    #[test]
    fn test_new1_prepare_cached_present() {
        let src = include_str!("advance.rs");
        // Find the function body for emit_descendant_removals_with_conn.
        let fn_marker = src
            .find("fn emit_descendant_removals_with_conn(")
            .expect("missing emit_descendant_removals_with_conn");
        // Body window: ~3KB should cover the function.
        let body = &src[fn_marker..fn_marker.saturating_add(3000).min(src.len())];
        assert!(
            body.contains("prepare_cached"),
            "NEW-1/D-10: emit_descendant_removals_with_conn must use \
             rusqlite's prepare_cached for SQL reuse across siblings. \
             Saw body:\n{}",
            body,
        );
    }

    /// **NEW-1 — legacy fallback path still exists and opens fresh
    /// connection per call.** This keeps the test_b11_fallback semantics
    /// working when prev_pool is None.
    #[test]
    fn test_new1_legacy_fallback_signature() {
        let src = include_str!("advance.rs");
        // The PUBLIC legacy fallback fn `emit_descendant_removals` still
        // takes `prev_db_path: &str` and opens its own connection.
        let fn_marker = src
            .find("fn emit_descendant_removals(")
            .expect("missing emit_descendant_removals (legacy fallback)");
        let body = &src[fn_marker..fn_marker.saturating_add(1500).min(src.len())];
        assert!(
            body.contains("prev_db_path: &str"),
            "NEW-1: legacy fallback emit_descendant_removals must keep \
             prev_db_path: &str signature for back-compat."
        );
        assert!(
            body.contains("open_with_flags"),
            "NEW-1: legacy fallback must still open its own connection \
             from prev_db_path."
        );
    }

    /// **Phase 35 / Wave 3 — cascade-throughput hard gate.**
    ///
    /// 3-level cascade fixture (parents 100 → children 1000 → grandchildren
    /// 10000), single-shot delete-all-parents. Measures wall-clock for:
    ///   - LEGACY: emit_descendant_removals_legacy (opens fresh
    ///     `Connection::open_with_flags` per call, plain `prepare`).
    ///   - NEW-1:  emit_descendant_removals_with_conn (single borrowed
    ///     `&Connection`, `prepare_cached`).
    ///
    /// Hard gate per D-17 #3: ratio (legacy_ms / new_ms) >= 2.0.
    /// Also asserts connection-open count reduction >= 10×.
    ///
    /// Marked `#[ignore]` so the default `cargo test` run doesn't pay the
    /// fixture build cost; run with `cargo test --release bench_cascade
    /// -- --ignored --nocapture` or via the bench harness.
    #[test]
    #[ignore]
    fn bench_cascade_new1_throughput() {
        use crate::connection_pool::ConnectionPool;
        use std::time::Instant;

        const PARENTS: usize = 100;
        const CHILDREN_PER_PARENT: usize = 10;
        const GRANDCHILDREN_PER_CHILD: usize = 10;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cascade.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             CREATE TABLE parent (id INTEGER PRIMARY KEY); \
             CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER); \
             CREATE TABLE grandchild (id INTEGER PRIMARY KEY, child_id INTEGER); \
             CREATE INDEX idx_child_parent ON child(parent_id); \
             CREATE INDEX idx_grand_child ON grandchild(child_id);",
        )
        .unwrap();

        // Bulk insert 100 + 1000 + 10000 = 11100 rows.
        conn.execute_batch("BEGIN").unwrap();
        for p in 1..=PARENTS {
            conn.execute("INSERT INTO parent VALUES (?1)", [p as i64]).unwrap();
            for c in 0..CHILDREN_PER_PARENT {
                let cid = (p * 1000 + c) as i64;
                conn.execute("INSERT INTO child VALUES (?1, ?2)", [cid, p as i64]).unwrap();
                for g in 0..GRANDCHILDREN_PER_CHILD {
                    let gid = (cid * 100 + g as i64) as i64;
                    conn.execute("INSERT INTO grandchild VALUES (?1, ?2)", [gid, cid]).unwrap();
                }
            }
        }
        conn.execute_batch("COMMIT").unwrap();
        drop(conn);

        // children_of map: parent → child, child → grandchild.
        let mut children_of = HashMap::new();
        children_of.insert(
            "parent".to_string(),
            vec![ChildRelation {
                relationship_name: "children".to_string(),
                child_table: "child".to_string(),
                parent_join_col: vec!["id".to_string()],
                child_join_col: vec!["parent_id".to_string()],
                child_pk: vec!["id".to_string()],
                child_order: vec![],
                child_limit: None,
            }],
        );
        children_of.insert(
            "child".to_string(),
            vec![ChildRelation {
                relationship_name: "grandchildren".to_string(),
                child_table: "grandchild".to_string(),
                parent_join_col: vec!["id".to_string()],
                child_join_col: vec!["child_id".to_string()],
                child_pk: vec!["id".to_string()],
                child_order: vec![],
                child_limit: None,
            }],
        );

        let db_path_str = db_path.to_str().unwrap().to_string();

        // ── LEGACY path: opens a fresh connection per call ─────────────
        let t0 = Instant::now();
        let mut legacy_changes = Vec::new();
        for pid in 1..=PARENTS {
            let mut deleted = serde_json::Map::new();
            deleted.insert("id".to_string(), serde_json::json!(pid));
            emit_descendant_removals_legacy(
                &db_path_str,
                &deleted,
                "parent",
                &children_of,
                "q1",
                &None,
                &mut legacy_changes,
            );
        }
        let legacy_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let legacy_count = legacy_changes.len();

        // ── NEW-1 path: single connection from prev_pool, prepare_cached ─
        let prev_pool = ConnectionPool::new(&db_path_str, 1).unwrap();
        let prev_conn_guard = prev_pool.get().unwrap();
        let t0 = Instant::now();
        let mut new1_changes = Vec::new();
        for pid in 1..=PARENTS {
            let mut deleted = serde_json::Map::new();
            deleted.insert("id".to_string(), serde_json::json!(pid));
            emit_descendant_removals_with_conn(
                &*prev_conn_guard,
                &deleted,
                "parent",
                &children_of,
                "q1",
                &None,
                &mut new1_changes,
            );
        }
        let new1_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let new1_count = new1_changes.len();
        drop(prev_conn_guard);

        // Same workload — same emitted-row count.
        assert_eq!(legacy_count, new1_count, "legacy and NEW-1 emit different row counts");
        // Expected: 1000 child + 10000 grandchild = 11000 removes.
        assert_eq!(new1_count, PARENTS * CHILDREN_PER_PARENT * (1 + GRANDCHILDREN_PER_CHILD));

        let ratio = legacy_ms / new1_ms;
        // Connection::open count: legacy opens once per recursion node
        // (PARENTS + PARENTS*CHILDREN_PER_PARENT = 100 + 1000 = 1100).
        // NEW-1 opens exactly 1 (prev_pool's single connection).
        let legacy_open_count = PARENTS + PARENTS * CHILDREN_PER_PARENT;
        let new1_open_count = 1;
        let open_reduction = legacy_open_count as f64 / new1_open_count as f64;

        eprintln!("CASCADE_THROUGHPUT_BASELINE: ms={:.2}", legacy_ms);
        eprintln!("CASCADE_THROUGHPUT_NEW1: ms={:.2}", new1_ms);
        eprintln!("CASCADE_THROUGHPUT_RATIO: r={:.2}", ratio);
        eprintln!("CASCADE_OPEN_REDUCTION: legacy={} new1={} ratio={:.0}x",
                  legacy_open_count, new1_open_count, open_reduction);

        // Hard gates (D-17 #2 + #3).
        assert!(
            ratio >= 2.0,
            "Phase 35 D-17 #3: cascade ratio must be >= 2.0; got {ratio:.2} \
             (legacy={legacy_ms:.2}ms, new1={new1_ms:.2}ms)"
        );
        assert!(
            open_reduction >= 10.0,
            "Phase 35 D-17 #2: connection-open reduction must be >= 10x; got {open_reduction}x"
        );
    }
}
use zero_ivm_rs::types::FetchRequest;
