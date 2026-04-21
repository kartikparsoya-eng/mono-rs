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

/// Free function for parallel child fetching (avoids capturing &self in par_iter closures).
fn fetch_children_for_row_static(
    source: &Arc<RustTableSource>,
    child_config: &[OperatorConfig],
    parent_key: &[String],
    child_key: &[String],
    parent_row: &zero_ivm_rs::types::Row,
) -> Vec<Node> {
    use zero_ivm_rs::filter::{Value, compare_values};
    let constraint = if !parent_key.is_empty() && !child_key.is_empty() {
        parent_row.get(&parent_key[0]).map(|v| {
            zero_ivm_rs::types::Constraint {
                key: child_key[0].clone(),
                value: v.clone(),
            }
        })
    } else {
        None
    };
    let child_source = make_child_source(source, child_config);
    let child_source = match child_source {
        Some(s) => s,
        None => return vec![],
    };
    let mut child_op = match build_operator_with_live_source(Arc::new(child_source), child_config) {
        Ok(op) => op,
        Err(_) => return vec![],
    };
    let child_nodes = child_op.fetch(&FetchRequest {
        constraint,
        start: None,
        reverse: false,
    });
    if parent_key.len() > 1 {
        child_nodes
            .into_iter()
            .filter(|cn| {
                parent_key.iter().zip(child_key.iter()).all(|(pk, ck)| {
                    let pv = parent_row.get(pk).map(Value::from_json).unwrap_or(Value::Null);
                    let cv = cn.row.get(ck).map(Value::from_json).unwrap_or(Value::Null);
                    !matches!(pv, Value::Null) && !matches!(cv, Value::Null)
                        && compare_values(&pv, &cv) == std::cmp::Ordering::Equal
                })
            })
            .collect()
    } else {
        child_nodes
    }
}

fn fetch_child_count_static(
    source: &Arc<RustTableSource>,
    child_config: &[OperatorConfig],
    parent_key: &[String],
    child_key: &[String],
    parent_row: &zero_ivm_rs::types::Row,
) -> usize {
    let constraint = if !parent_key.is_empty() && !child_key.is_empty() {
        parent_row.get(&parent_key[0]).map(|v| {
            zero_ivm_rs::types::Constraint {
                key: child_key[0].clone(),
                value: v.clone(),
            }
        })
    } else {
        None
    };
    let child_source = make_child_source(source, child_config);
    let child_source = match child_source {
        Some(s) => s,
        None => return 0,
    };
    let mut child_op = match build_operator_with_live_source(Arc::new(child_source), child_config) {
        Ok(op) => op,
        Err(_) => return 0,
    };
    child_op.fetch(&FetchRequest {
        constraint,
        start: None,
        reverse: false,
    }).len()
}

/// Create a RustTableSource for the child table based on the child config's Source entry.
fn make_child_source(
    parent_source: &Arc<RustTableSource>,
    child_config: &[OperatorConfig],
) -> Option<RustTableSource> {
    if child_config.is_empty() {
        return None;
    }
    match &child_config[0] {
        OperatorConfig::Source {
            table_name,
            columns,
            primary_key,
            sort,
        } => {
            let mut ct = HashMap::new();
            for c in columns {
                ct.insert(c.clone(), ColumnType::String);
            }
            let mut src = RustTableSource::new(
                parent_source.db_path(),
                2,
                table_name.clone(),
                columns.clone(),
                ct,
                primary_key.clone(),
            ).ok()?;
            let ordering: Vec<(String, String)> = sort.clone();
            src.connect(Some(ordering), None, None);
            Some(src)
        }
        _ => None,
    }
}

/// A Join operator that fetches children in parallel across parent rows.
/// Instead of holding a single mutable child operator, it holds the child
/// operator config and rebuilds the child tree per parent row on Rayon threads.
pub struct ParallelJoinOperator {
    parent: Box<dyn Operator>,
    child_config: Vec<OperatorConfig>,
    source: Arc<RustTableSource>,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
}

impl ParallelJoinOperator {
    pub fn new(
        parent: Box<dyn Operator>,
        child_config: Vec<OperatorConfig>,
        source: Arc<RustTableSource>,
        parent_key: Vec<String>,
        child_key: Vec<String>,
        relationship_name: String,
    ) -> Self {
        Self {
            parent,
            child_config,
            source,
            parent_key,
            child_key,
            relationship_name,
        }
    }
}

unsafe impl Send for ParallelJoinOperator {}

