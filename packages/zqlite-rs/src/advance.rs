use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use napi_derive::napi;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::diff::{self, Change, DiffError, Row, TableAndZqlSpec};

// ─── Pipeline Topology Types ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Operator {
    #[serde(rename = "filter")]
    Filter { predicate: serde_json::Value },
    #[serde(rename = "join")]
    Join {
        parent_key: Vec<String>,
        child_key: Vec<String>,
        child_table: String,
        relationship: String,
    },
    #[serde(rename = "take")]
    Take {
        sort: Vec<(String, String)>,
        limit: Option<usize>,
    },
    #[serde(rename = "exists")]
    Exists {
        relationship: String,
        parent_field: Vec<String>,
        not_exists: bool,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct PipelineConfig {
    pub query_id: String,
    pub source_tables: Vec<String>,
    pub operators: Vec<Operator>,
}

#[derive(Debug, Clone, Serialize)]
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
            // Edit: row updated. Also remove any conflicting prev rows
            for (i, prev) in change.prev_values.iter().enumerate() {
                if i == 0 {
                    // First prev is the "same row" (matching PK) — this is an edit
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
                    // Additional prev rows are unique key conflicts — remove them
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
            // Join/Take/Exists pass through — they require state not available here
            _ => {}
        }
    }
    true
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
                {"type": "filter", "predicate": {"op": "eq", "field": "active", "value": true}},
                {"type": "take", "sort": [["name", "asc"]], "limit": 10}
            ]
        }"#;
        let config: PipelineConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.query_id, "q1");
        assert_eq!(config.operators.len(), 2);
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
}
