use std::collections::{HashMap, HashSet};
use std::fmt;

use rusqlite::types::ValueRef;
use serde_json::Value;

use crate::connection_pool::{ConnectionPool, PoolError};
use crate::overlay::{compute_overlays, generate_with_overlay, generate_with_overlay_unordered};
use crate::query_builder::{build_select_query, ColumnType, Condition, Constraint, Ordering, Start};
use crate::source::{Change, FetchRequest, Node, Overlay, Row, SortDirection, SortSpec, SourceChange};

#[derive(Debug)]
pub enum TableSourceError {
    Pool(PoolError),
    Sqlite(rusqlite::Error),
    InvalidConnection(usize),
}

impl fmt::Display for TableSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableSourceError::Pool(e) => write!(f, "pool error: {e}"),
            TableSourceError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            TableSourceError::InvalidConnection(id) => {
                write!(f, "invalid connection id: {id}")
            }
        }
    }
}

impl std::error::Error for TableSourceError {}

impl From<PoolError> for TableSourceError {
    fn from(e: PoolError) -> Self {
        TableSourceError::Pool(e)
    }
}

impl From<rusqlite::Error> for TableSourceError {
    fn from(e: rusqlite::Error) -> Self {
        TableSourceError::Sqlite(e)
    }
}

type Result<T> = std::result::Result<T, TableSourceError>;

pub struct Connection {
    pub sort: Option<Vec<SortSpec>>,
    pub ordering: Option<Ordering>,
    pub filters: Option<Condition>,
    pub split_edit_keys: Option<HashSet<String>>,
    pub last_pushed_epoch: u64,
}

pub struct RustTableSource {
    pool: ConnectionPool,
    write_conn: rusqlite::Connection,
    table_name: String,
    columns: Vec<String>,
    column_types: HashMap<String, ColumnType>,
    primary_key: Vec<String>,
    connections: Vec<Connection>,
    overlay: Option<Overlay>,
    push_epoch: u64,
}

// SAFETY: RustTableSource is safe to share across threads during hydration:
// - ConnectionPool uses Arc<Mutex<Vec<Connection>>> internally
// - `connections` (Vec<Connection>) is read-only after setup (immutable borrows only)
// - `write_conn` is only used for push/overlay on the NAPI thread, never during parallel hydration
// - All other fields (table_name, columns, column_types, primary_key) are immutable
unsafe impl Send for RustTableSource {}
unsafe impl Sync for RustTableSource {}

impl RustTableSource {
    pub fn new(
        db_path: &str,
        pool_size: usize,
        table_name: String,
        columns: Vec<String>,
        column_types: HashMap<String, ColumnType>,
        primary_key: Vec<String>,
    ) -> Result<Self> {
        let pool = ConnectionPool::new(db_path, pool_size)?;
        let write_conn = rusqlite::Connection::open(db_path)?;
        write_conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        Ok(Self {
            pool,
            write_conn,
            table_name,
            columns,
            column_types,
            primary_key,
            connections: Vec::new(),
            overlay: None,
            push_epoch: 0,
        })
    }

    pub fn connect(
        &mut self,
        ordering: Option<Ordering>,
        filters: Option<Condition>,
        split_edit_keys: Option<HashSet<String>>,
    ) -> usize {
        let sort = ordering.as_ref().map(|ord| {
            ord.iter()
                .map(|(field, dir)| SortSpec {
                    field: field.clone(),
                    direction: if dir == "desc" {
                        SortDirection::Desc
                    } else {
                        SortDirection::Asc
                    },
                })
                .collect()
        });

        let conn = Connection {
            sort,
            ordering,
            filters,
            split_edit_keys,
            last_pushed_epoch: 0,
        };
        self.connections.push(conn);
        self.connections.len() - 1
    }

