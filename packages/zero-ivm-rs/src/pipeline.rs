use std::collections::HashMap;

use serde::Deserialize;

use crate::cap_op::CapOperator;
use crate::exists_op::ExistsOperator;
use crate::filter::{Predicate, Value};
use crate::filter_op::FilterOperator;
use crate::join_op::JoinOperator;
use crate::operator::Operator;
use crate::skip_op::{Bound, SkipOperator};
use crate::take_op::TakeOperator;
use crate::types::{
    Change, Constraint, FetchRequest, Node, Row, SortDirection, SortSpec, compare_rows,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum OperatorConfig {
    #[serde(rename = "source")]
    Source {
        table_name: String,
        columns: Vec<String>,
        primary_key: Vec<String>,
        sort: Vec<(String, String)>,
    },
    #[serde(rename = "filter")]
    Filter { predicate: serde_json::Value },
    #[serde(rename = "join")]
    Join {
        parent_key: Vec<String>,
        child_key: Vec<String>,
        relationship_name: String,
        child: Vec<OperatorConfig>,
    },
    #[serde(rename = "take")]
    Take {
        limit: usize,
        sort: Vec<(String, String)>,
        partition_key: Option<Vec<String>>,
    },
    #[serde(rename = "exists")]
    Exists {
        relationship_name: String,
        not_exists: bool,
        parent_key: Vec<String>,
        child_key: Vec<String>,
        child: Vec<OperatorConfig>,
    },
    #[serde(rename = "skip")]
    Skip {
        bound_row: serde_json::Value,
        exclusive: bool,
        sort: Vec<(String, String)>,
    },
    #[serde(rename = "cap")]
    Cap {
        limit: usize,
        primary_key: Vec<String>,
        partition_key: Option<Vec<String>>,
    },
}

fn parse_sort(sort: &[(String, String)]) -> Vec<SortSpec> {
    sort.iter()
        .map(|(field, dir)| SortSpec {
            field: field.clone(),
            direction: if dir == "desc" {
                SortDirection::Desc
            } else {
                SortDirection::Asc
            },
        })
        .collect()
}

fn parse_predicate(value: &serde_json::Value) -> Result<Predicate, String> {
    let obj = value.as_object().ok_or("predicate must be an object")?;

    if let Some(field) = obj.get("field") {
        let field = field.as_str().ok_or("field must be a string")?.to_string();
        if let Some(val) = obj.get("eq") {
            return Ok(Predicate::Eq(field, Value::from_json(val)));
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
        return Err(format!("unknown predicate operator for field {}", field));
    }

    if let Some(arr) = obj.get("and") {
        let preds: Result<Vec<Predicate>, String> = arr
            .as_array()
            .ok_or("and must be an array")?
            .iter()
            .map(parse_predicate)
            .collect();
        return Ok(Predicate::And(preds?));
    }

    if let Some(arr) = obj.get("or") {
        let preds: Result<Vec<Predicate>, String> = arr
            .as_array()
            .ok_or("or must be an array")?
            .iter()
            .map(parse_predicate)
            .collect();
        return Ok(Predicate::Or(preds?));
    }

    Err("unknown predicate format".to_string())
}

pub struct SourceOperator {
    rows: Vec<Node>,
    sort: Vec<SortSpec>,
}

impl SourceOperator {
    pub fn new(rows: Vec<Node>, sort: Vec<SortSpec>) -> Self {
        Self { rows, sort }
    }
}

impl Operator for SourceOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let mut result: Vec<Node> = self.rows.clone();

        if let Some(ref constraint) = req.constraint {
            result.retain(|node| node.row.get(&constraint.key) == Some(&constraint.value));
        }

        result.sort_by(|a, b| compare_rows(&a.row, &b.row, &self.sort));

        if let Some(ref start) = req.start {
            let idx = result.iter().position(|node| {
                let cmp = compare_rows(&node.row, &start.row, &self.sort);
                if start.basis == "after" {
                    cmp == std::cmp::Ordering::Greater
                } else {
                    cmp != std::cmp::Ordering::Less
                }
            });
            if let Some(i) = idx {
                result = result[i..].to_vec();
            } else {
                result.clear();
            }
        }

        if req.reverse {
            result.reverse();
        }

        result
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "source"
    }
}

