use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use napi_derive::napi;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::diff::{self, Change, DiffError, Row, TableAndZqlSpec};

// ─── Pipeline Topology Types ────────────────────────────────────────────────

// Only Filter operators are actively processed — other types are accepted for
// forward-compatibility but not yet used in the fan-out path.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Operator {
    #[serde(rename = "filter")]
    Filter { predicate: serde_json::Value },
    #[serde(rename = "take")]
    Take {
        sort: Vec<(String, String)>,
        limit: Option<i64>,
    },
    #[serde(rename = "exists")]
    Exists {
        relationship: String,
        parent_field: Vec<String>,
        not_exists: bool,
    },
    #[serde(rename = "join")]
    Join {
        relationship: String,
        parent_field: Vec<String>,
        child_field: Vec<String>,
        child_ast: serde_json::Value,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct PipelineConfig {
    pub query_id: String,
    pub source_tables: Vec<String>,
    pub operators: Vec<Operator>,
    #[serde(default)]
    pub primary_key: Vec<String>,
    #[serde(default)]
    pub related: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub order_by: Option<Vec<(String, String)>>,
}

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
}

// ─── Predicate AST (reimplemented from zero-ivm-rs, cdylib can't cross-link) ─

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

impl Value {
    fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) => Value::String(s.clone()),
            _ => Value::Null,
        }
    }
}

#[derive(Clone, Debug)]
enum Predicate {
    Eq(String, Value),
    Neq(String, Value),
    Gt(String, Value),
    Gte(String, Value),
    Lt(String, Value),
    Lte(String, Value),
    In(String, Vec<Value>),
    Like(String, String),
    IsNull(String),
    IsNotNull(String),
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
}

impl Predicate {
    fn from_json(v: &serde_json::Value) -> Option<Self> {
        let obj = v.as_object()?;

        // Check for AST where-condition format (type: "simple" | "and" | "or")
        if let Some(typ) = obj.get("type").and_then(|t| t.as_str()) {
            return Self::from_ast_json(obj, typ);
        }

        let op = obj.get("op")?.as_str()?;

        match op {
            "eq" | "neq" | "gt" | "gte" | "lt" | "lte" => {
                let field = obj.get("field")?.as_str()?.to_string();
                let value = Value::from_json(obj.get("value")?);
                Some(match op {
                    "eq" => Predicate::Eq(field, value),
                    "neq" => Predicate::Neq(field, value),
                    "gt" => Predicate::Gt(field, value),
                    "gte" => Predicate::Gte(field, value),
                    "lt" => Predicate::Lt(field, value),
                    "lte" => Predicate::Lte(field, value),
                    _ => unreachable!(),
                })
            }
            "in" => {
                let field = obj.get("field")?.as_str()?.to_string();
                let values: Vec<Value> = obj
                    .get("values")?
                    .as_array()?
                    .iter()
                    .map(Value::from_json)
                    .collect();
                Some(Predicate::In(field, values))
            }
            "like" => {
                let field = obj.get("field")?.as_str()?.to_string();
                let pattern = obj.get("value")?.as_str()?.to_string();
                Some(Predicate::Like(field, pattern))
            }
            "isNull" => Some(Predicate::IsNull(obj.get("field")?.as_str()?.to_string())),
            "isNotNull" => Some(Predicate::IsNotNull(obj.get("field")?.as_str()?.to_string())),
            "and" => {
                let conditions: Vec<Predicate> = obj
                    .get("conditions")?
                    .as_array()?
                    .iter()
                    .filter_map(Predicate::from_json)
                    .collect();
                Some(Predicate::And(conditions))
            }
            "or" => {
                let conditions: Vec<Predicate> = obj
                    .get("conditions")?
                    .as_array()?
                    .iter()
                    .filter_map(Predicate::from_json)
                    .collect();
                Some(Predicate::Or(conditions))
            }
            "not" => {
                let inner = Predicate::from_json(obj.get("condition")?)?;
                Some(Predicate::Not(Box::new(inner)))
            }
            _ => None,
        }
    }

    /// Parse the TS AST where-condition format.
    fn from_ast_json(obj: &serde_json::Map<String, serde_json::Value>, typ: &str) -> Option<Self> {
        match typ {
            "simple" => {
                let ast_op = obj.get("op")?.as_str()?;
                let left = obj.get("left")?.as_object()?;
                let right = obj.get("right")?.as_object()?;
                let field = left.get("name")?.as_str()?.to_string();

                if ast_op == "IS" {
                    let val = right.get("value");
                    if val.is_none() || val == Some(&serde_json::Value::Null) {
                        return Some(Predicate::IsNull(field));
                    }
                    return Some(Predicate::IsNotNull(field));
                }
                if ast_op == "IS NOT" {
                    return Some(Predicate::IsNotNull(field));
                }
                if ast_op == "IN" {
                    if let Some(arr) = right.get("value").and_then(|v| v.as_array()) {
                        let values: Vec<Value> = arr.iter().map(Value::from_json).collect();
                        return Some(Predicate::In(field, values));
                    }
                    return None;
                }
                if ast_op == "LIKE" {
                    let pattern = right.get("value")?.as_str()?.to_string();
                    return Some(Predicate::Like(field, pattern));
                }

                let value = Value::from_json(right.get("value")?);
                match ast_op {
                    "=" => Some(Predicate::Eq(field, value)),
                    "!=" => Some(Predicate::Neq(field, value)),
                    ">" => Some(Predicate::Gt(field, value)),
                    ">=" => Some(Predicate::Gte(field, value)),
                    "<" => Some(Predicate::Lt(field, value)),
                    "<=" => Some(Predicate::Lte(field, value)),
                    _ => None,
                }
            }
            "and" => {
                let conditions: Vec<Predicate> = obj
                    .get("conditions")?
                    .as_array()?
                    .iter()
                    .filter_map(Predicate::from_json)
                    .collect();
                Some(Predicate::And(conditions))
            }
            "or" => {
                let conditions: Vec<Predicate> = obj
                    .get("conditions")?
                    .as_array()?
                    .iter()
                    .filter_map(Predicate::from_json)
                    .collect();
                Some(Predicate::Or(conditions))
            }
            _ => None,
        }
    }
}