impl Operator for ParallelJoinOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let parent_nodes = self.parent.fetch(req);
        let rel_name = &self.relationship_name;

        // Extract fields for the parallel closure so we don't capture &self
        let source = &self.source;
        let child_config = &self.child_config;
        let parent_key = &self.parent_key;
        let child_key = &self.child_key;

        let children_per_parent: Vec<Vec<Node>> = parent_nodes
            .par_iter()
            .map(|node| {
                fetch_children_for_row_static(
                    source, child_config, parent_key, child_key, &node.row,
                )
            })
            .collect();

        parent_nodes
            .into_iter()
            .zip(children_per_parent)
            .map(|(mut node, children)| {
                node.relationships.insert(rel_name.clone(), children);
                node
            })
            .collect()
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        // Push path not used during hydration
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "parallel_join"
    }
}

/// A parallel Exists operator that checks child existence in parallel across parent rows.
pub struct ParallelExistsOperator {
    input: Box<dyn Operator>,
    child_config: Vec<OperatorConfig>,
    source: Arc<RustTableSource>,
    not_exists: bool,
    parent_key: Vec<String>,
    child_key: Vec<String>,
}

impl ParallelExistsOperator {
    pub fn new(
        input: Box<dyn Operator>,
        child_config: Vec<OperatorConfig>,
        source: Arc<RustTableSource>,
        not_exists: bool,
        parent_key: Vec<String>,
        child_key: Vec<String>,
    ) -> Self {
        Self {
            input,
            child_config,
            source,
            not_exists,
            parent_key,
            child_key,
        }
    }
}

unsafe impl Send for ParallelExistsOperator {}

