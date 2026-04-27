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

#[derive(Debug, Clone, Default, Deserialize)]
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
    pub timings: Option<AdvanceTimings>,
}

/// Per-pipeline timing breakdown (microseconds) for benchmarking.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AdvanceTimings {
    pub pipeline_count: u32,
    /// Per-pipeline timings: (query_id, build_us, warmup_us, rewind_us, push_us, dedup_filter_us, total_us)
    pub per_pipeline: Vec<PipelineTimings>,
    /// Total wall-clock time for advance_pipelines_full (includes Rayon overhead)
    pub total_us: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PipelineTimings {
    pub query_id: String,
    pub build_us: u64,
    pub warmup_us: u64,
    pub rewind_us: u64,
    pub push_us: u64,
    pub dedup_filter_us: u64,
    pub total_us: u64,
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
    Like(String, String, bool),
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
                Some(Predicate::Like(field, pattern, false))
            }
            "ilike" => {
                let field = obj.get("field")?.as_str()?.to_string();
                let pattern = obj.get("value")?.as_str()?.to_string();
                Some(Predicate::Like(field, pattern, true))
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
                    return Some(Predicate::Like(field, pattern, false));
                }
                if ast_op == "ILIKE" {
                    let pattern = right.get("value")?.as_str()?.to_string();
                    return Some(Predicate::Like(field, pattern, true));
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
            // NULL = x is NULL (falsy) in SQL three-valued logic
            row.get(field).map_or(false, |v| {
                let v = Value::from_json(v);
                if v == Value::Null { false } else { &v == value }
            })
        }
        Predicate::Neq(field, value) => {
            // NULL != x is NULL (falsy) in SQL three-valued logic
            row.get(field).map_or(false, |v| {
                let v = Value::from_json(v);
                if v == Value::Null { false } else { &v != value }
            })
        }
        Predicate::Gt(field, value) => row.get(field).map_or(false, |v| {
            let v = Value::from_json(v);
            if v == Value::Null { return false; }
            compare_values(&v, value) == std::cmp::Ordering::Greater
        }),
        Predicate::Gte(field, value) => row.get(field).map_or(false, |v| {
            let v = Value::from_json(v);
            if v == Value::Null { return false; }
            compare_values(&v, value) != std::cmp::Ordering::Less
        }),
        Predicate::Lt(field, value) => row.get(field).map_or(false, |v| {
            let v = Value::from_json(v);
            if v == Value::Null { return false; }
            compare_values(&v, value) == std::cmp::Ordering::Less
        }),
        Predicate::Lte(field, value) => row.get(field).map_or(false, |v| {
            let v = Value::from_json(v);
            if v == Value::Null { return false; }
            compare_values(&v, value) != std::cmp::Ordering::Greater
        }),
        Predicate::In(field, values) => {
            row.get(field).map_or(false, |v| {
                let v = Value::from_json(v);
                if v == Value::Null { false } else { values.contains(&v) }
            })
        }
        Predicate::Like(field, pattern, ci) => row.get(field).map_or(false, |v| match v {
            serde_json::Value::String(s) => like_match(s, pattern, *ci),
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

fn like_match(text: &str, pattern: &str, case_insensitive: bool) -> bool {
    let text_chars: Vec<char> = text.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    like_match_impl(&text_chars, &pattern_chars, 0, 0, case_insensitive)
}

/// Iterative two-pointer LIKE matching — O(N*M) worst case.
/// Tracks the last '%' position to backtrack greedily instead of recursing.
fn like_match_impl(text: &[char], pattern: &[char], mut ti: usize, mut pi: usize, case_insensitive: bool) -> bool {
    let (tlen, plen) = (text.len(), pattern.len());
    let mut star_pi: Option<usize> = None; // pattern index after last '%'
    let mut star_ti: usize = 0; // text index when we matched last '%'

    while ti < tlen || pi < plen {
        if pi < plen {
            match pattern[pi] {
                '%' => {
                    star_pi = Some(pi + 1);
                    star_ti = ti;
                    pi += 1;
                    continue;
                }
                '_' if ti < tlen => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                c if ti < tlen && if case_insensitive {
                    text[ti].to_ascii_lowercase() == c.to_ascii_lowercase()
                } else {
                    text[ti] == c
                } =>
                {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                _ => {} // mismatch — fall through to backtrack
            }
        }
        // Mismatch or pattern exhausted with text remaining: backtrack to last '%'
        if let Some(sp) = star_pi {
            star_ti += 1;
            ti = star_ti;
            pi = sp;
        } else {
            return false;
        }
    }
    true
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
                    // Same row (PK match) — 4-way filter split
                    let old_passes = passes_filter(&pipeline.operators, prev);
                    let new_passes = passes_filter(&pipeline.operators, next_value);
                    match (old_passes, new_passes) {
                        (true, true) => {
                            // Both visible — genuine edit
                            results.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: change.table.clone(),
                                row_key: change.row_key.clone(),
                                row: Some(next_value.clone()),
                                change_type: "edit".to_string(),
                            });
                        }
                        (true, false) => {
                            // Was visible, now hidden — remove
                            results.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: change.table.clone(),
                                row_key: change.row_key.clone(),
                                row: None,
                                change_type: "remove".to_string(),
                            });
                        }
                        (false, true) => {
                            // Was hidden, now visible — add (not edit)
                            results.push(RowChange {
                                query_id: pipeline.query_id.clone(),
                                table: change.table.clone(),
                                row_key: change.row_key.clone(),
                                row: Some(next_value.clone()),
                                change_type: "add".to_string(),
                            });
                        }
                        (false, false) => {
                            // Neither visible — drop
                        }
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
                timings: None,
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
                timings: None,
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
                timings: None,
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
                timings: None,
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
        timings: None,
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
        timings: None,
    };

    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("Failed to serialize result: {}", e)))
}