// ─── Predicate Evaluation ───────────────────────────────────────────────────

fn evaluate_predicate(predicate: &Predicate, row: &Row) -> bool {
    match predicate {
        Predicate::Eq(field, value) => {
            row.get(field).map_or(false, |v| &Value::from_json(v) == value)
        }
        Predicate::Neq(field, value) => {
            row.get(field).map_or(true, |v| &Value::from_json(v) != value)
        }
        Predicate::Gt(field, value) => row.get(field).map_or(false, |v| {
            compare_values(&Value::from_json(v), value) == std::cmp::Ordering::Greater
        }),
        Predicate::Gte(field, value) => row.get(field).map_or(false, |v| {
            compare_values(&Value::from_json(v), value) != std::cmp::Ordering::Less
        }),
        Predicate::Lt(field, value) => row.get(field).map_or(false, |v| {
            compare_values(&Value::from_json(v), value) == std::cmp::Ordering::Less
        }),
        Predicate::Lte(field, value) => row.get(field).map_or(false, |v| {
            compare_values(&Value::from_json(v), value) != std::cmp::Ordering::Greater
        }),
        Predicate::In(field, values) => {
            row.get(field).map_or(false, |v| values.contains(&Value::from_json(v)))
        }
        Predicate::Like(field, pattern) => row.get(field).map_or(false, |v| match v {
            serde_json::Value::String(s) => like_match(s, pattern),
            _ => false,
        }),
        Predicate::IsNull(field) => {
            row.get(field).map_or(true, |v| v.is_null())
        }
        Predicate::IsNotNull(field) => {
            row.get(field).map_or(false, |v| !v.is_null())
        }
        Predicate::And(conditions) => conditions.iter().all(|c| evaluate_predicate(c, row)),
        Predicate::Or(conditions) => conditions.iter().any(|c| evaluate_predicate(c, row)),
        Predicate::Not(condition) => !evaluate_predicate(condition, row),
    }
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => Ordering::Equal,
    }
}

fn like_match(text: &str, pattern: &str) -> bool {
    let text_chars: Vec<char> = text.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    like_match_impl(&text_chars, &pattern_chars, 0, 0)
}

fn like_match_impl(text: &[char], pattern: &[char], ti: usize, pi: usize) -> bool {
    if pi == pattern.len() {
        return ti == text.len();
    }
    match pattern[pi] {
        '%' => {
            for i in ti..=text.len() {
                if like_match_impl(text, pattern, i, pi + 1) {
                    return true;
                }
            }
            false
        }
        '_' => {
            if ti < text.len() {
                like_match_impl(text, pattern, ti + 1, pi + 1)
            } else {
                false
            }
        }
        c => {
            if ti < text.len() && text[ti].to_ascii_lowercase() == c.to_ascii_lowercase() {
                like_match_impl(text, pattern, ti + 1, pi + 1)
            } else {
                false
            }
        }
    }
}

// ─── Pipeline Processing ────────────────────────────────────────────────────

fn process_change_for_pipeline(pipeline: &PipelineConfig, change: &Change) -> Vec<RowChange> {
    // Check if change is relevant to this pipeline
    if !pipeline.source_tables.contains(&change.table) {
        return Vec::new();
    }

    let mut results = Vec::new();

    // Determine change types from prev_values/next_value
    if let Some(ref next_value) = change.next_value {
        if change.prev_values.is_empty() {
            // Add: new row, no previous
            if passes_filter(&pipeline.operators, next_value) {
                results.push(RowChange {
                    query_id: pipeline.query_id.clone(),
                    table: change.table.clone(),
                    row_key: change.row_key.clone(),
                    row: Some(next_value.clone()),
                    change_type: "add".to_string(),
                });
            }
        } else {
            // Edit: row updated. Match prev row by primary key equality.
            let matched_prev = find_prev_by_pk(&pipeline.primary_key, &change.prev_values, next_value);

            for prev in &change.prev_values {
                let is_matched = matched_prev.map_or(false, |m| std::ptr::eq(m, prev));
                if is_matched {
                    // Same row (PK match) — this is an edit
                    if passes_filter(&pipeline.operators, next_value) {
                        results.push(RowChange {
                            query_id: pipeline.query_id.clone(),
                            table: change.table.clone(),
                            row_key: change.row_key.clone(),
                            row: Some(next_value.clone()),
                            change_type: "edit".to_string(),
                        });
                    } else if passes_filter(&pipeline.operators, prev) {
                        // Was visible, now filtered out -> remove
                        results.push(RowChange {
                            query_id: pipeline.query_id.clone(),
                            table: change.table.clone(),
                            row_key: change.row_key.clone(),
                            row: None,
                            change_type: "remove".to_string(),
                        });
                    }
                } else {
                    // Non-matched prev rows are unique key conflicts — remove them
                    if passes_filter(&pipeline.operators, prev) {
                        results.push(RowChange {
                            query_id: pipeline.query_id.clone(),
                            table: change.table.clone(),
                            row_key: change.row_key.clone(),
                            row: None,
                            change_type: "remove".to_string(),
                        });
                    }
                }
            }

            // If no PK match found, treat next_value as an add
            if matched_prev.is_none() {
                if passes_filter(&pipeline.operators, next_value) {
                    results.push(RowChange {
                        query_id: pipeline.query_id.clone(),
                        table: change.table.clone(),
                        row_key: change.row_key.clone(),
                        row: Some(next_value.clone()),
                        change_type: "add".to_string(),
                    });
                }
            }
        }
    } else {
        // Delete: row removed
        for prev in &change.prev_values {
            if passes_filter(&pipeline.operators, prev) {
                results.push(RowChange {
                    query_id: pipeline.query_id.clone(),
                    table: change.table.clone(),
                    row_key: change.row_key.clone(),
                    row: None,
                    change_type: "remove".to_string(),
                });
            }
        }
    }

    results
}