    pub fn fetch(&self, connection_id: usize, req: &FetchRequest) -> Result<Vec<Node>> {
        let conn_info = self
            .connections
            .get(connection_id)
            .ok_or(TableSourceError::InvalidConnection(connection_id))?;

        let constraint: Option<Constraint> = req.constraint.as_ref().map(|c| {
            let mut m = Constraint::new();
            m.insert(c.key.clone(), c.value.clone());
            m
        });

        let start: Option<Start> = req.start.as_ref().map(|s| Start {
            row: s.row.clone(),
            basis: s.basis.clone(),
        });

        let (sql, params) = build_select_query(
            &self.table_name,
            &self.columns,
            &self.column_types,
            constraint.as_ref(),
            conn_info.filters.as_ref(),
            conn_info.ordering.as_ref(),
            req.reverse,
            start.as_ref(),
        );

        let pooled = self.pool.get()?;
        let rows = execute_query(&pooled, &sql, &params)?;

        let sort = conn_info.sort.as_deref().unwrap_or(&[]);
        let start_row = req.start.as_ref().map(|s| &s.row);
        let overlays = compute_overlays(
            start_row,
            req.constraint.as_ref(),
            self.overlay.as_ref(),
            conn_info.last_pushed_epoch,
            sort,
            None,
        );

        let nodes = if conn_info.sort.is_some() {
            generate_with_overlay(rows, &overlays, sort)
        } else {
            generate_with_overlay_unordered(rows, &overlays, &self.primary_key)
        };

        Ok(nodes)
    }

    pub fn set_snapshot(&self) -> Result<()> {
        self.pool.set_snapshot()?;
        Ok(())
    }

    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    pub fn primary_key(&self) -> &[String] {
        &self.primary_key
    }

    pub fn connection(&self, id: usize) -> Option<&Connection> {
        self.connections.get(id)
    }

    pub fn push(&mut self, change: SourceChange) -> Result<Vec<Vec<Change>>> {
        self.push_epoch += 1;
        let epoch = self.push_epoch;

        let changes = self.maybe_split_edit(change);

        let mut all_results: Vec<Vec<Change>> = self.connections.iter().map(|_| Vec::new()).collect();

        let is_split = changes.len() > 1;

        for (idx, ch) in changes.iter().enumerate() {
            // Skip existence checks for split-edit pairs: the Remove deletes
            // the old row before the Add re-inserts with the new values.
            if !is_split {
                self.assert_change_valid(ch)?;
            }

            for (i, conn) in self.connections.iter_mut().enumerate() {
                conn.last_pushed_epoch = epoch;
                all_results[i].push(source_change_to_change(ch));
            }

            self.overlay = Some(Overlay {
                epoch,
                change: ch.clone(),
            });

            // Write each change immediately so subsequent assertions see
            // the updated DB state (write-after-push per individual change).
            self.write_change(ch)?;
        }

        self.overlay = None;

        Ok(all_results)
    }

