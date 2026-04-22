//! Translates a TS AST JSON (from zero-protocol) into `Vec<OperatorConfig>`
//! for use by `hydrate_pipelines`.

use std::collections::HashMap;

use serde::Deserialize;
use zero_ivm_rs::pipeline::OperatorConfig;

// --- AST serde types (mirrors zero-protocol/src/ast.ts) ---

#[derive(Debug, Deserialize)]
pub struct HydrateQuery {
    pub query_id: String,
    pub ast: Ast,
    pub primary_key: Vec<String>,
    /// Map of table_name → { column_name → value_type }.
    /// value_type is one of: "string", "number", "boolean", "json", "null".
    /// When present, only these columns are included in output rows,
    /// and type coercion (e.g. SQLite int → boolean) is applied.
    #[serde(default)]
    pub column_types: Option<HashMap<String, HashMap<String, String>>>,
    /// Map of table_name → primary_key columns, provided by TS from
    /// `#primaryKeys` (Zero schema PKs). Used instead of SQLite PRAGMA
    /// table_info which returns SQLite PKs that may differ from Zero PKs.
    #[serde(default)]
    pub all_primary_keys: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct Ast {
    pub table: String,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(rename = "where")]
    pub where_cond: Option<Box<Condition>>,
    #[serde(default)]
    pub related: Option<Vec<CorrelatedSubquery>>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(rename = "orderBy", default)]
    pub order_by: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub start: Option<StartBound>,
}

#[derive(Debug, Deserialize)]
pub struct StartBound {
    pub row: serde_json::Value,
    pub exclusive: bool,
}

#[derive(Debug, Deserialize)]
pub struct CorrelatedSubquery {
    pub correlation: Correlation,
    pub subquery: Box<Ast>,
    #[serde(default)]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub system: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Correlation {
    #[serde(rename = "parentField")]
    pub parent_field: Vec<String>,
    #[serde(rename = "childField")]
    pub child_field: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum Condition {
    #[serde(rename = "simple")]
    Simple {
        op: String,
        left: ConditionValue,
        right: ConditionValue,
    },
    #[serde(rename = "and")]
    And {
        conditions: Vec<Condition>,
    },
    #[serde(rename = "or")]
    Or {
        conditions: Vec<Condition>,
    },
    #[serde(rename = "correlatedSubquery")]
    CorrelatedSubquery {
        related: Box<CorrelatedSubquery>,
        op: String,
        #[serde(default)]
        flip: Option<bool>,
        #[serde(default)]
        scalar: Option<bool>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ConditionValue {
    #[serde(rename = "literal")]
    Literal { value: serde_json::Value },
    #[serde(rename = "column")]
    Column { name: String },
    #[serde(rename = "static")]
    Static {
        anchor: String,
        field: serde_json::Value,
    },
}

// --- Column introspection ---

pub struct SchemaCache {
    columns: HashMap<String, Vec<String>>,
    primary_keys: HashMap<String, Vec<String>>,
    db_path: String,
}

impl SchemaCache {
    pub fn new(db_path: &str) -> Self {
        Self {
            columns: HashMap::new(),
            primary_keys: HashMap::new(),
            db_path: db_path.to_string(),
        }
    }

    fn ensure_table_info(&mut self, table_name: &str) -> Result<(), String> {
        if self.columns.contains_key(table_name) {
            return Ok(());
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| format!("Failed to open db: {e}"))?;
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info(\"{}\")", table_name))
            .map_err(|e| format!("PRAGMA table_info failed: {e}"))?;
        let mut cols = Vec::new();
        let mut pk_cols: Vec<(i32, String)> = Vec::new();
        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(1)?;
                let pk: i32 = row.get(5)?;
                Ok((name, pk))
            })
            .map_err(|e| format!("query_map failed: {e}"))?;
        for r in rows {
            let (name, pk) = r.map_err(|e| format!("row error: {e}"))?;
            cols.push(name.clone());
            if pk > 0 {
                pk_cols.push((pk, name));
            }
        }
        if cols.is_empty() {
            return Err(format!("Table '{table_name}' not found or has no columns"));
        }
        pk_cols.sort_by_key(|(idx, _)| *idx);
        let pk: Vec<String> = pk_cols.into_iter().map(|(_, name)| name).collect();
        self.columns.insert(table_name.to_string(), cols);
        self.primary_keys.insert(table_name.to_string(), pk);
        Ok(())
    }

    pub fn get_columns(&mut self, table_name: &str) -> Result<Vec<String>, String> {
        self.ensure_table_info(table_name)?;
        Ok(self.columns.get(table_name).unwrap().clone())
    }

    pub fn get_primary_key(&mut self, table_name: &str) -> Result<Vec<String>, String> {
        self.ensure_table_info(table_name)?;
        Ok(self.primary_keys.get(table_name).unwrap().clone())
    }
}