fn passes_filter(operators: &[Operator], row: &Row) -> bool {
    for op in operators {
        match op {
            Operator::Filter { predicate } => {
                if let Some(pred) = Predicate::from_json(predicate) {
                    if !evaluate_predicate(&pred, row) {
                        return false;
                    }
                }
            }
            // Take, Exists, and Join are accepted for forward-compatibility
            // but not yet evaluated in the fan-out path.
            Operator::Take { .. } | Operator::Exists { .. } | Operator::Join { .. } => {}
        }
    }
    true
}

/// Find the prev_value row whose primary key columns match next_value.
/// Falls back to first prev_value if primary_key is empty (backwards compat).
fn find_prev_by_pk<'a>(primary_key: &[String], prev_values: &'a [Row], next_value: &Row) -> Option<&'a Row> {
    if primary_key.is_empty() {
        return prev_values.first();
    }
    prev_values.iter().find(|prev| {
        primary_key.iter().all(|col| {
            let prev_val = prev.get(col);
            let next_val = next_value.get(col);
            match (prev_val, next_val) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                _ => false,
            }
        })
    })
}

fn process_pipeline(pipeline: &PipelineConfig, changes: &[Change]) -> Vec<RowChange> {
    changes
        .iter()
        .flat_map(|change| process_change_for_pipeline(pipeline, change))
        .collect()
}

// ─── napi Entry Point ───────────────────────────────────────────────────────

#[napi]
pub fn rust_advance(
    db_path: String,
    prev_version: String,
    curr_version: String,
    syncable_tables_json: String,
    all_table_names_json: String,
    permissions_table: String,
    pipeline_configs_json: String,
) -> napi::Result<String> {
    // Deserialize inputs
    let syncable_tables: HashMap<String, TableAndZqlSpec> =
        serde_json::from_str(&syncable_tables_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse syncable_tables: {}", e)))?;

    let all_table_names: HashSet<String> = serde_json::from_str(&all_table_names_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse all_table_names: {}", e)))?;

    let pipelines: Vec<PipelineConfig> = serde_json::from_str(&pipeline_configs_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse pipeline_configs: {}", e)))?;

    // Open two read-only connections
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;

    let prev_conn = rusqlite::Connection::open_with_flags(&db_path, flags)
        .map_err(|e| napi::Error::from_reason(format!("Failed to open prev conn: {}", e)))?;
    let curr_conn = rusqlite::Connection::open_with_flags(&db_path, flags)
        .map_err(|e| napi::Error::from_reason(format!("Failed to open curr conn: {}", e)))?;

    // Begin deferred transactions for snapshot isolation
    prev_conn
        .execute_batch("BEGIN DEFERRED")
        .map_err(|e| napi::Error::from_reason(format!("Failed to begin prev: {}", e)))?;
    curr_conn
        .execute_batch("BEGIN DEFERRED")
        .map_err(|e| napi::Error::from_reason(format!("Failed to begin curr: {}", e)))?;

    // Version check: verify curr_conn sees the expected version
    let actual_version: Result<String, _> = curr_conn.query_row(
        "SELECT version FROM \"_zero.replicationState\" LIMIT 1",
        [],
        |row| row.get(0),
    );

    if let Ok(ref actual) = actual_version {
        if actual != &curr_version {
            let _ = prev_conn.execute_batch("ROLLBACK");
            let _ = curr_conn.execute_batch("ROLLBACK");
            let result = AdvanceResult {
                changes: Vec::new(),
                error: Some(format!(
                    "version mismatch: expected {}, got {}",
                    curr_version, actual
                )),
                error_type: Some("version_mismatch".to_string()),
            };
            return Ok(serde_json::to_string(&result).unwrap());
        }
    }
    // If table doesn't exist, skip version check (test environments)

    // Read diff
    let diff_result = diff::read_diff(
        &prev_conn,
        &curr_conn,
        &prev_version,
        &syncable_tables,
        &all_table_names,
        &permissions_table,
    );

    let changes = match diff_result {
        Ok(changes) => changes,
        Err(DiffError::Reset(msg)) => {
            let _ = prev_conn.execute_batch("ROLLBACK");
            let _ = curr_conn.execute_batch("ROLLBACK");
            let result = AdvanceResult {
                changes: Vec::new(),
                error: Some(msg),
                error_type: Some("reset".to_string()),
            };
            return Ok(serde_json::to_string(&result).unwrap());
        }
        Err(DiffError::Truncate(msg)) => {
            let _ = prev_conn.execute_batch("ROLLBACK");
            let _ = curr_conn.execute_batch("ROLLBACK");
            let result = AdvanceResult {
                changes: Vec::new(),
                error: Some(msg),
                error_type: Some("truncate".to_string()),
            };
            return Ok(serde_json::to_string(&result).unwrap());
        }
        Err(DiffError::Unknown(msg)) => {
            let _ = prev_conn.execute_batch("ROLLBACK");
            let _ = curr_conn.execute_batch("ROLLBACK");
            let result = AdvanceResult {
                changes: Vec::new(),
                error: Some(msg),
                error_type: Some("unknown".to_string()),
            };
            return Ok(serde_json::to_string(&result).unwrap());
        }
    };

    // Rayon fan-out over pipelines
    let changes_arc = Arc::new(changes);
    let row_changes: Vec<RowChange> = pipelines
        .par_iter()
        .flat_map(|pipeline| process_pipeline(pipeline, &changes_arc))
        .collect();

    // Cleanup
    let _ = prev_conn.execute_batch("ROLLBACK");
    let _ = curr_conn.execute_batch("ROLLBACK");

    let result = AdvanceResult {
        changes: row_changes,
        error: None,
        error_type: None,
    };

    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("Failed to serialize result: {}", e)))
}

// ─── Fan-out Only Entry Point ────────────────────────────────────────────────

/// Accepts pre-computed changes (from TS diff) and only does Rayon fan-out
/// over pipelines. This avoids the prev-snapshot bug where two fresh SQLite
/// connections see the same data for edits.
#[napi]
pub fn rust_fan_out(
    changes_json: String,
    pipeline_configs_json: String,
) -> napi::Result<String> {
    let changes: Vec<Change> = serde_json::from_str(&changes_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {}", e)))?;

    let pipelines: Vec<PipelineConfig> = serde_json::from_str(&pipeline_configs_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse pipeline_configs: {}", e)))?;

    let changes_arc = Arc::new(changes);
    let row_changes: Vec<RowChange> = pipelines
        .par_iter()
        .flat_map(|pipeline| process_pipeline(pipeline, &changes_arc))
        .collect();

    let result = AdvanceResult {
        changes: row_changes,
        error: None,
        error_type: None,
    };

    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("Failed to serialize result: {}", e)))
}

// ─── Full Advance (Phase 24) ────────────────────────────────────────────────

use crate::hydrate::build_push_operator_chain;
use crate::source::SourceChange;
use crate::table_source::RustTableSource;
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
}