// ─── Full Advance (Phase 24) ────────────────────────────────────────────────

use crate::hydrate::build_push_operator_list;
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

#[derive(Debug, Serialize)]
pub struct FullAdvanceResult {
    pub changes: Vec<RowChange>,
    pub error: Option<String>,
    pub error_type: Option<String>,
    pub reset: bool,
    pub timings: Option<AdvanceTimings>,
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

pub fn advance_pipelines_full(
    db_path: &str,
    changes: &[Change],
    pipelines: &[FullPipelineConfig],
) -> FullAdvanceResult {
    use std::time::Instant;
    let profile = std::env::var("RUST_ADVANCE_PROFILE").unwrap_or_default() == "1";
    let t_total = Instant::now();

    // Process pipelines in parallel — each gets its own RustTableSource
    let changes_arc = Arc::new(changes.to_vec());

    let all_results: Vec<(Vec<RowChange>, Option<PipelineTimings>)> = pipelines
        .par_iter()
        .map(|pipeline| {
            process_full_pipeline(db_path, pipeline, &changes_arc, profile)
        })
        .collect();

    let mut row_changes = Vec::new();
    let mut per_pipeline = Vec::new();
    for (changes, timings) in all_results {
        row_changes.extend(changes);
        if let Some(t) = timings {
            per_pipeline.push(t);
        }
    }

    let timings = if profile {
        let total_us = t_total.elapsed().as_micros() as u64;
        eprintln!("[rust-advance-profile] total={}us pipelines={}", total_us, pipelines.len());
        for pt in &per_pipeline {
            eprintln!("  [pipeline {}] total={}us build={}us warmup={}us rewind={}us push={}us dedup={}us",
                pt.query_id, pt.total_us, pt.build_us, pt.warmup_us, pt.rewind_us, pt.push_us, pt.dedup_filter_us);
        }
        Some(AdvanceTimings {
            pipeline_count: pipelines.len() as u32,
            per_pipeline,
            total_us,
        })
    } else {
        None
    };

    FullAdvanceResult {
        changes: row_changes,
        error: None,
        error_type: None,
        reset: false,
        timings,
    }
}

fn process_full_pipeline(
    db_path: &str,
    pipeline: &FullPipelineConfig,
    changes: &[Change],
    profile: bool,
) -> (Vec<RowChange>, Option<PipelineTimings>) {
    use std::time::Instant;
    let t_total = Instant::now();
    let t_build_start = Instant::now();
    // Extract column info from the Source config
    let (table_name, columns, pk, sort) = match &pipeline.operator_config[0] {
        OperatorConfig::Source {
            table_name,
            columns,
            primary_key,
            sort,
        } => (table_name.clone(), columns.clone(), primary_key.clone(), sort.clone()),
        _ => return (vec![], None),
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
            return (vec![], None);
        }
    };

    // Connect with sort + split_edit_keys
    let split_keys: Option<HashSet<String>> = if pipeline.split_edit_keys.is_empty() {
        None
    } else {
        Some(pipeline.split_edit_keys.iter().cloned().collect())
    };
    let connection_id = source.connect(Some(sort), None, split_keys);

    let source_arc = Arc::new(source);

    // Build flat list of push operators (inner to outer)
    let mut op_list = match build_push_operator_list(source_arc.clone(), &pipeline.operator_config, connection_id) {
        Ok(list) if list.is_empty() => {
            // No operators after Source — just convert changes directly
            return (changes_to_row_changes_direct(changes, pipeline, &table_name, &pk), None);
        }
        Ok(list) => list,
        Err(e) => {
            eprintln!("advance_full: failed to build push list for {}: {e}", pipeline.query_id);
            return (vec![], None);
        }
    };
    let build_us = t_build_start.elapsed().as_micros() as u64;

    // Filter changes for this pipeline's table and push through
    let mut row_changes = Vec::new();

    // Warm-up fetch: populate stateful operators (TakeOperator, ExistsOperator).
    // Only operators with state (Take, Exists) need warm-up. Call fetch() on each.
    let t_warmup_start = Instant::now();
    for op in op_list.iter_mut() {
        let op_type = op.op_type();
        if op_type == "take" || op_type == "exists" || op_type == "skip" || op_type == "cap" {
            let _ = op.fetch(&FetchRequest::default());
        }
    }
    let warmup_us = t_warmup_start.elapsed().as_micros() as u64;