// --- AST -> OperatorConfig translation ---

pub fn ast_to_operator_configs(
    schema: &mut SchemaCache,
    ast: &Ast,
    primary_key: &[String],
) -> Result<Vec<OperatorConfig>, String> {
    let table_name = &ast.table;
    let columns = schema.get_columns(table_name)?;

    let order_by: Vec<(String, String)> = ast.order_by.as_ref().cloned().unwrap_or_default();

    // Build sort: orderBy + PK columns not already in orderBy
    let mut sort = order_by.clone();
    let existing: std::collections::HashSet<String> =
        sort.iter().map(|(f, _)| f.clone()).collect();
    for pk_col in primary_key {
        if !existing.contains(pk_col) {
            sort.push((pk_col.clone(), "asc".to_string()));
        }
    }

    let mut configs = Vec::new();

    // 1. Source
    configs.push(OperatorConfig::Source {
        table_name: table_name.clone(),
        columns,
        primary_key: primary_key.to_vec(),
        sort: sort.clone(),
    });

    // 2. Where conditions -> Filter + Exists
    if let Some(cond) = &ast.where_cond {
        append_condition_configs(schema, &mut configs, cond, primary_key)?;
    }

    // 3. Start -> Skip
    if let Some(start) = &ast.start {
        configs.push(OperatorConfig::Skip {
            bound_row: start.row.clone(),
            exclusive: start.exclusive,
            sort: sort.clone(),
        });
    }

    // 4. Limit -> Take
    if let Some(limit) = ast.limit {
        configs.push(OperatorConfig::Take {
            limit,
            sort: sort.clone(),
            partition_key: None,
        });
    }

    // 5. Related -> Join
    if let Some(related) = &ast.related {
        for rel in related {
            let child_configs =
                ast_to_operator_configs(schema, &rel.subquery, &rel.correlation.child_field)?;
            configs.push(OperatorConfig::Join {
                parent_key: rel.correlation.parent_field.clone(),
                child_key: rel.correlation.child_field.clone(),
                relationship_name: relationship_name(rel),
                child: child_configs,
            });
        }
    }

    Ok(configs)
}

/// Collect all relationship names and their underlying table names
/// from the AST recursively. Used to look up child table PKs.
pub fn collect_child_tables(ast: &Ast) -> Vec<(String, String)> {
    let mut result = Vec::new();
    collect_child_tables_recursive(ast, &mut result);
    result
}

fn collect_child_tables_recursive(ast: &Ast, out: &mut Vec<(String, String)>) {
    if let Some(related) = &ast.related {
        for rel in related {
            let rel_name = relationship_name(rel);
            let table_name = rel.subquery.table.clone();
            out.push((rel_name, table_name));
            collect_child_tables_recursive(&rel.subquery, out);
        }
    }
    // Also check exists subqueries in where conditions
    if let Some(cond) = &ast.where_cond {
        collect_child_tables_from_condition(cond, out);
    }
}

fn collect_child_tables_from_condition(cond: &Condition, out: &mut Vec<(String, String)>) {
    match cond {
        Condition::And { conditions: subs } | Condition::Or { conditions: subs } => {
            for sub in subs {
                collect_child_tables_from_condition(sub, out);
            }
        }
        Condition::CorrelatedSubquery { related, .. } => {
            let rel_name = relationship_name(related);
            let table_name = related.subquery.table.clone();
            out.push((rel_name, table_name));
            collect_child_tables_recursive(&related.subquery, out);
        }
        _ => {}
    }
}

fn relationship_name(rel: &CorrelatedSubquery) -> String {
    if let Some(alias) = &rel.subquery.alias {
        if !alias.is_empty() {
            return alias.clone();
        }
    }
    rel.subquery.table.clone()
}