#[derive(Debug, Serialize)]
pub struct FullAdvanceResult {
    pub changes: Vec<RowChange>,
    pub error: Option<String>,
    pub error_type: Option<String>,
    pub reset: bool,
}

/// Convert a diff::Row (HashMap) to a source::Row (serde_json::Map).
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
        if primary_key.len() == 1 {
            row.get(&primary_key[0])
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Array(
                primary_key
                    .iter()
                    .map(|k| row.get(k).cloned().unwrap_or(serde_json::Value::Null))
                    .collect(),
            )
        }
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

pub fn advance_pipelines_full(
    db_path: &str,
    changes: &[Change],
    pipelines: &[FullPipelineConfig],
) -> FullAdvanceResult {
    // Process pipelines in parallel — each gets its own RustTableSource
    let changes_arc = Arc::new(changes.to_vec());

    let all_row_changes: Vec<Vec<RowChange>> = pipelines
        .par_iter()
        .map(|pipeline| {
            process_full_pipeline(db_path, pipeline, &changes_arc)
        })
        .collect();

    let row_changes: Vec<RowChange> = all_row_changes.into_iter().flatten().collect();

    FullAdvanceResult {
        changes: row_changes,
        error: None,
        error_type: None,
        reset: false,
    }
}

fn process_full_pipeline(
    db_path: &str,
    pipeline: &FullPipelineConfig,
    changes: &[Change],
) -> Vec<RowChange> {
    // Extract column info from the Source config
    let (table_name, columns, pk, sort) = match &pipeline.operator_config[0] {
        OperatorConfig::Source {
            table_name,
            columns,
            primary_key,
            sort,
        } => (table_name.clone(), columns.clone(), primary_key.clone(), sort.clone()),
        _ => return vec![],
    };

    // Create RustTableSource for this pipeline's root table
    let mut column_types = HashMap::new();
    for c in &columns {
        column_types.insert(c.clone(), crate::query_builder::ColumnType::String);
    }
    let mut source = match RustTableSource::new(
        db_path, 2, table_name.clone(), columns, column_types, pk.clone(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("advance_full: failed to create source for {table_name}: {e}");
            return vec![];
        }
    };

    // Connect with sort + split_edit_keys
    let split_keys: Option<HashSet<String>> = if pipeline.split_edit_keys.is_empty() {
        None
    } else {
        Some(pipeline.split_edit_keys.iter().cloned().collect())
    };
    source.connect(Some(sort), None, split_keys);

    let source_arc = Arc::new(source);

    // Build push operator chain (everything after Source)
    let mut op_chain = match build_push_operator_chain(source_arc.clone(), &pipeline.operator_config) {
        Ok(Some(chain)) => chain,
        Ok(None) => {
            // No operators after Source — just convert changes directly
            return changes_to_row_changes_direct(changes, pipeline, &table_name, &pk);
        }
        Err(e) => {
            eprintln!("advance_full: failed to build push chain for {}: {e}", pipeline.query_id);
            return vec![];
        }
    };

    // Filter changes for this pipeline's table and push through
    let mut row_changes = Vec::new();
    for change in changes.iter() {
        if change.table != pipeline.source_table {
            continue;
        }

        let source_changes = diff_change_to_source_changes(change, &pipeline.primary_key);
        for sc in source_changes {
            let ivm_change = source_change_to_ivm_change(&sc);
            let output_changes = op_chain.push(ivm_change);
            for oc in &output_changes {
                row_changes.extend(ivm_change_to_row_changes(
                    oc,
                    &pipeline.query_id,
                    &change.table,
                    &pipeline.primary_key,
                ));
            }
        }
    }

    row_changes
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
#[napi]
pub fn rust_advance_full(
    db_path: String,
    changes_json: String,
    pipeline_configs_json: String,
) -> napi::Result<String> {
    let changes: Vec<Change> = serde_json::from_str(&changes_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;

    let pipelines: Vec<FullPipelineConfig> = serde_json::from_str(&pipeline_configs_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse pipeline_configs: {e}")))?;

    let result = advance_pipelines_full(&db_path, &changes, &pipelines);

    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("Failed to serialize result: {e}")))
}

// ─── Binary Serialization (Phase 25) ────────────────────────────────────────

use napi::bindgen_prelude::Buffer;

/// Encode a serde_json::Value into the binary buffer.
/// Tags: 0=null, 1=i64(8B LE), 2=f64(8B LE), 3=text(u32 len + bytes),
///       4=blob(u32 len + bytes), 5=bool(u8), 6=json fallback(u32 len + JSON bytes)
fn encode_json_value(buf: &mut Vec<u8>, val: &serde_json::Value) {
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

fn encode_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u16).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

pub fn encode_advance_result_buf(result: &AdvanceResult) -> Vec<u8> {
    let mut buf = Vec::with_capacity(result.changes.len() * 128);

    buf.extend_from_slice(&(result.changes.len() as u32).to_le_bytes());

    let has_error = result.error.is_some();
    buf.push(if has_error { 1 } else { 0 });

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

    buf
}

#[napi]
pub fn rust_fan_out_buf(
    changes_json: String,
    pipeline_configs_json: String,
) -> napi::Result<Buffer> {
    let changes: Vec<Change> = serde_json::from_str(&changes_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;
    let pipelines: Vec<PipelineConfig> = serde_json::from_str(&pipeline_configs_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse pipeline configs: {e}")))?;

    let changes_arc = Arc::new(changes);
    let row_changes: Vec<RowChange> = pipelines
        .par_iter()
        .flat_map(|pipeline| process_pipeline(pipeline, &changes_arc))
        .collect();

    let result = AdvanceResult {
        changes: row_changes,
        error: None,
        error_type: None,
    };
    Ok(Buffer::from(encode_advance_result_buf(&result)))
}

#[napi]
pub fn rust_advance_full_buf(
    db_path: String,
    changes_json: String,
    pipeline_configs_json: String,
) -> napi::Result<Buffer> {
    let changes: Vec<Change> = serde_json::from_str(&changes_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;
    let pipelines: Vec<FullPipelineConfig> = serde_json::from_str(&pipeline_configs_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse pipeline_configs: {e}")))?;

    let full_result = advance_pipelines_full(&db_path, &changes, &pipelines);
    let result = AdvanceResult {
        changes: full_result.changes,
        error: full_result.error,
        error_type: full_result.error_type,
    };
    Ok(Buffer::from(encode_advance_result_buf(&result)))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_config_deserialization() {
        let json = r#"{
            "query_id": "q1",
            "source_tables": ["users"],
            "operators": [
                {"type": "filter", "predicate": {"op": "eq", "field": "active", "value": true}}
            ],
            "primary_key": ["id"]
        }"#;
        let config: PipelineConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.query_id, "q1");
        assert_eq!(config.operators.len(), 1);
        assert_eq!(config.primary_key, vec!["id".to_string()]);
    }

    #[test]
    fn test_pipeline_config_deserialization_no_pk() {
        let json = r#"{
            "query_id": "q1",
            "source_tables": ["users"],
            "operators": []
        }"#;
        let config: PipelineConfig = serde_json::from_str(json).unwrap();
        assert!(config.primary_key.is_empty());
    }

    #[test]
    fn test_edit_matches_by_primary_key() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![],
            primary_key: vec!["id".to_string()],
        };

        let mut prev1 = Row::new();
        prev1.insert("id".to_string(), serde_json::json!("u2"));
        prev1.insert("name".to_string(), serde_json::json!("Other"));

        let mut prev2 = Row::new();
        prev2.insert("id".to_string(), serde_json::json!("u1"));
        prev2.insert("name".to_string(), serde_json::json!("Alice"));

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("name".to_string(), serde_json::json!("Bob"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev1, prev2],
            next_value: Some(next),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        // prev1 (u2) doesn't match PK -> remove
        // prev2 (u1) matches PK -> edit
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].change_type, "remove"); // u2 conflict
        assert_eq!(results[1].change_type, "edit");   // u1 matched
    }

    #[test]
    fn test_edit_no_pk_match_becomes_add_remove() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![],
            primary_key: vec!["id".to_string()],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u2"));
        prev.insert("name".to_string(), serde_json::json!("Other"));

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("name".to_string(), serde_json::json!("Alice"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: Some(next),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        // No PK match: prev u2 -> remove, next u1 -> add
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].change_type, "remove");
        assert_eq!(results[1].change_type, "add");
    }

    #[test]
    fn test_evaluate_predicate_eq() {
        let mut row = Row::new();
        row.insert("name".to_string(), serde_json::json!("Alice"));
        row.insert("age".to_string(), serde_json::json!(30));

        let pred = Predicate::Eq("name".to_string(), Value::String("Alice".to_string()));
        assert!(evaluate_predicate(&pred, &row));

        let pred = Predicate::Eq("name".to_string(), Value::String("Bob".to_string()));
        assert!(!evaluate_predicate(&pred, &row));

        let pred = Predicate::Gt("age".to_string(), Value::Number(25.0));
        assert!(evaluate_predicate(&pred, &row));

        let pred = Predicate::Lt("age".to_string(), Value::Number(25.0));
        assert!(!evaluate_predicate(&pred, &row));
    }

    #[test]
    fn test_evaluate_predicate_and_or() {
        let mut row = Row::new();
        row.insert("name".to_string(), serde_json::json!("Alice"));
        row.insert("active".to_string(), serde_json::json!(true));

        let pred = Predicate::And(vec![
            Predicate::Eq("name".to_string(), Value::String("Alice".to_string())),
            Predicate::Eq("active".to_string(), Value::Bool(true)),
        ]);
        assert!(evaluate_predicate(&pred, &row));

        let pred = Predicate::Or(vec![
            Predicate::Eq("name".to_string(), Value::String("Bob".to_string())),
            Predicate::Eq("active".to_string(), Value::Bool(true)),
        ]);
        assert!(evaluate_predicate(&pred, &row));

        let pred = Predicate::Not(Box::new(Predicate::Eq(
            "name".to_string(),
            Value::String("Bob".to_string()),
        )));
        assert!(evaluate_predicate(&pred, &row));
    }

    #[test]
    fn test_process_change_add() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
        };

        let mut next_value = Row::new();
        next_value.insert("id".to_string(), serde_json::json!("u1"));
        next_value.insert("active".to_string(), serde_json::json!(true));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(next_value),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "add");
    }

    #[test]
    fn test_process_change_remove() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u1"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: None,
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "remove");
    }

    #[test]
    fn test_process_change_edit() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u1"));
        prev.insert("name".to_string(), serde_json::json!("Alice"));

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("name".to_string(), serde_json::json!("Bob"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: Some(next),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "edit");
    }

    #[test]
    fn test_process_change_filtered_out() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
        };

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("active".to_string(), serde_json::json!(false));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(next),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_process_pipeline_multiple_changes() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };

        let mut row1 = Row::new();
        row1.insert("id".to_string(), serde_json::json!("u1"));
        let mut row2 = Row::new();
        row2.insert("id".to_string(), serde_json::json!("u2"));

        let changes = vec![
            Change {
                table: "users".to_string(),
                prev_values: vec![],
                next_value: Some(row1),
                row_key: serde_json::json!("u1"),
            },
            Change {
                table: "users".to_string(),
                prev_values: vec![],
                next_value: Some(row2),
                row_key: serde_json::json!("u2"),
            },
        ];

        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_process_change_skip_unrelated_table() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["orders".to_string()],
            primary_key: vec![],
            operators: vec![],
        };

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(next),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 0);
    }

    // ─── Edge Case Tests: Diff Size Extremes ────────────────────────────────

    #[test]
    fn test_empty_diff() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let changes: Vec<Change> = vec![];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_single_row_diff() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "add");
    }

    #[test]
    fn test_large_diff_1000_rows() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let changes: Vec<Change> = (0..1000)
            .map(|i| {
                let mut row = Row::new();
                row.insert("id".to_string(), serde_json::json!(format!("u{}", i)));
                Change {
                    table: "users".to_string(),
                    prev_values: vec![],
                    next_value: Some(row),
                    row_key: serde_json::json!(format!("u{}", i)),
                }
            })
            .collect();
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1000);
    }

    #[test]
    fn test_all_noop_diff() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let changes: Vec<Change> = (0..5)
            .map(|i| {
                let mut row = Row::new();
                row.insert("id".to_string(), serde_json::json!(format!("u{}", i)));
                row.insert("name".to_string(), serde_json::json!("same"));
                let prev = row.clone();
                Change {
                    table: "users".to_string(),
                    prev_values: vec![prev],
                    next_value: Some(row),
                    row_key: serde_json::json!(format!("u{}", i)),
                }
            })
            .collect();
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 5);
        for r in &results {
            assert_eq!(r.change_type, "edit");
        }
    }

    #[test]
    fn test_large_diff_filtered() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
        };
        let changes: Vec<Change> = (0..1000)
            .map(|i| {
                let mut row = Row::new();
                row.insert("id".to_string(), serde_json::json!(format!("u{}", i)));
                row.insert("active".to_string(), serde_json::json!(i % 2 == 0));
                Change {
                    table: "users".to_string(),
                    prev_values: vec![],
                    next_value: Some(row),
                    row_key: serde_json::json!(format!("u{}", i)),
                }
            })
            .collect();
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 500);
    }

    // ─── Edge Case Tests: Data Types ────────────────────────────────────────

    #[test]
    fn test_null_value_in_row() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("name".to_string(), serde_json::Value::Null);
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "add");
    }

    #[test]
    fn test_empty_string_value() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("name".to_string(), serde_json::json!(""));
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "add");
    }

    #[test]
    fn test_unicode_values() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("name".to_string(), serde_json::json!("\u{1F600}\u{4E16}\u{754C}"));
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "add");
        let name = results[0].row.as_ref().unwrap().get("name").unwrap();
        assert_eq!(name.as_str().unwrap(), "\u{1F600}\u{4E16}\u{754C}");
    }

    #[test]
    fn test_very_long_string() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let long_str = "a".repeat(100_000);
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("bio".to_string(), serde_json::json!(long_str));
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1);
        let bio = results[0].row.as_ref().unwrap().get("bio").unwrap();
        assert_eq!(bio.as_str().unwrap().len(), 100_000);
    }

    #[test]
    fn test_integer_value() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("age".to_string(), serde_json::json!(42));
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1);
        let age = results[0].row.as_ref().unwrap().get("age").unwrap();
        assert_eq!(age.as_i64().unwrap(), 42);
    }

    #[test]
    fn test_real_value() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("score".to_string(), serde_json::json!(3.14159));
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 1);
        let score = results[0].row.as_ref().unwrap().get("score").unwrap();
        assert!((score.as_f64().unwrap() - 3.14159).abs() < 1e-10);
    }

    #[test]
    fn test_filter_with_null_field() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("active".to_string(), serde_json::Value::Null);
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_filter_with_missing_field() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            primary_key: vec![],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
        };
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        let changes = vec![Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!("u1"),
        }];
        let results = process_pipeline(&pipeline, &changes);
        assert_eq!(results.len(), 0);
    }

    // ─── Rayon determinism helpers & tests ───────────────────────────────────

    fn sorted_row_changes(changes: &[RowChange]) -> Vec<&RowChange> {
        let mut sorted: Vec<&RowChange> = changes.iter().collect();
        sorted.sort_by(|a, b| {
            a.query_id
                .cmp(&b.query_id)
                .then_with(|| a.row_key.to_string().cmp(&b.row_key.to_string()))
        });
        sorted
    }

    fn make_add_change(table: &str, id: &str, val: i64, active: bool, category: &str) -> Change {
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!(id));
        row.insert("val".to_string(), serde_json::json!(val));
        row.insert("active".to_string(), serde_json::json!(active));
        row.insert("category".to_string(), serde_json::json!(category));
        Change {
            table: table.to_string(),
            prev_values: vec![],
            next_value: Some(row),
            row_key: serde_json::json!(id),
        }
    }

    fn make_edit_change(table: &str, id: &str, old_val: i64, new_val: i64) -> Change {
        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!(id));
        prev.insert("val".to_string(), serde_json::json!(old_val));
        prev.insert("active".to_string(), serde_json::json!(true));
        prev.insert("category".to_string(), serde_json::json!("a"));
        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!(id));
        next.insert("val".to_string(), serde_json::json!(new_val));
        next.insert("active".to_string(), serde_json::json!(true));
        next.insert("category".to_string(), serde_json::json!("a"));
        Change {
            table: table.to_string(),
            prev_values: vec![prev],
            next_value: Some(next),
            row_key: serde_json::json!(id),
        }
    }

    fn make_delete_change(table: &str, id: &str) -> Change {
        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!(id));
        prev.insert("val".to_string(), serde_json::json!(100));
        prev.insert("active".to_string(), serde_json::json!(false));
        prev.insert("category".to_string(), serde_json::json!("b"));
        Change {
            table: "items".to_string(),
            prev_values: vec![prev],
            next_value: None,
            row_key: serde_json::json!(id),
        }
    }

    fn make_20_pipelines() -> Vec<PipelineConfig> {
        let categories = ["a", "b", "c"];
        (0..20)
            .map(|i| {
                let operators = match i {
                    0..=4 => vec![],
                    5..=9 => vec![Operator::Filter {
                        predicate: serde_json::json!({"op": "gt", "field": "val", "value": i * 10}),
                    }],
                    10..=14 => vec![Operator::Filter {
                        predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
                    }],
                    15..=17 => vec![Operator::Filter {
                        predicate: serde_json::json!({"op": "in", "field": "category", "values": ["a", "b"]}),
                    }],
                    _ => vec![Operator::Filter {
                        predicate: serde_json::json!({
                            "op": "and",
                            "conditions": [
                                {"op": "gt", "field": "val", "value": 0},
                                {"op": "eq", "field": "active", "value": true}
                            ]
                        }),
                    }],
                };
                PipelineConfig {
                    query_id: format!("q{}", i),
                    source_tables: vec!["items".to_string()],
                    primary_key: vec!["id".to_string()],
                    operators,
                }
            })
            .collect()
    }

    fn make_50_changes() -> Vec<Change> {
        let categories = ["a", "b", "c"];
        let mut changes = Vec::with_capacity(50);
        for i in 0..30 {
            changes.push(make_add_change(
                "items",
                &format!("r{}", i),
                (i as i64) * 10,
                i % 2 == 0,
                categories[i % 3],
            ));
        }
        for i in 0..10 {
            changes.push(make_edit_change(
                "items",
                &format!("e{}", i),
                (i as i64) * 5,
                (i as i64) * 5 + 100,
            ));
        }
        for i in 0..10 {
            changes.push(make_delete_change("items", &format!("d{}", i)));
        }
        changes
    }

    #[test]
    fn test_par_iter_determinism_20_pipelines_50_iterations() {
        let pipelines = make_20_pipelines();
        let changes = make_50_changes();
        let changes_arc = Arc::new(changes);

        let baseline: Vec<RowChange> = pipelines
            .par_iter()
            .flat_map(|p| process_pipeline(p, &changes_arc))
            .collect();
        assert!(
            !baseline.is_empty(),
            "baseline must produce results to be a meaningful test"
        );
        let baseline_sorted = sorted_row_changes(&baseline);

        for i in 0..50 {
            let result: Vec<RowChange> = pipelines
                .par_iter()
                .flat_map(|p| process_pipeline(p, &changes_arc))
                .collect();
            let result_sorted = sorted_row_changes(&result);
            assert_eq!(
                baseline_sorted, result_sorted,
                "determinism failed on iteration {}",
                i
            );
        }
    }

    #[test]
    fn test_par_iter_determinism_preserves_pipeline_order() {
        let pipelines: Vec<PipelineConfig> = (0..10)
            .map(|i| PipelineConfig {
                query_id: format!("order_q{}", i),
                source_tables: vec!["items".to_string()],
                primary_key: vec!["id".to_string()],
                operators: vec![],
            })
            .collect();
        let changes: Vec<Change> = (0..5)
            .map(|i| make_add_change("items", &format!("p{}", i), i as i64, true, "a"))
            .collect();
        let changes_arc = Arc::new(changes);

        for iteration in 0..50 {
            let result: Vec<RowChange> = pipelines
                .par_iter()
                .flat_map(|p| process_pipeline(p, &changes_arc))
                .collect();
            let mut last_seen_index: i32 = -1;
            for rc in &result {
                let idx = rc
                    .query_id
                    .strip_prefix("order_q")
                    .unwrap()
                    .parse::<i32>()
                    .unwrap();
                assert!(
                    idx >= last_seen_index,
                    "pipeline order violated at iteration {}: saw q{} after q{}",
                    iteration,
                    idx,
                    last_seen_index
                );
                last_seen_index = idx;
            }
        }
    }

    #[test]
    fn test_par_iter_empty_pipelines_no_panic() {
        let pipelines: Vec<PipelineConfig> = vec![];
        let changes = make_50_changes();
        let changes_arc = Arc::new(changes);
        let result: Vec<RowChange> = pipelines
            .par_iter()
            .flat_map(|p| process_pipeline(p, &changes_arc))
            .collect();
        assert!(result.is_empty());
    }

    #[test]
    fn test_par_iter_empty_changes_no_panic() {
        let pipelines = make_20_pipelines();
        let changes: Vec<Change> = vec![];
        let changes_arc = Arc::new(changes);
        let result: Vec<RowChange> = pipelines
            .par_iter()
            .flat_map(|p| process_pipeline(p, &changes_arc))
            .collect();
        assert!(result.is_empty());
    }

    // ─── Phase 19: Edit Semantics Tests ─────────────────────────────────────

    /// Edit split quadrant: old passes filter, new fails → remove
    #[test]
    fn test_edit_split_old_passes_new_fails_emits_remove() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
            primary_key: vec!["id".to_string()],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u1"));
        prev.insert("active".to_string(), serde_json::json!(true));
        prev.insert("name".to_string(), serde_json::json!("Alice"));

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("active".to_string(), serde_json::json!(false));
        next.insert("name".to_string(), serde_json::json!("Alice"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: Some(next),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "remove");
        assert!(results[0].row.is_none());
    }

    /// Edit split quadrant: old fails filter, new passes → "edit" (PK matched).
    /// When PK matches, line 273 checks passes_filter(next) first — if true, emits "edit"
    /// regardless of whether old passed. This is correct: the downstream operator (TS filter)
    /// handles the split into "add" if needed. At the advance level, PK match = edit.
    #[test]
    fn test_edit_split_old_fails_new_passes_emits_edit() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
            primary_key: vec!["id".to_string()],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u1"));
        prev.insert("active".to_string(), serde_json::json!(false));
        prev.insert("name".to_string(), serde_json::json!("Alice"));

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("active".to_string(), serde_json::json!(true));
        next.insert("name".to_string(), serde_json::json!("Alice Updated"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: Some(next.clone()),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 1);
        // PK matched + new passes filter → "edit" at advance level
        assert_eq!(results[0].change_type, "edit");
        assert_eq!(results[0].row, Some(next));
    }

    /// Edit quadrant: both pass filter → edit with updated row data
    #[test]
    fn test_edit_both_pass_filter_emits_edit() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "gt", "field": "age", "value": 18}),
            }],
            primary_key: vec!["id".to_string()],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u1"));
        prev.insert("age".to_string(), serde_json::json!(25));
        prev.insert("name".to_string(), serde_json::json!("Alice"));

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("age".to_string(), serde_json::json!(30));
        next.insert("name".to_string(), serde_json::json!("Alice Updated"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: Some(next.clone()),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "edit");
        assert_eq!(results[0].row, Some(next));
    }

    /// Edit quadrant: both fail filter → no output
    #[test]
    fn test_edit_both_fail_filter_emits_nothing() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![Operator::Filter {
                predicate: serde_json::json!({"op": "eq", "field": "active", "value": true}),
            }],
            primary_key: vec!["id".to_string()],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u1"));
        prev.insert("active".to_string(), serde_json::json!(false));
        prev.insert("name".to_string(), serde_json::json!("Alice"));

        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("active".to_string(), serde_json::json!(false));
        next.insert("name".to_string(), serde_json::json!("Bob"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: Some(next),
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert!(results.is_empty());
    }

    /// Insert→Delete cancellation (D-92): per-change processing produces add + remove pair.
    /// Full cancellation (zero net output) is a higher-level property not handled at
    /// process_change_for_pipeline level — each changelog entry is processed independently.
    #[test]
    fn test_insert_then_delete_same_pk_produces_add_remove_pair() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![],
            primary_key: vec!["id".to_string()],
        };

        // Change 1: insert
        let mut next = Row::new();
        next.insert("id".to_string(), serde_json::json!("u1"));
        next.insert("name".to_string(), serde_json::json!("Alice"));

        let insert_change = Change {
            table: "users".to_string(),
            prev_values: vec![],
            next_value: Some(next.clone()),
            row_key: serde_json::json!("u1"),
        };

        let insert_results = process_change_for_pipeline(&pipeline, &insert_change);
        assert_eq!(insert_results.len(), 1);
        assert_eq!(insert_results[0].change_type, "add");
        assert_eq!(insert_results[0].row, Some(next.clone()));

        // Change 2: delete same PK
        let delete_change = Change {
            table: "users".to_string(),
            prev_values: vec![next],
            next_value: None,
            row_key: serde_json::json!("u1"),
        };

        let delete_results = process_change_for_pipeline(&pipeline, &delete_change);
        assert_eq!(delete_results.len(), 1);
        assert_eq!(delete_results[0].change_type, "remove");
        assert!(delete_results[0].row.is_none());
    }

    /// Update→Delete produces remove with correct row_key
    #[test]
    fn test_update_then_delete_emits_remove() {
        let pipeline = PipelineConfig {
            query_id: "q1".to_string(),
            source_tables: vec!["users".to_string()],
            operators: vec![],
            primary_key: vec!["id".to_string()],
        };

        let mut prev = Row::new();
        prev.insert("id".to_string(), serde_json::json!("u1"));
        prev.insert("name".to_string(), serde_json::json!("Updated"));

        let change = Change {
            table: "users".to_string(),
            prev_values: vec![prev],
            next_value: None,
            row_key: serde_json::json!("u1"),
        };

        let results = process_change_for_pipeline(&pipeline, &change);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].change_type, "remove");
        assert!(results[0].row.is_none());
        assert_eq!(results[0].row_key, serde_json::json!("u1"));
    }

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
            error_type: None,
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
}
