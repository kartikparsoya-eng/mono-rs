use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;
use serde::Deserialize;

use zero_ivm_rs::operator::Operator;
use zero_ivm_rs::pipeline::{build_operator, OperatorConfig};
use zero_ivm_rs::types::{Change, FetchRequest, Node};

use crate::connection_pool::ConnectionPool;
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
                    key: c.key.clone(),
                    value: c.value.clone(),
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

#[derive(Debug, Deserialize)]
pub struct HydratePipelineConfig {
    pub pipeline_id: String,
    pub operator_config: Vec<OperatorConfig>,
}

#[derive(Debug)]
pub struct HydrateResult {
    pub pipeline_id: String,
    pub nodes: Result<Vec<Node>, String>,
}

/// Hydrate multiple pipelines in parallel using Rayon.
pub fn hydrate_pipelines(
    source: Arc<RustTableSource>,
    configs: Vec<HydratePipelineConfig>,
) -> Vec<HydrateResult> {
    configs
        .into_par_iter()
        .map(|config| {
            let result = hydrate_single_pipeline(source.clone(), config.operator_config);
            HydrateResult {
                pipeline_id: config.pipeline_id,
                nodes: result,
            }
        })
        .collect()
}

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

    fn parse_predicate(value: &serde_json::Value) -> Result<Predicate, String> {
        use zero_ivm_rs::filter::Value;
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
            return Err(format!("unknown predicate operator for field {field}"));
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

    match config {
        OperatorConfig::Source { .. } => {
            Err("unexpected Source in non-root position".to_string())
        }
        OperatorConfig::Filter { predicate } => {
            let pred = parse_predicate(predicate)?;
            Ok(Box::new(FilterOperator::new(input, pred)))
        }
        OperatorConfig::Join {
            parent_key,
            child_key,
            relationship_name,
            child,
        } => {
            let child_op = build_operator_with_live_source(source, child)?;
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
        } => {
            let child_op = build_operator_with_live_source(source, child)?;
            Ok(Box::new(ExistsOperator::new(
                input,
                child_op,
                relationship_name.clone(),
                *not_exists,
                parent_key.clone(),
                child_key.clone(),
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

    #[test]
    fn test_hydrate_multiple_pipelines_parallel() {
        let db = setup_test_db();
        let mut src = make_source(&db, 8);
        for _ in 0..5 {
            src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        }
        let arc_src = Arc::new(src);

        let configs: Vec<HydratePipelineConfig> = (0..5)
            .map(|i| HydratePipelineConfig {
                pipeline_id: format!("pipeline_{i}"),
                operator_config: vec![OperatorConfig::Source {
                    table_name: "users".into(),
                    columns: vec!["id".into(), "name".into(), "age".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            })
            .collect();

        let results = hydrate_pipelines(arc_src, configs);
        assert_eq!(results.len(), 5);
        for r in &results {
            assert!(r.nodes.is_ok());
            assert_eq!(r.nodes.as_ref().unwrap().len(), 5);
        }
    }

    #[test]
    fn test_hydrate_mixed_pipelines() {
        let db = setup_test_db();
        let mut src = make_source(&db, 8);
        for _ in 0..3 {
            src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        }
        let arc_src = Arc::new(src);

        let configs = vec![
            HydratePipelineConfig {
                pipeline_id: "all".into(),
                operator_config: vec![OperatorConfig::Source {
                    table_name: "users".into(),
                    columns: vec!["id".into(), "name".into(), "age".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
            HydratePipelineConfig {
                pipeline_id: "filtered".into(),
                operator_config: vec![
                    OperatorConfig::Source {
                        table_name: "users".into(),
                        columns: vec!["id".into(), "name".into(), "age".into()],
                        primary_key: vec!["id".into()],
                        sort: vec![("id".into(), "asc".into())],
                    },
                    OperatorConfig::Filter {
                        predicate: json!({"field": "name", "eq": "Bob"}),
                    },
                ],
            },
            HydratePipelineConfig {
                pipeline_id: "limited".into(),
                operator_config: vec![
                    OperatorConfig::Source {
                        table_name: "users".into(),
                        columns: vec!["id".into(), "name".into(), "age".into()],
                        primary_key: vec!["id".into()],
                        sort: vec![("id".into(), "asc".into())],
                    },
                    OperatorConfig::Take {
                        limit: 3,
                        sort: vec![("id".into(), "asc".into())],
                        partition_key: None,
                    },
                ],
            },
        ];

        let results = hydrate_pipelines(arc_src, configs);
        let by_id: HashMap<&str, &HydrateResult> = results
            .iter()
            .map(|r| (r.pipeline_id.as_str(), r))
            .collect();

        assert_eq!(by_id["all"].nodes.as_ref().unwrap().len(), 5);
        assert_eq!(by_id["filtered"].nodes.as_ref().unwrap().len(), 1);
        assert_eq!(by_id["limited"].nodes.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn test_hydrate_same_snapshot() {
        let db = setup_test_db();
        let mut src = make_source(&db, 4);
        for _ in 0..3 {
            src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        }
        let arc_src = Arc::new(src);

        let configs: Vec<HydratePipelineConfig> = (0..3)
            .map(|i| HydratePipelineConfig {
                pipeline_id: format!("p{i}"),
                operator_config: vec![OperatorConfig::Source {
                    table_name: "users".into(),
                    columns: vec!["id".into(), "name".into(), "age".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            })
            .collect();

        let results = hydrate_pipelines(arc_src, configs);
        let first = results[0].nodes.as_ref().unwrap();
        for r in &results[1..] {
            let nodes = r.nodes.as_ref().unwrap();
            assert_eq!(nodes.len(), first.len());
            for (a, b) in first.iter().zip(nodes.iter()) {
                assert_eq!(a.row, b.row);
            }
        }
    }

    #[test]
    fn test_hydrate_empty_config_errors() {
        let db = setup_test_db();
        let src = make_source(&db, 2);
        let arc_src = Arc::new(src);

        let configs = vec![HydratePipelineConfig {
            pipeline_id: "bad".into(),
            operator_config: vec![],
        }];

        let results = hydrate_pipelines(arc_src, configs);
        assert!(results[0].nodes.is_err());
    }
}