fn append_condition_configs(
    schema: &mut SchemaCache,
    configs: &mut Vec<OperatorConfig>,
    cond: &Condition,
    primary_key: &[String],
) -> Result<(), String> {
    match cond {
        Condition::Simple { .. } => {
            let pred = condition_to_predicate_json(cond)?;
            configs.push(OperatorConfig::Filter { predicate: pred });
        }
        Condition::And { conditions } => {
            for sub in conditions {
                append_condition_configs(schema, configs, sub, primary_key)?;
            }
        }
        Condition::Or { conditions } => {
            let mut simple_conds = Vec::new();
            let mut csq_conds = Vec::new();
            for sub in conditions {
                if has_csq(sub) {
                    csq_conds.push(sub);
                } else {
                    simple_conds.push(sub);
                }
            }
            if csq_conds.is_empty() {
                let pred = condition_to_predicate_json(cond)?;
                configs.push(OperatorConfig::Filter { predicate: pred });
            } else {
                let or_cond_json = if simple_conds.is_empty() {
                    None
                } else if simple_conds.len() == 1 {
                    Some(condition_to_predicate_json(simple_conds[0])?)
                } else {
                    let preds: Result<Vec<serde_json::Value>, String> =
                        simple_conds.iter().map(|c| condition_to_predicate_json(c)).collect();
                    Some(serde_json::json!({ "or": preds? }))
                };
                for csq in &csq_conds {
                    append_csq_as_exists(schema, configs, csq, primary_key, or_cond_json.clone())?;
                }
            }
        }
        Condition::CorrelatedSubquery { related, op, .. } => {
            let not_exists = op == "NOT EXISTS";
            let child_configs = ast_to_operator_configs(
                schema,
                &related.subquery,
                &related.correlation.child_field,
            )?;
            configs.push(OperatorConfig::Exists {
                relationship_name: relationship_name(related),
                not_exists,
                parent_key: related.correlation.parent_field.clone(),
                child_key: related.correlation.child_field.clone(),
                child: child_configs,
                or_condition: None,
            });
        }
    }
    Ok(())
}

fn has_csq(cond: &Condition) -> bool {
    match cond {
        Condition::CorrelatedSubquery { .. } => true,
        Condition::And { conditions } | Condition::Or { conditions } => {
            conditions.iter().any(has_csq)
        }
        Condition::Simple { .. } => false,
    }
}

fn append_csq_as_exists(
    schema: &mut SchemaCache,
    configs: &mut Vec<OperatorConfig>,
    cond: &Condition,
    primary_key: &[String],
    or_condition: Option<serde_json::Value>,
) -> Result<(), String> {
    match cond {
        Condition::CorrelatedSubquery { related, op, .. } => {
            let not_exists = op == "NOT EXISTS";
            let child_configs = ast_to_operator_configs(
                schema,
                &related.subquery,
                &related.correlation.child_field,
            )?;
            configs.push(OperatorConfig::Exists {
                relationship_name: relationship_name(related),
                not_exists,
                parent_key: related.correlation.parent_field.clone(),
                child_key: related.correlation.child_field.clone(),
                child: child_configs,
                or_condition,
            });
            Ok(())
        }
        _ => Err(format!("Expected CorrelatedSubquery, got {:?}", cond)),
    }
}

fn condition_to_predicate_json(cond: &Condition) -> Result<serde_json::Value, String> {
    match cond {
        Condition::Simple { op, left, right } => {
            let field = match left {
                ConditionValue::Column { name } => name.clone(),
                ConditionValue::Literal { value: left_val } => {
                    // Handle literal=literal conditions (e.g., 1=0 for ALWAYS_FALSE
                    // produced by scalar subquery resolution when no rows match).
                    let right_val = extract_literal_value(right)?;
                    if (op == "=" || op == "IS") && left_val != &right_val {
                        // Always false: OR of nothing
                        return Ok(serde_json::json!({"or": []}));
                    } else if (op == "=" || op == "IS") && left_val == &right_val {
                        // Always true: AND of nothing
                        return Ok(serde_json::json!({"and": []}));
                    }
                    return Err(format!("Unsupported literal-literal comparison with op: {op}"));
                }
                _ => return Err("Filter left side must be a column or literal reference".to_string()),
            };
            let value = extract_literal_value(right)?;
            let rust_op = match op.as_str() {
                "=" | "IS" => "eq",
                "!=" | "IS NOT" => "neq",
                ">" => "gt",
                ">=" => "gte",
                "<" => "lt",
                "<=" => "lte",
                "LIKE" => "like",
                "NOT LIKE" | "ILIKE" | "NOT ILIKE" => "like",
                "IN" => "in",
                "NOT IN" => "in",
                other => return Err(format!("Unsupported operator: {other}")),
            };

            match rust_op {
                "in" => {
                    let arr = match &value {
                        serde_json::Value::Array(a) => a.clone(),
                        other => vec![other.clone()],
                    };
                    // Empty array short-circuit:
                    // `x IN ()` is always false, `x NOT IN ()` is always true.
                    if arr.is_empty() {
                        return if op == "NOT IN" {
                            Ok(serde_json::json!({"and": []})) // always true
                        } else {
                            Ok(serde_json::json!({"or": []})) // always false
                        };
                    }
                    if op == "NOT IN" {
                        return Ok(serde_json::json!({
                            "not": {
                                "field": field,
                                "in": arr,
                            }
                        }));
                    }
                    Ok(serde_json::json!({
                        "field": field,
                        "in": arr,
                    }))
                }
                "like" => {
                    let pattern = value.as_str().unwrap_or("").to_string();
                    let like_pred = serde_json::json!({
                        "field": field,
                        "like": pattern,
                    });
                    if op == "NOT LIKE" || op == "NOT ILIKE" {
                        Ok(serde_json::json!({"not": like_pred}))
                    } else {
                        Ok(like_pred)
                    }
                }
                _ => {
                    if value.is_null() {
                        let null_op = if rust_op == "eq" { "isNull" } else { "isNotNull" };
                        Ok(serde_json::json!({
                            "field": field,
                            null_op: true,
                        }))
                    } else {
                        Ok(serde_json::json!({
                            "field": field,
                            rust_op: value,
                        }))
                    }
                }
            }
        }
        Condition::And { conditions } => {
            let preds: Result<Vec<serde_json::Value>, String> =
                conditions.iter().map(condition_to_predicate_json).collect();
            Ok(serde_json::json!({
                "and": preds?,
            }))
        }
        Condition::Or { conditions } => {
            let preds: Result<Vec<serde_json::Value>, String> =
                conditions.iter().map(condition_to_predicate_json).collect();
            Ok(serde_json::json!({
                "or": preds?,
            }))
        }
        Condition::CorrelatedSubquery { .. } => {
            Err("CorrelatedSubquery should be handled as Exists, not Filter".to_string())
        }
    }
}