    fn maybe_split_edit(&self, change: SourceChange) -> Vec<SourceChange> {
        if let SourceChange::Edit { ref row, ref old_row } = change {
            for conn in &self.connections {
                if let Some(ref keys) = conn.split_edit_keys {
                    for key in keys {
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
            }
        }
        vec![change]
    }

    fn assert_change_valid(&self, change: &SourceChange) -> Result<()> {
        match change {
            SourceChange::Add(row) => {
                if self.check_exists(row)? {
                    panic!("push Add: row already exists in DB");
                }
            }
            SourceChange::Remove(row) => {
                if !self.check_exists(row)? {
                    panic!("push Remove: row does not exist in DB");
                }
            }
            SourceChange::Edit { old_row, .. } => {
                if !self.check_exists(old_row)? {
                    panic!("push Edit: old_row does not exist in DB");
                }
            }
        }
        Ok(())
    }

    fn check_exists(&self, row: &Row) -> Result<bool> {
        let where_clause: Vec<String> = self
            .primary_key
            .iter()
            .enumerate()
            .map(|(i, k)| format!("\"{}\" = ?{}", k, i + 1))
            .collect();
        let sql = format!(
            "SELECT 1 FROM \"{}\" WHERE {} LIMIT 1",
            self.table_name,
            where_clause.join(" AND ")
        );
        let mut stmt = self.write_conn.prepare(&sql)?;
        for (i, k) in self.primary_key.iter().enumerate() {
            let val = row.get(k).unwrap_or(&serde_json::Value::Null);
            bind_json_param(&mut stmt, i + 1, val)?;
        }
        let mut rows = stmt.raw_query();
        Ok(rows.next()?.is_some())
    }

    fn write_change(&self, change: &SourceChange) -> Result<()> {
        match change {
            SourceChange::Add(row) => {
                let cols: Vec<String> =
                    self.columns.iter().map(|c| format!("\"{}\"" , c)).collect();
                let placeholders: Vec<String> =
                    (1..=self.columns.len()).map(|i| format!("?{}", i)).collect();
                let sql = format!(
                    "INSERT INTO \"{}\" ({}) VALUES ({})",
                    self.table_name,
                    cols.join(", "),
                    placeholders.join(", ")
                );
                let mut stmt = self.write_conn.prepare(&sql)?;
                for (i, col) in self.columns.iter().enumerate() {
                    let val = row.get(col).unwrap_or(&serde_json::Value::Null);
                    bind_json_param(&mut stmt, i + 1, val)?;
                }
                stmt.raw_execute()?;
            }
            SourceChange::Remove(row) => {
                let where_clause: Vec<String> = self
                    .primary_key
                    .iter()
                    .enumerate()
                    .map(|(i, k)| format!("\"{}\" = ?{}", k, i + 1))
                    .collect();
                let sql = format!(
                    "DELETE FROM \"{}\" WHERE {}",
                    self.table_name,
                    where_clause.join(" AND ")
                );
                let mut stmt = self.write_conn.prepare(&sql)?;
                for (i, k) in self.primary_key.iter().enumerate() {
                    let val = row.get(k).unwrap_or(&serde_json::Value::Null);
                    bind_json_param(&mut stmt, i + 1, val)?;
                }
                stmt.raw_execute()?;
            }
            SourceChange::Edit { row, old_row } => {
                let pk_changed = self
                    .primary_key
                    .iter()
                    .any(|k| row.get(k) != old_row.get(k));
                let non_pk_cols: Vec<&String> = self
                    .columns
                    .iter()
                    .filter(|c| !self.primary_key.contains(c))
                    .collect();

                if !pk_changed && !non_pk_cols.is_empty() {
                    let mut param_idx = 1usize;
                    let set_clause: Vec<String> = non_pk_cols
                        .iter()
                        .map(|c| {
                            let s = format!("\"{}\" = ?{}", c, param_idx);
                            param_idx += 1;
                            s
                        })
                        .collect();
                    let where_clause: Vec<String> = self
                        .primary_key
                        .iter()
                        .map(|k| {
                            let s = format!("\"{}\" = ?{}", k, param_idx);
                            param_idx += 1;
                            s
                        })
                        .collect();
                    let sql = format!(
                        "UPDATE \"{}\" SET {} WHERE {}",
                        self.table_name,
                        set_clause.join(", "),
                        where_clause.join(" AND ")
                    );
                    let mut stmt = self.write_conn.prepare(&sql)?;
                    let mut idx = 1;
                    for c in &non_pk_cols {
                        let val = row.get(c.as_str()).unwrap_or(&serde_json::Value::Null);
                        bind_json_param(&mut stmt, idx, val)?;
                        idx += 1;
                    }
                    for k in &self.primary_key {
                        let val = old_row.get(k).unwrap_or(&serde_json::Value::Null);
                        bind_json_param(&mut stmt, idx, val)?;
                        idx += 1;
                    }
                    stmt.raw_execute()?;
                } else {
                    self.write_change(&SourceChange::Remove(old_row.clone()))?;
                    self.write_change(&SourceChange::Add(row.clone()))?;
                }
            }
        }
        Ok(())
    }
}

fn source_change_to_change(change: &SourceChange) -> Change {
    let make_node = |row: &Row| Node {
        row: row.clone(),
        relationships: HashMap::new(),
    };
    match change {
        SourceChange::Add(row) => Change::Add(make_node(row)),
        SourceChange::Remove(row) => Change::Remove(make_node(row)),
        SourceChange::Edit { row, old_row } => Change::Edit {
            node: make_node(row),
            old_node: make_node(old_row),
        },
    }
}

fn bind_json_param(
    stmt: &mut rusqlite::Statement<'_>,
    idx: usize,
    val: &Value,
) -> std::result::Result<(), rusqlite::Error> {
    match val {
        Value::Null => stmt.raw_bind_parameter(idx, rusqlite::types::Null)?,
        Value::Bool(b) => stmt.raw_bind_parameter(idx, *b)?,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                stmt.raw_bind_parameter(idx, i)?;
            } else {
                stmt.raw_bind_parameter(idx, n.as_f64().unwrap_or(0.0))?;
            }
        }
        Value::String(s) => stmt.raw_bind_parameter(idx, s.as_str())?,
        _ => stmt.raw_bind_parameter(idx, serde_json::to_string(val).unwrap_or_default())?,
    }
    Ok(())
}