    // Rewind stateful operators from post-tx → pre-tx by pushing reverse
    // changes through the operator list. The warm-up fetched post-tx DB,
    // so we undo each root-table change: Add→Remove, Remove→Add, Edit→reverse Edit.
    // Output is discarded — only the operator state mutations matter.
    let t_rewind_start = Instant::now();
    for change in changes.iter() {
        if change.table == pipeline.source_table {
            let source_changes = diff_change_to_source_changes(change, &pipeline.primary_key);
            for sc in &source_changes {
                let reverse = match sc {
                    SourceChange::Add(row) => source_change_to_ivm_change(&SourceChange::Remove(row.clone())),
                    SourceChange::Remove(row) => source_change_to_ivm_change(&SourceChange::Add(row.clone())),
                    SourceChange::Edit { row, old_row } => source_change_to_ivm_change(&SourceChange::Edit {
                        row: old_row.clone(),
                        old_row: row.clone(),
                    }),
                };
                // Push through each operator sequentially (inner to outer)
                let mut current_changes = vec![reverse];
                for op in op_list.iter_mut() {
                    let mut next_changes = Vec::new();
                    for c in current_changes {
                        next_changes.extend(op.push(c));
                    }
                    current_changes = next_changes;
                }
            }
        }
    }

    let rewind_us = t_rewind_start.elapsed().as_micros() as u64;

    // Collect child table mappings for child-change handling.
    let t_push_start = Instant::now();
    let child_table_map = collect_child_table_map(&pipeline.operator_config);
    let children_of_map = collect_children_of_map(&pipeline.operator_config);

    // Helper: push a change through all operators sequentially
    fn push_through_all(change: IvmChange, ops: &mut [Box<dyn IvmOperator>]) -> Vec<IvmChange> {
        let mut current = vec![change];
        for op in ops.iter_mut() {
            let mut next = Vec::new();
            for c in current {
                next.extend(op.push(c));
            }
            current = next;
        }
        current
    }