pub fn build_operator(configs: &[OperatorConfig]) -> Result<Box<dyn Operator>, String> {
    if configs.is_empty() {
        return Err("empty operator config".to_string());
    }

    let mut current: Option<Box<dyn Operator>> = None;

    for config in configs {
        current = Some(match config {
            OperatorConfig::Source { sort, .. } => {
                let sort_specs = parse_sort(sort);
                Box::new(SourceOperator::new(vec![], sort_specs))
            }
            OperatorConfig::Filter { predicate } => {
                let input = current.ok_or("filter requires an input operator")?;
                let pred = parse_predicate(predicate)?;
                Box::new(FilterOperator::new(input, pred))
            }
            OperatorConfig::Join {
                parent_key,
                child_key,
                relationship_name,
                child,
            } => {
                let parent = current.ok_or("join requires a parent operator")?;
                let child_op = build_operator(child)?;
                Box::new(JoinOperator::new(
                    parent,
                    child_op,
                    parent_key.clone(),
                    child_key.clone(),
                    relationship_name.clone(),
                ))
            }
            OperatorConfig::Take {
                limit,
                sort,
                partition_key,
            } => {
                let input = current.ok_or("take requires an input operator")?;
                let sort_specs = parse_sort(sort);
                Box::new(TakeOperator::new(
                    input,
                    *limit,
                    sort_specs,
                    partition_key.clone(),
                ))
            }
            OperatorConfig::Exists {
                relationship_name,
                not_exists,
                parent_key,
                child_key,
                child,
            } => {
                let input = current.ok_or("exists requires an input operator")?;
                let child_op = build_operator(child)?;
                Box::new(ExistsOperator::new(
                    input,
                    child_op,
                    relationship_name.clone(),
                    *not_exists,
                    parent_key.clone(),
                    child_key.clone(),
                ))
            }
            OperatorConfig::Skip {
                bound_row,
                exclusive,
                sort,
            } => {
                let input = current.ok_or("skip requires an input operator")?;
                let row: Row = serde_json::from_value(bound_row.clone())
                    .map_err(|e| format!("invalid bound_row: {}", e))?;
                let sort_specs = parse_sort(sort);
                let bound = Bound {
                    row,
                    exclusive: *exclusive,
                };
                Box::new(SkipOperator::new(input, bound, sort_specs))
            }
            OperatorConfig::Cap {
                limit,
                primary_key,
                partition_key,
            } => {
                let input = current.ok_or("cap requires an input operator")?;
                Box::new(CapOperator::new(
                    input,
                    *limit,
                    primary_key.clone(),
                    partition_key.clone(),
                ))
            }
        });
    }

    current.ok_or("no operator built".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_source_config() {
        let json = r#"{"type":"source","table_name":"users","columns":["id","name"],"primary_key":["id"],"sort":[["id","asc"]]}"#;
        let config: OperatorConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, OperatorConfig::Source { .. }));
    }

    #[test]
    fn test_deserialize_filter_config() {
        let json = r#"{"type":"filter","predicate":{"field":"status","eq":"active"}}"#;
        let config: OperatorConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, OperatorConfig::Filter { .. }));
    }

    #[test]
    fn test_deserialize_join_config() {
        let json = r#"{"type":"join","parent_key":["id"],"child_key":["parentId"],"relationship_name":"items","child":[{"type":"source","table_name":"items","columns":["id"],"primary_key":["id"],"sort":[["id","asc"]]}]}"#;
        let config: OperatorConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, OperatorConfig::Join { .. }));
    }

    #[test]
    fn test_deserialize_take_config() {
        let json = r#"{"type":"take","limit":10,"sort":[["id","asc"]],"partition_key":null}"#;
        let config: OperatorConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, OperatorConfig::Take { .. }));
    }

    #[test]
    fn test_deserialize_exists_config() {
        let json = r#"{"type":"exists","relationship_name":"children","not_exists":false,"parent_key":["id"],"child_key":["parent_id"],"child":[{"type":"source","table_name":"c","columns":["id"],"primary_key":["id"],"sort":[["id","asc"]]}]}"#;
        let config: OperatorConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, OperatorConfig::Exists { .. }));
    }

    #[test]
    fn test_deserialize_skip_config() {
        let json = r#"{"type":"skip","bound_row":{"id":5},"exclusive":true,"sort":[["id","asc"]]}"#;
        let config: OperatorConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, OperatorConfig::Skip { .. }));
    }

    #[test]
    fn test_deserialize_cap_config() {
        let json = r#"{"type":"cap","limit":100,"primary_key":["id"],"partition_key":null}"#;
        let config: OperatorConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, OperatorConfig::Cap { .. }));
    }

    #[test]
    fn test_build_source_operator() {
        let configs = vec![OperatorConfig::Source {
            table_name: "users".to_string(),
            columns: vec!["id".to_string()],
            primary_key: vec!["id".to_string()],
            sort: vec![("id".to_string(), "asc".to_string())],
        }];
        let op = build_operator(&configs);
        assert!(op.is_ok());
    }

    #[test]
    fn test_build_empty_config_errors() {
        let result = build_operator(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_source_operator_fetch() {
        let rows = vec![
            Node {
                row: [("id".to_string(), serde_json::json!(3))]
                    .into_iter()
                    .collect(),
                relationships: HashMap::new(),
            },
            Node {
                row: [("id".to_string(), serde_json::json!(1))]
                    .into_iter()
                    .collect(),
                relationships: HashMap::new(),
            },
            Node {
                row: [("id".to_string(), serde_json::json!(2))]
                    .into_iter()
                    .collect(),
                relationships: HashMap::new(),
            },
        ];
        let sort = vec![SortSpec {
            field: "id".to_string(),
            direction: SortDirection::Asc,
        }];
        let mut op = SourceOperator::new(rows, sort);
        let result = op.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(2));
        assert_eq!(result[2].row.get("id").unwrap(), &serde_json::json!(3));
    }

    #[test]
    fn test_source_operator_fetch_with_constraint() {
        let rows = vec![
            Node {
                row: [
                    ("id".to_string(), serde_json::json!(1)),
                    ("status".to_string(), serde_json::json!("a")),
                ]
                .into_iter()
                .collect(),
                relationships: HashMap::new(),
            },
            Node {
                row: [
                    ("id".to_string(), serde_json::json!(2)),
                    ("status".to_string(), serde_json::json!("b")),
                ]
                .into_iter()
                .collect(),
                relationships: HashMap::new(),
            },
        ];
        let sort = vec![SortSpec {
            field: "id".to_string(),
            direction: SortDirection::Asc,
        }];
        let mut op = SourceOperator::new(rows, sort);
        let result = op.fetch(&FetchRequest {
            constraint: Some(Constraint {
                key: "status".to_string(),
                value: serde_json::json!("a"),
            }),
            ..Default::default()
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
    }
}