fn execute_query(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[Value],
) -> std::result::Result<Vec<Row>, rusqlite::Error> {
    let mut stmt = conn.prepare(sql)?;

    for (i, val) in params.iter().enumerate() {
        bind_json_param(&mut stmt, i + 1, val)?;
    }

    let col_count = stmt.column_count();
    let col_names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("").to_string())
        .collect();

    let mut rows: Vec<Row> = Vec::new();
    let mut raw_rows = stmt.raw_query();
    while let Some(raw_row) = raw_rows.next()? {
        let mut row = Row::new();
        for (i, name) in col_names.iter().enumerate() {
            let val = match raw_row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => Value::Number(n.into()),
                ValueRef::Real(f) => {
                    Value::Number(serde_json::Number::from_f64(f).unwrap_or_else(|| 0.into()))
                }
                ValueRef::Text(bytes) => {
                    Value::String(String::from_utf8_lossy(bytes).into_owned())
                }
                ValueRef::Blob(_) => Value::Null,
            };
            row.insert(name.clone(), val);
        }
        rows.push(row);
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection as SqliteConn;
    use serde_json::json;

    fn setup_test_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = SqliteConn::open(file.path()).expect("open");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE users (
                 id TEXT PRIMARY KEY,
                 name TEXT,
                 age INTEGER
             );
             INSERT INTO users VALUES ('1', 'Alice', 30);
             INSERT INTO users VALUES ('2', 'Bob', 25);
             INSERT INTO users VALUES ('3', 'Carol', 35);",
        )
        .expect("setup");
        drop(conn);
        file
    }

    fn make_source(db: &tempfile::NamedTempFile) -> RustTableSource {
        let mut ct = HashMap::new();
        ct.insert("id".into(), ColumnType::String);
        ct.insert("name".into(), ColumnType::String);
        ct.insert("age".into(), ColumnType::Number);

        RustTableSource::new(
            db.path().to_str().unwrap(),
            2,
            "users".into(),
            vec!["id".into(), "name".into(), "age".into()],
            ct,
            vec!["id".into()],
        )
        .unwrap()
    }

    #[test]
    fn test_table_source_basic_fetch() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].row.get("name").unwrap(), &json!("Alice"));
        assert_eq!(nodes[1].row.get("name").unwrap(), &json!("Bob"));
        assert_eq!(nodes[2].row.get("name").unwrap(), &json!("Carol"));
    }

    #[test]
    fn test_table_source_fetch_with_constraint() {
        use crate::source::FetchConstraint;

        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(None, None, None);
        let req = FetchRequest {
            constraint: Some(FetchConstraint {
                key: "id".into(),
                value: json!("2"),
            }),
            start: None,
            reverse: false,
        };
        let nodes = src.fetch(cid, &req).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].row.get("name").unwrap(), &json!("Bob"));
    }

    #[test]
    fn test_table_source_fetch_with_ordering() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("age".into(), "asc".into())]), None, None);
        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        assert_eq!(nodes[0].row.get("age").unwrap(), &json!(25));
        assert_eq!(nodes[1].row.get("age").unwrap(), &json!(30));
        assert_eq!(nodes[2].row.get("age").unwrap(), &json!(35));

        let cid2 = src.connect(Some(vec![("age".into(), "desc".into())]), None, None);
        let nodes = src.fetch(cid2, &FetchRequest::default()).unwrap();
        assert_eq!(nodes[0].row.get("age").unwrap(), &json!(35));
        assert_eq!(nodes[2].row.get("age").unwrap(), &json!(25));
    }

    #[test]
    fn test_table_source_fetch_reverse() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let req = FetchRequest {
            constraint: None,
            start: None,
            reverse: true,
        };
        let nodes = src.fetch(cid, &req).unwrap();
        assert_eq!(nodes[0].row.get("id").unwrap(), &json!("3"));
        assert_eq!(nodes[2].row.get("id").unwrap(), &json!("1"));
    }

    #[test]
    fn test_table_source_fetch_with_start() {
        use crate::source::FetchStart;

        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        let mut start_row = Row::new();
        start_row.insert("id".into(), json!("1"));
        let req = FetchRequest {
            constraint: None,
            start: Some(FetchStart {
                row: start_row,
                basis: "after".into(),
            }),
            reverse: false,
        };
        let nodes = src.fetch(cid, &req).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].row.get("id").unwrap(), &json!("2"));
        assert_eq!(nodes[1].row.get("id").unwrap(), &json!("3"));
    }

    #[test]
    fn test_table_source_multiple_connections() {
        let db = setup_test_db();
        let mut src = make_source(&db);

        let c1 = src.connect(Some(vec![("name".into(), "asc".into())]), None, None);
        let c2 = src.connect(Some(vec![("age".into(), "desc".into())]), None, None);

        let n1 = src.fetch(c1, &FetchRequest::default()).unwrap();
        let n2 = src.fetch(c2, &FetchRequest::default()).unwrap();

        assert_eq!(n1[0].row.get("name").unwrap(), &json!("Alice"));
        assert_eq!(n1[2].row.get("name").unwrap(), &json!("Carol"));

        assert_eq!(n2[0].row.get("age").unwrap(), &json!(35));
        assert_eq!(n2[2].row.get("age").unwrap(), &json!(25));
    }

    #[test]
    fn test_invalid_connection() {
        let db = setup_test_db();
        let src = make_source(&db);
        let result = src.fetch(99, &FetchRequest::default());
        assert!(result.is_err());
        match result.unwrap_err() {
            TableSourceError::InvalidConnection(99) => {}
            other => panic!("expected InvalidConnection, got {other}"),
        }
    }

    fn make_row(pairs: &[(&str, Value)]) -> Row {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn test_push_add() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        let row = make_row(&[("id", json!("4")), ("name", json!("Dave")), ("age", json!(40))]);
        let results = src.push(SourceChange::Add(row)).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].len(), 1);
        match &results[0][0] {
            Change::Add(node) => {
                assert_eq!(node.row.get("name").unwrap(), &json!("Dave"));
            }
            other => panic!("expected Add, got {:?}", other),
        }

        // Verify written to DB
        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[3].row.get("name").unwrap(), &json!("Dave"));
    }

    #[test]
    fn test_push_remove() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        let row = make_row(&[("id", json!("2")), ("name", json!("Bob")), ("age", json!(25))]);
        let results = src.push(SourceChange::Remove(row)).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].len(), 1);
        match &results[0][0] {
            Change::Remove(node) => {
                assert_eq!(node.row.get("name").unwrap(), &json!("Bob"));
            }
            other => panic!("expected Remove, got {:?}", other),
        }

        // Verify deleted from DB
        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|n| n.row.get("id").unwrap() != &json!("2")));
    }

    #[test]
    fn test_push_edit() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        let old_row = make_row(&[("id", json!("1")), ("name", json!("Alice")), ("age", json!(30))]);
        let new_row = make_row(&[("id", json!("1")), ("name", json!("Alicia")), ("age", json!(31))]);
        let results = src
            .push(SourceChange::Edit {
                row: new_row,
                old_row,
            })
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].len(), 1);
        match &results[0][0] {
            Change::Edit { node, old_node } => {
                assert_eq!(node.row.get("name").unwrap(), &json!("Alicia"));
                assert_eq!(old_node.row.get("name").unwrap(), &json!("Alice"));
            }
            other => panic!("expected Edit, got {:?}", other),
        }

        // Verify updated in DB
        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        let alice = nodes.iter().find(|n| n.row.get("id").unwrap() == &json!("1")).unwrap();
        assert_eq!(alice.row.get("name").unwrap(), &json!("Alicia"));
        assert_eq!(alice.row.get("age").unwrap(), &json!(31));
    }

    #[test]
    fn test_push_split_edit() {
        let db = setup_test_db();
        let mut src = make_source(&db);

        let mut split_keys = HashSet::new();
        split_keys.insert("name".to_string());
        let cid = src.connect(
            Some(vec![("id".into(), "asc".into())]),
            None,
            Some(split_keys),
        );

        let old_row = make_row(&[("id", json!("1")), ("name", json!("Alice")), ("age", json!(30))]);
        let new_row = make_row(&[("id", json!("1")), ("name", json!("Alicia")), ("age", json!(31))]);
        let results = src
            .push(SourceChange::Edit {
                row: new_row,
                old_row,
            })
            .unwrap();

        // Split edit produces Remove + Add
        assert_eq!(results[0].len(), 2);
        match &results[0][0] {
            Change::Remove(node) => {
                assert_eq!(node.row.get("name").unwrap(), &json!("Alice"));
            }
            other => panic!("expected Remove, got {:?}", other),
        }
        match &results[0][1] {
            Change::Add(node) => {
                assert_eq!(node.row.get("name").unwrap(), &json!("Alicia"));
            }
            other => panic!("expected Add, got {:?}", other),
        }

        // Verify DB state
        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        assert_eq!(nodes.len(), 3);
        let updated = nodes.iter().find(|n| n.row.get("id").unwrap() == &json!("1")).unwrap();
        assert_eq!(updated.row.get("name").unwrap(), &json!("Alicia"));
    }

    #[test]
    fn test_push_add_then_fetch() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("age".into(), "asc".into())]), None, None);

        let row = make_row(&[("id", json!("4")), ("name", json!("Dave")), ("age", json!(10))]);
        src.push(SourceChange::Add(row)).unwrap();

        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        assert_eq!(nodes.len(), 4);
        // Dave has age 10, should be first in asc order
        assert_eq!(nodes[0].row.get("name").unwrap(), &json!("Dave"));
    }

    #[test]
    fn test_push_epoch_increments() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        assert_eq!(src.push_epoch, 0);

        let row1 = make_row(&[("id", json!("4")), ("name", json!("Dave")), ("age", json!(40))]);
        src.push(SourceChange::Add(row1)).unwrap();
        assert_eq!(src.push_epoch, 1);
        assert_eq!(src.connections[cid].last_pushed_epoch, 1);

        let row2 = make_row(&[("id", json!("5")), ("name", json!("Eve")), ("age", json!(28))]);
        src.push(SourceChange::Add(row2)).unwrap();
        assert_eq!(src.push_epoch, 2);
        assert_eq!(src.connections[cid].last_pushed_epoch, 2);
    }

    #[test]
    fn test_push_multiple_connections() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let _c1 = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);
        let _c2 = src.connect(Some(vec![("age".into(), "desc".into())]), None, None);

        let row = make_row(&[("id", json!("4")), ("name", json!("Dave")), ("age", json!(40))]);
        let results = src.push(SourceChange::Add(row)).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].len(), 1);
        assert_eq!(results[1].len(), 1);
        match (&results[0][0], &results[1][0]) {
            (Change::Add(n1), Change::Add(n2)) => {
                assert_eq!(n1.row.get("name").unwrap(), &json!("Dave"));
                assert_eq!(n2.row.get("name").unwrap(), &json!("Dave"));
            }
            _ => panic!("expected Add changes for both connections"),
        }
    }

    #[test]
    #[should_panic(expected = "push Add: row already exists")]
    fn test_push_add_duplicate_panics() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        let row = make_row(&[("id", json!("1")), ("name", json!("Alice")), ("age", json!(30))]);
        src.push(SourceChange::Add(row)).unwrap();
    }

    #[test]
    #[should_panic(expected = "push Remove: row does not exist")]
    fn test_push_remove_missing_panics() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        let row = make_row(&[("id", json!("99")), ("name", json!("Nobody")), ("age", json!(0))]);
        src.push(SourceChange::Remove(row)).unwrap();
    }

    #[test]
    fn test_push_edit_pk_change() {
        let db = setup_test_db();
        let mut src = make_source(&db);
        let cid = src.connect(Some(vec![("id".into(), "asc".into())]), None, None);

        let old_row = make_row(&[("id", json!("1")), ("name", json!("Alice")), ("age", json!(30))]);
        let new_row = make_row(&[("id", json!("10")), ("name", json!("Alice")), ("age", json!(30))]);
        src.push(SourceChange::Edit {
            row: new_row,
            old_row,
        })
        .unwrap();

        let nodes = src.fetch(cid, &FetchRequest::default()).unwrap();
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().any(|n| n.row.get("id").unwrap() == &json!("10")));
        assert!(nodes.iter().all(|n| n.row.get("id").unwrap() != &json!("1")));
    }

    #[test]
    fn test_push_no_split_when_key_unchanged() {
        let db = setup_test_db();
        let mut src = make_source(&db);

        let mut split_keys = HashSet::new();
        split_keys.insert("name".to_string());
        src.connect(
            Some(vec![("id".into(), "asc".into())]),
            None,
            Some(split_keys),
        );

        // Edit that does NOT change name should not split
        let old_row = make_row(&[("id", json!("1")), ("name", json!("Alice")), ("age", json!(30))]);
        let new_row = make_row(&[("id", json!("1")), ("name", json!("Alice")), ("age", json!(31))]);
        let results = src
            .push(SourceChange::Edit {
                row: new_row,
                old_row,
            })
            .unwrap();

        // Should be a single Edit, not split
        assert_eq!(results[0].len(), 1);
        match &results[0][0] {
            Change::Edit { .. } => {}
            other => panic!("expected Edit, got {:?}", other),
        }
    }
}