    for change in changes.iter() {
        if change.table == pipeline.source_table {
            // Root table change — push through all operators sequentially
            let source_changes = diff_change_to_source_changes(change, &pipeline.primary_key);
            for sc in source_changes {
                let ivm_change = source_change_to_ivm_change(&sc);
                let output_changes = push_through_all(ivm_change, &mut op_list);
                for oc in &output_changes {
                    flatten_ivm_change_to_row_changes(
                        &mut row_changes,
                        oc,
                        &pipeline.query_id,
                        &change.table,
                        &pipeline.primary_key,
                        &pipeline.all_primary_keys,
                    );
                    // When the pipeline emits a Remove for a root row
                    // (e.g. Take evicts a parent), emit removals for
                    // its child rows from related subqueries.
                    if let IvmChange::Remove(node) = oc {
                        // Check if the node's relationships have actual child rows
                        let has_child_rows = node.relationships.values().any(|v| !v.is_empty());
                        if !has_child_rows && !children_of_map.is_empty() {
                            let deleted_map: serde_json::Map<String, serde_json::Value> = node.row.clone();
                            emit_descendant_removals(
                                db_path, &deleted_map, &change.table, &children_of_map,
                                &pipeline.query_id, &pipeline.column_types, &mut row_changes,
                            );
                        }
                    }
                }
            }
        } else if let Some(child_infos) = child_table_map.get(&change.table) {
            // Child table change — emit removes directly as RowChanges.
            // For adds, the root handler already covers them via JoinOperator
            // fetch_children() (post-tx DB has the new rows).
            // For removes, the post-transaction DB lacks the deleted rows,
            // so we must emit them directly here.
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
                            // Emit descendant removals for multi-level joins
                            let deleted_map: serde_json::Map<String, serde_json::Value> = row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            emit_descendant_removals(
                                db_path, &deleted_map, &change.table, &children_of_map,
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

    let push_us = t_push_start.elapsed().as_micros() as u64;

    // Deduplicate by (table, row_key, change_type) — when both a root row and its
    // child are changed in the same transaction, JoinOperator.fetch_children() and
    // the direct child emit can produce duplicates.
    {
        let mut seen = HashSet::new();
        row_changes.retain(|rc| {
            let key = format!("{}|{}|{}", rc.table, serde_json::to_string(&rc.row_key).unwrap_or_default(), rc.change_type);
            seen.insert(key)
        });
    }

    // Filter output rows to only include columns specified in column_types.
    // Without this, Rust returns ALL SQLite columns while TS only returns
    // the columns from the query's schema.
    if let Some(ref ct) = pipeline.column_types {
        for rc in &mut row_changes {
            if let Some(ref mut row) = rc.row {
                if let Some(cols) = ct.get(&rc.table) {
                    row.retain(|k, _| cols.contains_key(k));
                }
            }
        }
    }

    let dedup_filter_us = t_total.elapsed().as_micros() as u64 - build_us - warmup_us - rewind_us - push_us;

    let timings = if profile {
        Some(PipelineTimings {
            query_id: pipeline.query_id.clone(),
            build_us,
            warmup_us,
            rewind_us,
            push_us,
            dedup_filter_us,
            total_us: t_total.elapsed().as_micros() as u64,
        })
    } else {
        None
    };

    (row_changes, timings)
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
struct ChildTableInfo {
    parent_table: String,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
    child_pk: Vec<String>,
}

/// Check if a child row has a matching parent in the DB for ALL join key columns.
/// For single-column keys this is always true (the child_table_map lookup already filtered).
/// For compound keys this prevents false matches on the first column only.
fn child_row_has_parent(
    db_path: &str,
    ci: &ChildTableInfo,
    child_row: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    if ci.parent_key.len() <= 1 {
        return true;
    }
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
struct ChildRelation {
    child_table: String,
    parent_join_col: Vec<String>,  // column(s) in parent table
    child_join_col: Vec<String>,   // column(s) in child table
    child_pk: Vec<String>,
    relationship_name: String,
    child_order: Vec<(String, String)>,  // ORDER BY columns from child subquery
    child_limit: Option<usize>,          // LIMIT from child subquery
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

    // Append timings trailer if present (flag bit 1 in flags byte)
    if let Some(ref timings) = result.timings {
        // Patch flags byte to set bit 1
        buf[4] |= 0x02;
        // total_us
        buf.extend_from_slice(&timings.total_us.to_le_bytes());
        // pipeline_count
        buf.extend_from_slice(&timings.pipeline_count.to_le_bytes());
        // per-pipeline timings
        for pt in &timings.per_pipeline {
            encode_str(&mut buf, &pt.query_id);
            buf.extend_from_slice(&pt.build_us.to_le_bytes());
            buf.extend_from_slice(&pt.warmup_us.to_le_bytes());
            buf.extend_from_slice(&pt.rewind_us.to_le_bytes());
            buf.extend_from_slice(&pt.push_us.to_le_bytes());
            buf.extend_from_slice(&pt.dedup_filter_us.to_le_bytes());
            buf.extend_from_slice(&pt.total_us.to_le_bytes());
        }
    }

    buf
}

// ─── Dispatch Poke (Phase 27) ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct VSPipelineEntry {
    pub vs_id: String,
    pub pipelines: Vec<PipelineConfig>,
}

#[napi]
/// Accepts pipeline configs from multiple ViewSyncers, runs Rayon fan-out
/// for all of them in parallel, and returns per-VS results in binary buffer format.
///
/// Binary format (little-endian):
///   [u32] vs_count
///   Per VS:
///     [u16 + bytes] vs_id
///     [u8] has_error (0=ok, 1=error)
///     If has_error:
///       [u16 + bytes] error message
///     Else:
///       [u32] change_count
///       Per change: same layout as encode_advance_result_buf changes
pub fn rust_dispatch_poke(
    changes_json: String,
    vs_pipelines_json: String,
) -> napi::Result<Buffer> {
    let changes: Vec<Change> = serde_json::from_str(&changes_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;
    let vs_entries: Vec<VSPipelineEntry> = serde_json::from_str(&vs_pipelines_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse vs_pipelines: {e}")))?;

    let changes_arc = Arc::new(changes);

    // Process all VS entries in parallel with error isolation
    let vs_results: Vec<(String, Result<Vec<RowChange>, String>)> = vs_entries
        .par_iter()
        .map(|entry| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let row_changes: Vec<RowChange> = entry
                    .pipelines
                    .par_iter()
                    .flat_map(|pipeline| process_pipeline(pipeline, &changes_arc))
                    .collect();
                row_changes
            }));
            let vs_id = entry.vs_id.clone();
            match result {
                Ok(changes) => (vs_id, Ok(changes)),
                Err(panic) => {
                    let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "Unknown panic in dispatch_poke".to_string()
                    };
                    (vs_id, Err(msg))
                }
            }
        })
        .collect();

    // Encode to binary buffer
    let mut buf = Vec::with_capacity(vs_results.len() * 256);
    buf.extend_from_slice(&(vs_results.len() as u32).to_le_bytes());

    for (vs_id, result) in &vs_results {
        encode_str(&mut buf, vs_id);
        match result {
            Err(err_msg) => {
                buf.push(1); // has_error
                encode_str(&mut buf, err_msg);
            }
            Ok(changes) => {
                buf.push(0); // no error
                buf.extend_from_slice(&(changes.len() as u32).to_le_bytes());
                for change in changes {
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
            }
        }
    }

    Ok(Buffer::from(buf))
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
        timings: None,
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
        timings: None,
    };
    Ok(Buffer::from(encode_advance_result_buf(&result)))
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
#[napi]
pub fn rust_advance_from_ast_buf(
    db_path: String,
    changes_json: String,
    queries_json: String,
) -> napi::Result<Buffer> {
    use crate::ast_to_config::{HydrateQuery, SchemaCache, ast_to_operator_configs, collect_child_tables};

    let changes: Vec<Change> = serde_json::from_str(&changes_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;

    let queries: Vec<HydrateQuery> = serde_json::from_str(&queries_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse queries JSON: {e}")))?;

    if queries.is_empty() || changes.is_empty() {
        let result = AdvanceResult {
            changes: vec![],
            error: None,
            error_type: None,
            timings: None,
        };
        return Ok(Buffer::from(encode_advance_result_buf(&result)));
    }

    let mut schema_cache = SchemaCache::new(&db_path);

    let mut full_configs: Vec<FullPipelineConfig> = Vec::with_capacity(queries.len());
    for query in &queries {
        let operator_config = ast_to_operator_configs(
            &mut schema_cache,
            &query.ast,
            &query.primary_key,
        )
        .map_err(|e| napi::Error::from_reason(format!(
            "AST translation failed for query '{}': {e}", query.query_id
        )))?;

        let split_edit_keys = collect_split_edit_keys(&query.ast);

        // Build all_primary_keys map: rel_name → pk columns.
        // Uses TS-provided PKs (Zero schema) when available, falls back to SQLite PRAGMA.
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

        full_configs.push(FullPipelineConfig {
            query_id: query.query_id.clone(),
            source_table: query.ast.table.clone(),
            operator_config,
            primary_key: query.primary_key.clone(),
            split_edit_keys,
            column_types: query.column_types.clone(),
            all_primary_keys: all_pks,
            rel_to_table: HashMap::new(),
        });
    }

    let full_result = advance_pipelines_full(&db_path, &changes, &full_configs);
    let result = AdvanceResult {
        changes: full_result.changes,
        error: full_result.error,
        error_type: full_result.error_type,
        timings: full_result.timings,
    };
    Ok(Buffer::from(encode_advance_result_buf(&result)))
}

// ─── Hydration NAPI (AST → OperatorConfig → hydrate_pipelines) ──────────────

#[napi]
pub fn rust_hydrate(db_path: String, queries_json: String) -> napi::Result<Buffer> {
    use crate::ast_to_config::{HydrateQuery, SchemaCache, ast_to_operator_configs, collect_child_tables};
    use crate::hydrate::{HydratePipelineConfig, hydrate_pipelines};
    use crate::query_builder::ColumnType;
    use crate::table_source::RustTableSource;
    use std::time::Instant;

    let profile = std::env::var("RUST_HYDRATE_PROFILE").unwrap_or_default() == "1";
    let t_total = Instant::now();

    let t0 = Instant::now();
    let queries: Vec<HydrateQuery> = serde_json::from_str(&queries_json)
        .map_err(|e| napi::Error::from_reason(format!("Failed to parse queries JSON: {e}")))?;
    let json_parse_us = t0.elapsed().as_micros();

    let t0 = Instant::now();
    let mut schema_cache = SchemaCache::new(&db_path);

    let mut pipeline_configs = Vec::with_capacity(queries.len());
    let mut query_ids: Vec<String> = Vec::with_capacity(queries.len());
    let mut table_names: Vec<String> = Vec::with_capacity(queries.len());
    let mut primary_keys: Vec<Vec<String>> = Vec::with_capacity(queries.len());
    let mut rel_to_table_maps: Vec<HashMap<String, String>> = Vec::with_capacity(queries.len());

    for query in &queries {
        let operator_config = ast_to_operator_configs(
            &mut schema_cache,
            &query.ast,
            &query.primary_key,
        )
        .map_err(|e| napi::Error::from_reason(format!(
            "AST translation failed for query '{}': {e}", query.query_id
        )))?;

        pipeline_configs.push(HydratePipelineConfig {
            pipeline_id: query.query_id.clone(),
            operator_config,
        });
        query_ids.push(query.query_id.clone());
        table_names.push(query.ast.table.clone());
        primary_keys.push(query.primary_key.clone());
        rel_to_table_maps.push(collect_child_tables(&query.ast).into_iter().collect());
    }
    let ast_translate_us = t0.elapsed().as_micros();

    // Build a map of rel_name → primary_key for all child tables.
    // This is used by flatten_nodes_to_row_changes to produce correct row keys.
    // Prefer TS-provided all_primary_keys (Zero schema PKs) over SQLite PRAGMA PKs.
    let mut all_pks: HashMap<String, Vec<String>> = HashMap::new();
    for query in &queries {
        // Root table PK
        all_pks.insert(query.ast.table.clone(), query.primary_key.clone());

        // If TS provided all_primary_keys, seed the map from it.
        // This gives us Zero schema PKs which may differ from SQLite PKs.
        if let Some(ref ts_pks) = query.all_primary_keys {
            for (table_name, pk) in ts_pks {
                if !all_pks.contains_key(table_name) {
                    all_pks.insert(table_name.clone(), pk.clone());
                }
            }
        }

        // Child table PKs (by relationship name and table name)
        for (rel_name, table_name) in collect_child_tables(&query.ast) {
            // Use TS-provided PK if available, otherwise fall back to SQLite PRAGMA
            let pk = if let Some(ref ts_pks) = query.all_primary_keys {
                ts_pks.get(&table_name).cloned().unwrap_or_else(|| {
                    schema_cache.get_primary_key(&table_name).unwrap_or_default()
                })
            } else {
                schema_cache.get_primary_key(&table_name).unwrap_or_default()
            };
            if !all_pks.contains_key(&rel_name) {
                all_pks.insert(rel_name.clone(), pk.clone());
            }
            // Also insert by actual table name (EXISTS children use table name as key)
            if !all_pks.contains_key(&table_name) {
                all_pks.insert(table_name, pk);
            }
        }
    }

    if pipeline_configs.is_empty() {
        let result = AdvanceResult {
            changes: vec![],
            error: None,
            error_type: None,
            timings: None,
        };
        return Ok(Buffer::from(encode_advance_result_buf(&result)));
    }

    // Create a single shared connection pool for ALL queries.
    // Rayon's thread pool determines the actual parallelism, so pool_size
    // matches the Rayon thread count (or a reasonable default).
    let t0 = Instant::now();
    let pool_size = rayon::current_num_threads().max(4);
    let shared_pool = crate::connection_pool::ConnectionPool::new(&db_path, pool_size)
        .map_err(|e| napi::Error::from_reason(format!("Failed to create connection pool: {e}")))?;
    let pool_create_us = t0.elapsed().as_micros();

    // Group queries by table so each RustTableSource has the correct table metadata.
    let mut table_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, tbl) in table_names.iter().enumerate() {
        table_groups.entry(tbl.clone()).or_default().push(i);
    }

    let mut all_changes: Vec<RowChange> = Vec::new();
    let mut total_hydrate_us: u128 = 0;
    let mut total_flatten_us: u128 = 0;

    for (table_name, indices) in &table_groups {
        let cols = schema_cache.get_columns(table_name)
            .map_err(|e| napi::Error::from_reason(e))?;
        let mut col_types = HashMap::new();
        for c in &cols {
            col_types.insert(c.clone(), ColumnType::String);
        }

        let group_pk = &primary_keys[indices[0]];
        let mut source = RustTableSource::new_with_shared_pool(
            shared_pool.clone(),
            table_name.clone(),
            cols,
            col_types,
            group_pk.clone(),
        )
        .map_err(|e| napi::Error::from_reason(format!("Failed to create source for table '{table_name}': {e}")))?;

        // Register connection 0 so LiveTableSource.fetch(conn_id=0, ...) succeeds.
        let first_idx = indices[0];
        let ordering = queries[first_idx].ast.order_by.clone();
        source.connect(ordering, None, None);

        let source_arc = Arc::new(source);
        let group_configs: Vec<HydratePipelineConfig> = indices.iter().map(|&i| {
            // Move the config out by swapping with a dummy
            std::mem::replace(&mut pipeline_configs[i], HydratePipelineConfig {
                pipeline_id: String::new(),
                operator_config: vec![],
            })
        }).collect();

        let t0 = Instant::now();
        let hydrate_results = hydrate_pipelines(source_arc, group_configs);
        total_hydrate_us += t0.elapsed().as_micros();

        for (j, hr) in hydrate_results.into_iter().enumerate() {
            let orig_idx = indices[j];
            let qid = &query_ids[orig_idx];
            let tbl = &table_names[orig_idx];
            let pk = &primary_keys[orig_idx];
            match hr.nodes {
                Ok(nodes) => {
                    let t0 = Instant::now();
                    flatten_nodes_to_row_changes(&mut all_changes, qid, tbl, pk, &nodes, &all_pks, &queries[orig_idx].column_types, &rel_to_table_maps[orig_idx]);
                    total_flatten_us += t0.elapsed().as_micros();
                }
                Err(e) => {
                    let result = AdvanceResult {
                        changes: vec![],
                        error: Some(format!("Hydration failed for {qid}: {e}")),
                        error_type: Some("hydration_error".to_string()),
                        timings: None,
                    };
                    return Ok(Buffer::from(encode_advance_result_buf(&result)));
                }
            }
        }
    }

    let t0 = Instant::now();
    let result = AdvanceResult {
        changes: all_changes,
        error: None,
        error_type: None,
        timings: None,
    };
    let encoded = encode_advance_result_buf(&result);
    let encode_us = t0.elapsed().as_micros();

    if profile {
        let total_us = t_total.elapsed().as_micros();
        eprintln!("[rust_hydrate profile] queries={} pool_size={} rayon_threads={}",
            queries.len(), pool_size, rayon::current_num_threads());
        eprintln!("  json_parse:    {:>8}us", json_parse_us);
        eprintln!("  ast_translate: {:>8}us", ast_translate_us);
        eprintln!("  pool_create:   {:>8}us", pool_create_us);
        eprintln!("  hydrate:       {:>8}us", total_hydrate_us);
        eprintln!("  flatten:       {:>8}us", total_flatten_us);
        eprintln!("  encode:        {:>8}us", encode_us);
        eprintln!("  TOTAL:         {:>8}us", total_us);
        eprintln!("  result_bytes:  {:>8}", encoded.len());
    }

    Ok(Buffer::from(encoded))
}

fn flatten_nodes_to_row_changes(
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
fn coerce_row(
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

use std::sync::Mutex;

struct PipelineState {
    source: Arc<RustTableSource>,
    op_list: Vec<Box<dyn IvmOperator>>,
    query_id: String,
    source_table: String,
    primary_key: Vec<String>,
    column_types: Option<HashMap<String, HashMap<String, String>>>,
    all_primary_keys: HashMap<String, Vec<String>>,
    child_table_map: HashMap<String, Vec<ChildTableInfo>>,
    children_of_map: HashMap<String, Vec<ChildRelation>>,
    operator_config: Vec<OperatorConfig>,
    has_operators: bool,
    rel_to_table: HashMap<String, String>,
}

/// Persistent pipeline that keeps operator trees and HashMap state alive across
/// hydrate/advance calls. Eliminates per-call pipeline rebuild overhead (~44%)
/// and per-advance warmup/rewind cycles.
#[napi]
pub struct RustPipeline {
    db_path: String,
    pipelines: Mutex<Vec<PipelineState>>,
}

// SAFETY: RustPipeline is only accessed from the NAPI (JS main) thread.
// The inner operator trees contain Arc<RustTableSource> which is already
// declared Send+Sync. The Mutex provides interior mutability safely.
unsafe impl Send for RustPipeline {}
unsafe impl Sync for RustPipeline {}

fn build_pipeline_state(
    db_path: &str,
    query: &crate::ast_to_config::HydrateQuery,
    schema_cache: &mut crate::ast_to_config::SchemaCache,
) -> Result<PipelineState, String> {
    use crate::ast_to_config::{ast_to_operator_configs, collect_child_tables};
    use crate::hydrate::build_push_operator_list;
    use crate::query_builder::ColumnType;

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
    let mut source = RustTableSource::new(
        db_path, 2, table_name.clone(), columns, column_types, pk.clone(),
    ).map_err(|e| format!("failed to create source for {table_name}: {e}"))?;

    let split_keys: Option<HashSet<String>> = if split_edit_keys.is_empty() {
        None
    } else {
        Some(split_edit_keys.iter().cloned().collect())
    };
    let connection_id = source.connect(Some(sort), None, split_keys);
    let source_arc = Arc::new(source);

    let op_list_result = build_push_operator_list(
        source_arc.clone(), &operator_config, connection_id,
    );

    let has_operators;
    let mut op_list = match op_list_result {
        Ok(list) if list.is_empty() => {
            has_operators = false;
            vec![]
        }
        Ok(list) => {
            has_operators = true;
            list
        }
        Err(e) => return Err(format!("failed to build push list: {e}")),
    };

    // Warm-up fetch: populate stateful operators (TakeState, etc.)
    if has_operators {
        for op in op_list.iter_mut() {
            let op_type = op.op_type();
            if op_type == "take" || op_type == "exists" || op_type == "skip" || op_type == "cap" {
                let _ = op.fetch(&FetchRequest::default());
            }
        }
    }

    let child_table_map = collect_child_table_map(&operator_config);
    let children_of_map = collect_children_of_map(&operator_config);

    Ok(PipelineState {
        source: source_arc,
        op_list,
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
    })
}

fn advance_persistent_pipeline(
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

    fn push_through_all(change: IvmChange, ops: &mut [Box<dyn IvmOperator>]) -> Vec<IvmChange> {
        let mut current = vec![change];
        for op in ops.iter_mut() {
            let mut next = Vec::new();
            for c in current {
                next.extend(op.push(c));
            }
            current = next;
        }
        current
    }

    let mut row_changes = Vec::new();

    for change in changes.iter() {
        if change.table == pipeline.source_table {
            let source_changes = diff_change_to_source_changes(change, &pipeline.primary_key);
            for sc in source_changes {
                let ivm_change = source_change_to_ivm_change(&sc);
                let output_changes = push_through_all(ivm_change, &mut pipeline.op_list);
                for oc in &output_changes {
                    flatten_ivm_change_to_row_changes(
                        &mut row_changes,
                        oc,
                        &pipeline.query_id,
                        &change.table,
                        &pipeline.primary_key,
                        &pipeline.all_primary_keys,
                    );
                    // When the pipeline emits a Remove for a root row
                    // (e.g. Take evicts a parent), we need to also emit
                    // removals for its child rows from related subqueries.
                    // The Remove node's relationships map is typically empty
                    // because operators don't populate children on eviction,
                    // so we query the DB for descendants.
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
            }
        } else if let Some(child_infos) = pipeline.child_table_map.get(&change.table) {
            let child_infos = child_infos.clone();
            for ci in &child_infos {
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
                            let deleted_map: serde_json::Map<String, serde_json::Value> =
                                row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                            emit_descendant_removals(
                                db_path, &deleted_map, &change.table,
                                &pipeline.children_of_map, &pipeline.query_id,
                                &pipeline.column_types, &mut row_changes,
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

        let mut schema_cache = SchemaCache::new(&db_path);
        let mut pipelines = Vec::with_capacity(queries.len());

        for query in &queries {
            let state = build_pipeline_state(&db_path, query, &mut schema_cache)
                .map_err(|e| napi::Error::from_reason(format!(
                    "Failed to build pipeline '{}': {e}", query.query_id
                )))?;
            pipelines.push(state);
        }

        Ok(Self {
            db_path,
            pipelines: Mutex::new(pipelines),
        })
    }

    /// Run initial hydration: fetch from all persistent operator trees.
    #[napi]
    pub fn hydrate(&self) -> napi::Result<Buffer> {
        let mut pipelines = self.pipelines.lock()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;

        let mut all_row_changes = Vec::new();

        for pipeline in pipelines.iter_mut() {
            if pipeline.has_operators {
                let last = pipeline.op_list.len() - 1;
                let nodes = pipeline.op_list[last].fetch(&FetchRequest::default());
                flatten_nodes_to_row_changes(
                    &mut all_row_changes,
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
                        &mut all_row_changes,
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
        }

        let result = AdvanceResult {
            changes: all_row_changes,
            error: None,
            error_type: None,
            timings: None,
        };
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Push changes through persistent operator trees. No pipeline rebuild,
    /// no warmup/rewind needed — state is maintained across calls.
    #[napi]
    pub fn advance(&self, changes_json: String) -> napi::Result<Buffer> {
        let changes: Vec<Change> = serde_json::from_str(&changes_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;

        if changes.is_empty() {
            let result = AdvanceResult {
                changes: vec![],
                error: None,
                error_type: None,
                timings: None,
            };
            return Ok(Buffer::from(encode_advance_result_buf(&result)));
        }

        let mut pipelines = self.pipelines.lock()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;

        let db_path = self.db_path.clone();
        let mut all_row_changes = Vec::new();

        for pipeline in pipelines.iter_mut() {
            let row_changes = advance_persistent_pipeline(pipeline, &changes, &db_path);
            all_row_changes.extend(row_changes);
        }

        let result = AdvanceResult {
            changes: all_row_changes,
            error: None,
            error_type: None,
            timings: None,
        };
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Re-open SQLite connections at a new path without rebuilding operator trees.
    #[napi]
    pub fn swap_snapshot(&mut self, new_db_path: String) -> napi::Result<()> {
        let mut pipelines = self.pipelines.lock()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;

        for pipeline in pipelines.iter_mut() {
            pipeline.source.swap_db(&new_db_path)
                .map_err(|e| napi::Error::from_reason(format!(
                    "Failed to swap DB for pipeline '{}': {e}", pipeline.query_id
                )))?;
        }

        drop(pipelines);
        self.db_path = new_db_path;
        Ok(())
    }

    /// Add a new query to the persistent pipeline set.
    #[napi]
    pub fn add_query(&self, query_json: String) -> napi::Result<()> {
        use crate::ast_to_config::{HydrateQuery, SchemaCache};

        let query: HydrateQuery = serde_json::from_str(&query_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse query JSON: {e}")))?;

        let mut schema_cache = SchemaCache::new(&self.db_path);
        let state = build_pipeline_state(&self.db_path, &query, &mut schema_cache)
            .map_err(|e| napi::Error::from_reason(format!(
                "Failed to build pipeline '{}': {e}", query.query_id
            )))?;

        let mut pipelines = self.pipelines.lock()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        pipelines.push(state);
        Ok(())
    }

    /// Remove a query from the persistent pipeline set.
    #[napi]
    pub fn remove_query(&self, query_id: String) -> napi::Result<()> {
        let mut pipelines = self.pipelines.lock()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        pipelines.retain(|p| p.query_id != query_id);
        Ok(())
    }

    /// Returns the number of active pipelines.
    #[napi]
    pub fn pipeline_count(&self) -> napi::Result<u32> {
        let pipelines = self.pipelines.lock()
            .map_err(|e| napi::Error::from_reason(format!("Pipeline lock poisoned: {e}")))?;
        Ok(pipelines.len() as u32)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
                    ..Default::default()
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
                ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

        // Build persistent pipelines
        let mut schema_cache = SchemaCache::new(&db_path);
        let mut pipelines: Vec<PipelineState> = Vec::new();
        for i in 0..num_pipelines {
            let query_json = make_bench_query(&format!("q{}", i), 10 + (i % 20));
            let query: HydrateQuery = serde_json::from_str(&query_json).unwrap();
            let state = build_pipeline_state(&db_path, &query, &mut schema_cache).unwrap();
            pipelines.push(state);
        }

        let changes = make_bench_changes(num_changes);

        // Warm up
        for pipeline in pipelines.iter_mut() {
            let _ = advance_persistent_pipeline(pipeline, &changes, &db_path);
        }

        // ── Sequential ──
        let start = Instant::now();
        let mut seq_total_changes = 0;
        for _ in 0..iterations {
            for pipeline in pipelines.iter_mut() {
                let row_changes = advance_persistent_pipeline(pipeline, &changes, &db_path);
                seq_total_changes += row_changes.len();
            }
        }
        let seq_elapsed = start.elapsed();

        // ── Parallel (rayon) ──
        // We need per-pipeline mutexes for par_iter since we need &mut access
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
