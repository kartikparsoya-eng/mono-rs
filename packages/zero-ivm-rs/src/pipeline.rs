use napi_derive::napi;
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

#[derive(Debug, Clone, Deserialize)]
pub struct ExistsBranch {
    pub relationship_name: String,
    pub not_exists: bool,
    pub parent_key: Vec<String>,
    pub child_key: Vec<String>,
    pub child: Vec<OperatorConfig>,
}

#[derive(Debug, Clone, Deserialize)]
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
        #[serde(default)]
        or_condition: Option<serde_json::Value>,
    },
    #[serde(rename = "or_exists")]
    OrExists {
        branches: Vec<ExistsBranch>,
        #[serde(default)]
        or_condition: Option<serde_json::Value>,
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

fn parse_predicate(value: &serde_json::Value) -> std::result::Result<Predicate, String> {
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
            let values = val
                .as_array()
                .ok_or("'in' value must be an array")?
                .iter()
                .map(Value::from_json)
                .collect();
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
        if obj.get("isNull").is_some() {
            return Ok(Predicate::IsNull(field));
        }
        if obj.get("isNotNull").is_some() {
            return Ok(Predicate::IsNotNull(field));
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

    if let Some(inner) = obj.get("not") {
        return Ok(Predicate::Not(Box::new(parse_predicate(inner)?)));
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

pub fn build_operator(configs: &[OperatorConfig]) -> std::result::Result<Box<dyn Operator>, String> {
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
                or_condition,
            } => {
                let input = current.ok_or("exists requires an input operator")?;
                let child_op = build_operator(child)?;
                let or_pred = if let Some(oc) = or_condition {
                    Some(parse_predicate(oc)?)
                } else {
                    None
                };
                Box::new(ExistsOperator::new(
                    input,
                    child_op,
                    relationship_name.clone(),
                    *not_exists,
                    parent_key.clone(),
                    child_key.clone(),
                ).with_or_predicate(or_pred))
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
            OperatorConfig::OrExists { .. } => {
                // OrExists is handled by the hydration path's ParallelOrExistsOperator.
                // In the push path, this should not appear (TS handles advance).
                return Err("OrExists not supported in push path".to_string());
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

#[napi]
pub struct Pipeline {
    operator: Box<dyn Operator>,
}

#[napi]
impl Pipeline {
    #[napi(factory)]
    pub fn build(config_json: String) -> napi::Result<Self> {
        let configs: Vec<OperatorConfig> = serde_json::from_str(&config_json)
            .map_err(|e| napi::Error::from_reason(format!("Invalid pipeline config: {}", e)))?;
        let operator = build_operator(&configs)
            .map_err(|e| napi::Error::from_reason(format!("Failed to build pipeline: {}", e)))?;
        Ok(Self { operator })
    }

    #[napi]
    pub fn fetch(&mut self, request_json: String) -> napi::Result<String> {
        let req: FetchRequest = serde_json::from_str(&request_json)
            .map_err(|e| napi::Error::from_reason(format!("Invalid fetch request: {}", e)))?;
        let nodes = self.operator.fetch(&req);
        serde_json::to_string(&nodes)
            .map_err(|e| napi::Error::from_reason(format!("Serialization error: {}", e)))
    }

    #[napi]
    pub fn push(&mut self, change_json: String) -> napi::Result<String> {
        let change: Change = serde_json::from_str(&change_json)
            .map_err(|e| napi::Error::from_reason(format!("Invalid change: {}", e)))?;
        let changes = self.operator.push(change);
        serde_json::to_string(&changes)
            .map_err(|e| napi::Error::from_reason(format!("Serialization error: {}", e)))
    }
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

    fn make_node(pairs: &[(&str, serde_json::Value)]) -> Node {
        Node {
            row: pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            relationships: HashMap::new(),
        }
    }

    fn id_sort() -> Vec<SortSpec> {
        vec![SortSpec {
            field: "id".to_string(),
            direction: SortDirection::Asc,
        }]
    }

    #[test]
    fn test_integration_filter_pipeline() {
        let rows: Vec<Node> = (1..=5)
            .map(|i| {
                make_node(&[
                    ("id", serde_json::json!(i)),
                    ("status", serde_json::json!(if i % 2 == 0 { "active" } else { "inactive" })),
                ])
            })
            .collect();
        let source = Box::new(SourceOperator::new(rows, id_sort()));
        let pred = Predicate::Eq("status".to_string(), Value::String("active".to_string()));
        let mut pipeline = FilterOperator::new(source, pred);

        let result = pipeline.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(2));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(4));
    }

    #[test]
    fn test_integration_join_pipeline() {
        let parents = vec![
            make_node(&[("id", serde_json::json!(1)), ("name", serde_json::json!("Alice"))]),
            make_node(&[("id", serde_json::json!(2)), ("name", serde_json::json!("Bob"))]),
            make_node(&[("id", serde_json::json!(3)), ("name", serde_json::json!("Carol"))]),
        ];
        let children = vec![
            make_node(&[("id", serde_json::json!(10)), ("parentId", serde_json::json!(1))]),
            make_node(&[("id", serde_json::json!(11)), ("parentId", serde_json::json!(1))]),
            make_node(&[("id", serde_json::json!(20)), ("parentId", serde_json::json!(2))]),
            make_node(&[("id", serde_json::json!(21)), ("parentId", serde_json::json!(2))]),
            make_node(&[("id", serde_json::json!(30)), ("parentId", serde_json::json!(3))]),
            make_node(&[("id", serde_json::json!(31)), ("parentId", serde_json::json!(3))]),
        ];
        let parent_source = Box::new(SourceOperator::new(parents, id_sort()));
        let child_source = Box::new(SourceOperator::new(children, id_sort()));
        let mut pipeline = JoinOperator::new(
            parent_source,
            child_source,
            vec!["id".to_string()],
            vec!["parentId".to_string()],
            "items".to_string(),
        );

        let result = pipeline.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].relationships["items"].len(), 2);
        assert_eq!(result[1].relationships["items"].len(), 2);
        assert_eq!(result[2].relationships["items"].len(), 2);
    }

    #[test]
    fn test_integration_take_pipeline() {
        let rows: Vec<Node> = (1..=5)
            .map(|i| make_node(&[("id", serde_json::json!(i))]))
            .collect();
        let source = Box::new(SourceOperator::new(rows, id_sort()));
        let mut pipeline = TakeOperator::new(source, 2, id_sort(), None);

        let result = pipeline.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(2));

        // Push add of id=0 (before bound=2) -> displaces bound
        let changes = pipeline.push(Change::Add(make_node(&[("id", serde_json::json!(0))])));
        assert!(changes.len() >= 2);
        let has_remove = changes.iter().any(|c| matches!(c, Change::Remove(_)));
        assert!(has_remove, "should displace bound row");
    }

    #[test]
    fn test_integration_exists_pipeline() {
        let parents = vec![
            make_node(&[("id", serde_json::json!(1))]),
            make_node(&[("id", serde_json::json!(2))]),
            make_node(&[("id", serde_json::json!(3))]),
        ];
        // Only parents 1 and 3 have children
        let children = vec![
            make_node(&[("id", serde_json::json!(10)), ("pid", serde_json::json!(1))]),
            make_node(&[("id", serde_json::json!(30)), ("pid", serde_json::json!(3))]),
        ];
        let parent_source = Box::new(SourceOperator::new(parents, id_sort()));
        let child_source = Box::new(SourceOperator::new(children, id_sort()));
        let mut pipeline = ExistsOperator::new(
            parent_source,
            child_source,
            "children".to_string(),
            false,
            vec!["id".to_string()],
            vec!["pid".to_string()],
        );

        let result = pipeline.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(1));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(3));
    }

    #[test]
    fn test_integration_filter_take_push() {
        let rows: Vec<Node> = (1..=10)
            .map(|i| {
                make_node(&[
                    ("id", serde_json::json!(i)),
                    ("status", serde_json::json!(if i % 2 == 0 { "active" } else { "inactive" })),
                ])
            })
            .collect();
        let source = Box::new(SourceOperator::new(rows, id_sort()));
        let pred = Predicate::Eq("status".to_string(), Value::String("active".to_string()));
        let filter: Box<dyn Operator> = Box::new(FilterOperator::new(source, pred));
        let mut pipeline = TakeOperator::new(filter, 2, id_sort(), None);

        // Fetch: should get first 2 active rows (id=2, id=4)
        let result = pipeline.fetch(&FetchRequest::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].row.get("id").unwrap(), &serde_json::json!(2));
        assert_eq!(result[1].row.get("id").unwrap(), &serde_json::json!(4));

        // Push an active Add with id=1 -> filter passes, take processes
        let add = Change::Add(make_node(&[
            ("id", serde_json::json!(1)),
            ("status", serde_json::json!("active")),
        ]));
        let changes = pipeline.push(add);
        // id=1 is before bound (id=4), so should displace
        assert!(!changes.is_empty());

        // Demonstrate filter-level push: inactive row is dropped by filter
        // before it ever reaches take. In the current architecture, push
        // propagation is the caller's responsibility.
        let mut filter_only = FilterOperator::new(
            Box::new(SourceOperator::new(vec![], id_sort())),
            Predicate::Eq("status".to_string(), Value::String("active".to_string())),
        );
        let add_inactive = Change::Add(make_node(&[
            ("id", serde_json::json!(0)),
            ("status", serde_json::json!("inactive")),
        ]));
        let filter_result = filter_only.push(add_inactive);
        assert!(filter_result.is_empty(), "filter should drop inactive row");
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