fn extract_literal_value(cv: &ConditionValue) -> Result<serde_json::Value, String> {
    match cv {
        ConditionValue::Literal { value } => Ok(value.clone()),
        ConditionValue::Column { name } => Err(format!(
            "Column reference '{name}' on right side not supported in hydration filters"
        )),
        ConditionValue::Static { anchor, field } => Err(format!(
            "Unresolved static parameter (anchor={anchor}, field={field}) in hydration"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_ast() {
        let json = r#"{
            "query_id": "q1",
            "ast": {
                "table": "issues",
                "orderBy": [["created", "desc"]],
                "where": {
                    "type": "simple",
                    "op": "=",
                    "left": {"type": "column", "name": "status"},
                    "right": {"type": "literal", "value": "open"}
                },
                "limit": 10
            },
            "primary_key": ["id"]
        }"#;
        let query: HydrateQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.query_id, "q1");
        assert_eq!(query.ast.table, "issues");
        assert!(query.ast.where_cond.is_some());
        assert_eq!(query.ast.limit, Some(10));
    }

    #[test]
    fn test_condition_to_predicate_eq() {
        let cond = Condition::Simple {
            op: "=".to_string(),
            left: ConditionValue::Column {
                name: "status".to_string(),
            },
            right: ConditionValue::Literal {
                value: serde_json::json!("open"),
            },
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        assert_eq!(pred["field"], "status");
        assert_eq!(pred["eq"], "open");
    }

    #[test]
    fn test_condition_to_predicate_is_null() {
        let cond = Condition::Simple {
            op: "IS".to_string(),
            left: ConditionValue::Column {
                name: "deleted_at".to_string(),
            },
            right: ConditionValue::Literal {
                value: serde_json::Value::Null,
            },
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        assert_eq!(pred["field"], "deleted_at");
        assert_eq!(pred["isNull"], true);
    }

    #[test]
    fn test_condition_to_predicate_in() {
        let cond = Condition::Simple {
            op: "IN".to_string(),
            left: ConditionValue::Column {
                name: "id".to_string(),
            },
            right: ConditionValue::Literal {
                value: serde_json::json!([1, 2, 3]),
            },
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        assert_eq!(pred["field"], "id");
        assert_eq!(pred["in"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_condition_to_predicate_and() {
        let cond = Condition::And {
            conditions: vec![
                Condition::Simple {
                    op: "=".to_string(),
                    left: ConditionValue::Column {
                        name: "a".to_string(),
                    },
                    right: ConditionValue::Literal {
                        value: serde_json::json!(1),
                    },
                },
                Condition::Simple {
                    op: ">".to_string(),
                    left: ConditionValue::Column {
                        name: "b".to_string(),
                    },
                    right: ConditionValue::Literal {
                        value: serde_json::json!(5),
                    },
                },
            ],
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        let arr = pred["and"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_parse_with_related() {
        let json = r#"{
            "query_id": "q2",
            "ast": {
                "table": "issues",
                "related": [{
                    "correlation": {
                        "parentField": ["id"],
                        "childField": ["issueId"]
                    },
                    "subquery": {
                        "table": "comments",
                        "alias": "comments"
                    }
                }]
            },
            "primary_key": ["id"]
        }"#;
        let query: HydrateQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.ast.related.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_parse_correlated_subquery_condition() {
        let json = r#"{
            "type": "correlatedSubquery",
            "op": "EXISTS",
            "related": {
                "correlation": {
                    "parentField": ["id"],
                    "childField": ["issueId"]
                },
                "subquery": {
                    "table": "comments"
                }
            }
        }"#;
        let cond: Condition = serde_json::from_str(json).unwrap();
        assert!(matches!(cond, Condition::CorrelatedSubquery { .. }));
    }
}
