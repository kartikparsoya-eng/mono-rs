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
    db_path: String,
}

impl SchemaCache {
    pub fn new(db_path: &str) -> Self {
        Self {
            columns: HashMap::new(),
            db_path: db_path.to_string(),
        }
    }

    pub fn get_columns(&mut self, table_name: &str) -> Result<Vec<String>, String> {
        if let Some(cols) = self.columns.get(table_name) {
            return Ok(cols.clone());
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| format!("Failed to open db: {e}"))?;
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info(\"{}\")", table_name))
            .map_err(|e| format!("PRAGMA table_info failed: {e}"))?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("query_map failed: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        if cols.is_empty() {
            return Err(format!("Table '{table_name}' not found or has no columns"));
        }
        self.columns.insert(table_name.to_string(), cols.clone());
        Ok(cols)
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
        Condition::Or { .. } => {
            let pred = condition_to_predicate_json(cond)?;
            configs.push(OperatorConfig::Filter { predicate: pred });
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
            });
        }
    }
    Ok(())
}

fn condition_to_predicate_json(cond: &Condition) -> Result<serde_json::Value, String> {
    match cond {
        Condition::Simple { op, left, right } => {
            let field = match left {
                ConditionValue::Column { name } => name.clone(),
                _ => return Err("Filter left side must be a column reference".to_string()),
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
                    Ok(serde_json::json!({
                        "field": field,
                        "in": arr,
                    }))
                }
                "like" => {
                    let pattern = value.as_str().unwrap_or("").to_string();
                    Ok(serde_json::json!({
                        "field": field,
                        "like": pattern,
                    }))
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