impl Operator for ParallelExistsOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        let parent_nodes = self.input.fetch(req);
        let not_exists = self.not_exists;

        // Extract fields for the parallel closure so we don't capture &self
        let source = &self.source;
        let child_config = &self.child_config;
        let parent_key = &self.parent_key;
        let child_key = &self.child_key;

        let counts: Vec<usize> = parent_nodes
            .par_iter()
            .map(|node| {
                fetch_child_count_static(
                    source, child_config, parent_key, child_key, &node.row,
                )
            })
            .collect();

        parent_nodes
            .into_iter()
            .zip(counts)
            .filter(|(_, count)| {
                if not_exists { *count == 0 } else { *count > 0 }
            })
            .map(|(node, _)| node)
            .collect()
    }

    fn push(&mut self, _change: Change) -> Vec<Change> {
        vec![]
    }

    fn op_type(&self) -> &'static str {
        "parallel_exists"
    }
}

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
            Ok(Box::new(ParallelJoinOperator::new(
                input,
                child.clone(),
                source,
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
            Ok(Box::new(ParallelExistsOperator::new(
                input,
                child.clone(),
                source,
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

    fn setup_join_test_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE users (
                 id TEXT PRIMARY KEY,
                 name TEXT
             );
             CREATE TABLE posts (
                 id TEXT PRIMARY KEY,
                 user_id TEXT,
                 title TEXT
             );
             INSERT INTO users VALUES ('1', 'Alice');
             INSERT INTO users VALUES ('2', 'Bob');
             INSERT INTO users VALUES ('3', 'Carol');
             INSERT INTO posts VALUES ('p1', '1', 'Post A1');
             INSERT INTO posts VALUES ('p2', '1', 'Post A2');
             INSERT INTO posts VALUES ('p3', '2', 'Post B1');
             INSERT INTO posts VALUES ('p4', '3', 'Post C1');
             INSERT INTO posts VALUES ('p5', '3', 'Post C2');
             INSERT INTO posts VALUES ('p6', '3', 'Post C3');",
        )
        .expect("setup");
        drop(conn);
        file
    }

    fn make_join_source(
        db: &tempfile::NamedTempFile,
        table: &str,
        columns: Vec<&str>,
        pk: &str,
        pool_size: usize,
    ) -> RustTableSource {
        let mut ct = HashMap::new();
        for c in &columns {
            ct.insert(c.to_string(), ColumnType::String);
        }
        RustTableSource::new(
            db.path().to_str().unwrap(),
            pool_size,
            table.into(),
            columns.iter().map(|s| s.to_string()).collect(),
            ct,
            vec![pk.into()],
        )
        .unwrap()
    }

    #[test]
    fn test_parallel_join_fetches_children() {
        let db = setup_join_test_db();
        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        // Build a pipeline: Source(users) -> ParallelJoin(posts on user_id)
        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Join {
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                relationship_name: "posts".into(),
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config);
        assert!(result.is_ok());
        let nodes = result.unwrap();
        assert_eq!(nodes.len(), 3);

        // Alice has 2 posts
        assert_eq!(nodes[0].row.get("name").unwrap(), &json!("Alice"));
        assert_eq!(nodes[0].relationships["posts"].len(), 2);

        // Bob has 1 post
        assert_eq!(nodes[1].row.get("name").unwrap(), &json!("Bob"));
        assert_eq!(nodes[1].relationships["posts"].len(), 1);

        // Carol has 3 posts
        assert_eq!(nodes[2].row.get("name").unwrap(), &json!("Carol"));
        assert_eq!(nodes[2].relationships["posts"].len(), 3);
    }

    #[test]
    fn test_parallel_join_null_key_no_children() {
        let db = setup_join_test_db();
        // Add a user with NULL-like id
        let conn = rusqlite::Connection::open(db.path()).expect("open");
        conn.execute("INSERT INTO users VALUES (NULL, 'Nobody')", []).unwrap();
        drop(conn);

        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("name".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("name".into(), "asc".into())],
            },
            OperatorConfig::Join {
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                relationship_name: "posts".into(),
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        let nobody = result.iter().find(|n| n.row.get("name").unwrap() == &json!("Nobody"));
        assert!(nobody.is_some());
        assert_eq!(nobody.unwrap().relationships["posts"].len(), 0);
    }

    #[test]
    fn test_parallel_join_multiple_pipelines() {
        let db = setup_join_test_db();
        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let join_config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Join {
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                relationship_name: "posts".into(),
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
        ];

        let configs: Vec<HydratePipelineConfig> = (0..4)
            .map(|i| HydratePipelineConfig {
                pipeline_id: format!("p{i}"),
                operator_config: join_config.clone(),
            })
            .collect();

        let results = hydrate_pipelines(arc_src, configs);
        assert_eq!(results.len(), 4);
        for r in &results {
            let nodes = r.nodes.as_ref().unwrap();
            assert_eq!(nodes.len(), 3);
            assert_eq!(nodes[0].relationships["posts"].len(), 2);
            assert_eq!(nodes[2].relationships["posts"].len(), 3);
        }
    }

    #[test]
    fn test_parallel_exists_filters_correctly() {
        let db = setup_join_test_db();
        // Remove posts for Bob so exists filter excludes him
        let conn = rusqlite::Connection::open(db.path()).expect("open");
        conn.execute("DELETE FROM posts WHERE user_id = '2'", []).unwrap();
        drop(conn);

        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        // exists: users who have posts
        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Exists {
                relationship_name: "posts".into(),
                not_exists: false,
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        // Alice and Carol have posts, Bob doesn't
        assert_eq!(result.len(), 2);
        let names: Vec<&str> = result.iter().map(|n| n.row.get("name").unwrap().as_str().unwrap()).collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Carol"));
        assert!(!names.contains(&"Bob"));
    }

    #[test]
    fn test_parallel_not_exists() {
        let db = setup_join_test_db();
        let conn = rusqlite::Connection::open(db.path()).expect("open");
        conn.execute("DELETE FROM posts WHERE user_id = '2'", []).unwrap();
        drop(conn);

        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Exists {
                relationship_name: "posts".into(),
                not_exists: true,
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                child: vec![OperatorConfig::Source {
                    table_name: "posts".into(),
                    columns: vec!["id".into(), "user_id".into(), "title".into()],
                    primary_key: vec!["id".into()],
                    sort: vec![("id".into(), "asc".into())],
                }],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].row.get("name").unwrap(), &json!("Bob"));
    }

    #[test]
    fn test_parallel_join_with_filter_on_children() {
        let db = setup_join_test_db();
        let mut src = make_join_source(&db, "users", vec!["id", "name"], "id", 8);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let arc_src = Arc::new(src);

        // Join with filter on child: only posts with title containing "A"
        let config = vec![
            OperatorConfig::Source {
                table_name: "users".into(),
                columns: vec!["id".into(), "name".into()],
                primary_key: vec!["id".into()],
                sort: vec![("id".into(), "asc".into())],
            },
            OperatorConfig::Join {
                parent_key: vec!["id".into()],
                child_key: vec!["user_id".into()],
                relationship_name: "posts".into(),
                child: vec![
                    OperatorConfig::Source {
                        table_name: "posts".into(),
                        columns: vec!["id".into(), "user_id".into(), "title".into()],
                        primary_key: vec!["id".into()],
                        sort: vec![("id".into(), "asc".into())],
                    },
                    OperatorConfig::Filter {
                        predicate: json!({"field": "title", "eq": "Post A1"}),
                    },
                ],
            },
        ];

        let result = hydrate_single_pipeline(arc_src, config).unwrap();
        assert_eq!(result.len(), 3);
        // Alice should have 1 matching post
        assert_eq!(result[0].relationships["posts"].len(), 1);
        assert_eq!(
            result[0].relationships["posts"][0].row.get("title").unwrap(),
            &json!("Post A1")
        );
        // Bob and Carol have 0 matching posts
        assert_eq!(result[1].relationships["posts"].len(), 0);
        assert_eq!(result[2].relationships["posts"].len(), 0);
    }
}
