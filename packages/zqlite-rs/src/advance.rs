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

fn emit_descendant_removals(
    db_path: &str,
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
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return,
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

        for row_result in rows_iter {
            if let Ok(child_row) = row_result {
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
                // Recurse for deeper levels
                emit_descendant_removals(
                    db_path, &child_row, &rel.child_table, children_of,
                    query_id, column_types, row_changes,
                );
            }
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

/// Persistent pipeline that keeps operator trees and HashMap state alive across
/// hydrate/advance calls. Eliminates per-call pipeline rebuild overhead (~44%)
/// and per-advance warmup/rewind cycles.
#[napi]
pub struct RustPipeline {
    db_path: Mutex<String>,
    pipelines: RwLock<Vec<Mutex<PipelineState>>>,
    shared_pool: ConnectionPool,
}

// SAFETY: RustPipeline fields are Send+Sync:
// - db_path: Mutex<String> is Send+Sync
// - pipelines: RwLock<Vec<Mutex<PipelineState>>> where PipelineState contains
//   Arc<RustTableSource> (Send+Sync) and Vec<Box<dyn Operator + Send>>.
//   All inner types are Send. The RwLock+Mutex combination provides safe
//   interior mutability. Rayon par_iter requires Send.
// - shared_pool: ConnectionPool is Clone+Send+Sync (Arc<Mutex<..>> internally)
unsafe impl Send for RustPipeline {}
unsafe impl Sync for RustPipeline {}

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

    let rel_to_table: HashMap<String, String> = collect_child_tables(&query.ast)
        .into_iter()
        .collect();

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

pub(crate) fn advance_persistent_pipeline(
    pipeline: &mut PipelineState,
    changes: &[Change],
    db_path: &str,
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
                            emit_descendant_removals(
                                db_path, &deleted_map, &change.table, &pipeline.children_of_map,
                                &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                            );
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
                            emit_descendant_removals(
                                db_path, &deleted_map, &change.table, &pipeline.children_of_map,
                                &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                            );
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

#[napi]
impl RustPipeline {
    #[napi(constructor)]
    pub fn new(db_path: String, queries_json: String) -> napi::Result<Self> {
        use crate::ast_to_config::{HydrateQuery, SchemaCache};

        let queries: Vec<HydrateQuery> = serde_json::from_str(&queries_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse queries JSON: {e}")))?;

        // Create a shared connection pool sized to Rayon thread count.
        // All pipelines share this pool — bounds concurrent SQLite WAL readers
        // to core count instead of pipeline count.
        let pool_size = rayon::current_num_threads().max(4);
        let shared_pool = ConnectionPool::new(&db_path, pool_size)
            .map_err(|e| napi::Error::from_reason(format!("Failed to create shared pool: {e}")))?;

        let mut schema_cache = SchemaCache::new(&db_path);
        let mut pipelines = Vec::with_capacity(queries.len());

        for query in &queries {
            let state = build_pipeline_state(&db_path, query, &mut schema_cache, &shared_pool)
                .map_err(|e| napi::Error::from_reason(format!(
                    "Failed to build pipeline '{}': {e}", query.query_id
                )))?;
            pipelines.push(Mutex::new(state));
        }

        Ok(Self {
            db_path: Mutex::new(db_path),
            pipelines: RwLock::new(pipelines),
            shared_pool,
        })
    }

    /// Create an empty pipeline set. Queries are added incrementally via add_query().
    #[napi(factory)]
    pub fn create_empty(db_path: String) -> napi::Result<Self> {
        let pool_size = rayon::current_num_threads().max(4);
        let shared_pool = ConnectionPool::new(&db_path, pool_size)
            .map_err(|e| napi::Error::from_reason(format!("Failed to create shared pool: {e}")))?;

        Ok(Self {
            db_path: Mutex::new(db_path),
            pipelines: RwLock::new(Vec::new()),
            shared_pool,
        })
    }

    /// Run initial hydration: fetch from all persistent operator trees.
    #[napi]
    pub fn hydrate(&self) -> napi::Result<Buffer> {
        let pipelines = self.pipelines.read()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;

        let all_row_changes: Vec<RowChange> = pipelines.par_iter().flat_map(|pipeline_mutex| {
            let mut pipeline = pipeline_mutex.lock().unwrap();
            let mut row_changes = Vec::new();

            if let Some(ref mut chain) = pipeline.chain {
                let nodes = chain.fetch(&FetchRequest::default());
                flatten_nodes_to_row_changes(
                    &mut row_changes,
                    &pipeline.query_id,
                    &pipeline.source_table,
                    &pipeline.primary_key,
                    &nodes,
                    &pipeline.all_primary_keys,
                    &pipeline.column_types,
                    &pipeline.rel_to_table,
                );
            } else {
                let req = crate::source::FetchRequest::default();
                if let Ok(nodes) = pipeline.source.fetch(0, &req) {
                    let ivm_nodes: Vec<zero_ivm_rs::types::Node> = nodes.into_iter().map(|n| {
                        zero_ivm_rs::types::Node {
                            row: n.row,
                            relationships: n.relationships.into_iter().map(|(k, v)| {
                                (k, v.into_iter().map(|cn| zero_ivm_rs::types::Node {
                                    row: cn.row,
                                    relationships: HashMap::new(),
                                }).collect())
                            }).collect(),
                        }
                    }).collect();
                    flatten_nodes_to_row_changes(
                        &mut row_changes,
                        &pipeline.query_id,
                        &pipeline.source_table,
                        &pipeline.primary_key,
                        &ivm_nodes,
                        &pipeline.all_primary_keys,
                        &pipeline.column_types,
                        &pipeline.rel_to_table,
                    );
                }
            }

            row_changes
        }).collect();

        let result = AdvanceResult {
            changes: all_row_changes,
            error: None,
            error_type: None, reset_signal: None,
        };
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Run initial hydration for a single query by its query_id.
    #[napi]
    pub fn hydrate_query(&self, query_id: String) -> napi::Result<Buffer> {
        let pipelines = self.pipelines.read()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;

        let pipeline_mutex = pipelines.iter()
            .find(|p| p.lock().unwrap().query_id == query_id)
            .ok_or_else(|| napi::Error::from_reason(format!("No pipeline found for query_id: {query_id}")))?;

        let mut pipeline = pipeline_mutex.lock().unwrap();
        let mut row_changes = Vec::new();

        if let Some(ref mut chain) = pipeline.chain {
            let nodes = chain.fetch(&FetchRequest::default());
            flatten_nodes_to_row_changes(
                &mut row_changes,
                &pipeline.query_id,
                &pipeline.source_table,
                &pipeline.primary_key,
                &nodes,
                &pipeline.all_primary_keys,
                &pipeline.column_types,
                &pipeline.rel_to_table,
            );
        } else {
            let req = crate::source::FetchRequest::default();
            if let Ok(nodes) = pipeline.source.fetch(0, &req) {
                let ivm_nodes: Vec<zero_ivm_rs::types::Node> = nodes.into_iter().map(|n| {
                    zero_ivm_rs::types::Node {
                        row: n.row,
                        relationships: n.relationships.into_iter().map(|(k, v)| {
                            (k, v.into_iter().map(|cn| zero_ivm_rs::types::Node {
                                row: cn.row,
                                relationships: HashMap::new(),
                            }).collect())
                        }).collect(),
                    }
                }).collect();
                flatten_nodes_to_row_changes(
                    &mut row_changes,
                    &pipeline.query_id,
                    &pipeline.source_table,
                    &pipeline.primary_key,
                    &ivm_nodes,
                    &pipeline.all_primary_keys,
                    &pipeline.column_types,
                    &pipeline.rel_to_table,
                );
            }
        }

        let result = AdvanceResult {
            changes: row_changes,
            error: None,
            error_type: None, reset_signal: None,
        };
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Push changes through persistent operator trees. No pipeline rebuild,
    /// no warmup/rewind needed — state is maintained across calls.
    #[napi]
    pub fn advance(&self, changes_json: String) -> napi::Result<Buffer> {
        use std::time::Instant;

        let t0 = Instant::now();
        let changes: Vec<Change> = serde_json::from_str(&changes_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;
        let t_parse = t0.elapsed();

        if changes.is_empty() {
            let result = AdvanceResult {
                changes: vec![],
                error: None,
                error_type: None, reset_signal: None,
            };
            return Ok(Buffer::from(encode_advance_result_buf(&result)));
        }

        let t1 = Instant::now();
        let pipelines = self.pipelines.read()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        let db_path = self.db_path.lock().unwrap().clone();
        let t_lock = t1.elapsed();

        let t2 = Instant::now();
        let all_row_changes: Vec<RowChange> = pipelines.iter().flat_map(|pipeline_mutex| {
            let mut pipeline = pipeline_mutex.lock().unwrap();
            advance_persistent_pipeline(&mut pipeline, &changes, &db_path)
        }).collect();
        let t_ivm = t2.elapsed();

        let t3 = Instant::now();
        let result = AdvanceResult {
            changes: all_row_changes,
            error: None,
            error_type: None, reset_signal: None,
        };
        let buf = encode_advance_result_buf(&result);
        let t_encode = t3.elapsed();

        Ok(Buffer::from(buf))
    }

    /// Re-open SQLite connections at a new path without rebuilding operator trees.
    #[napi]
    pub fn swap_snapshot(&self, new_db_path: String) -> napi::Result<()> {
        // Swap the shared pool once — all pipeline sources share it.
        self.shared_pool.swap_path(&new_db_path)
            .map_err(|e| napi::Error::from_reason(format!("Failed to swap shared pool: {e}")))?;

        // Reset per-pipeline state (overlay, epoch, connection metadata).
        let pipelines = self.pipelines.read()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        for pipeline_mutex in pipelines.iter() {
            let pipeline = pipeline_mutex.lock().unwrap();
            pipeline.source.reset_state();
        }
        drop(pipelines);

        *self.db_path.lock().unwrap() = new_db_path;
        Ok(())
    }

    /// Add a new query to the persistent pipeline set.
    #[napi]
    pub fn add_query(&self, query_json: String) -> napi::Result<()> {
        use crate::ast_to_config::{HydrateQuery, SchemaCache};

        let query: HydrateQuery = serde_json::from_str(&query_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse query JSON: {e}")))?;

        let db_path = self.db_path.lock().unwrap().clone();
        let mut schema_cache = SchemaCache::new(&db_path);
        let state = build_pipeline_state(&db_path, &query, &mut schema_cache, &self.shared_pool)
            .map_err(|e| napi::Error::from_reason(format!(
                "Failed to build pipeline '{}': {e}", query.query_id
            )))?;

        let mut pipelines = self.pipelines.write()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        pipelines.push(Mutex::new(state));
        Ok(())
    }

    /// Remove a query from the persistent pipeline set.
    #[napi]
    pub fn remove_query(&self, query_id: String) -> napi::Result<()> {
        let mut pipelines = self.pipelines.write()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        pipelines.retain(|p| p.lock().unwrap().query_id != query_id);
        Ok(())
    }

    /// Returns the number of active pipelines.
    #[napi]
    pub fn pipeline_count(&self) -> napi::Result<u32> {
        let pipelines = self.pipelines.read()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        Ok(pipelines.len() as u32)
    }
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
                    let _ = advance_persistent_pipeline(pipeline, &changes, &db_path);
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
                let row_changes = advance_persistent_pipeline(pipeline, &changes, &db_path);
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
                    advance_persistent_pipeline(&mut pipeline, &changes, &db_path)
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
}
use zero_ivm_rs::types::FetchRequest;
