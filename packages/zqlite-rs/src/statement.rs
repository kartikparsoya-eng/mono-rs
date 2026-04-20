use std::cell::RefCell;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::{Env, JsObject, NapiRaw};
use napi_derive::napi;
use rusqlite::Connection;

use crate::row_iterator::RowIterator;
use crate::types::{get_column_names, js_array_params_to_sqlite, row_to_js_object};

#[napi]
pub struct Statement {
    conn: Arc<RefCell<Connection>>,
    sql: String,
    safe_integers: bool,
}

#[napi]
impl Statement {
    /// Toggle safe integers mode (returns bigint for INTEGER columns when true).
    #[napi]
    pub fn safe_integers(&mut self, use_big_int: bool) -> Result<()> {
        self.safe_integers = use_big_int;
        Ok(())
    }

    /// Execute an INSERT/UPDATE/DELETE statement.
    /// Returns {changes: number, lastInsertRowid: number|bigint}.
    #[napi]
    pub fn run(&self, env: Env, params: JsObject) -> Result<JsObject> {
        let values = extract_params(&env, &params)?;
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        // Use query + drain to match better-sqlite3 behavior (run() works on SELECT too)
        match stmt.execute(params_ref.as_slice()) {
            Ok(_) => {}
            Err(rusqlite::Error::ExecuteReturnedResults) => {
                // SELECT statements — just drain the rows
                let mut rows = stmt
                    .query(params_ref.as_slice())
                    .map_err(|e| Error::from_reason(format!("{e}")))?;
                while let Some(_) = rows.next().map_err(|e| Error::from_reason(format!("{e}")))? {}
            }
            Err(e) => return Err(Error::from_reason(format!("{e}"))),
        }

        let changes = conn.changes() as f64;
        let last_insert_rowid = conn.last_insert_rowid();

        let mut result = env.create_object()?;
        result.set_named_property("changes", changes)?;

        if self.safe_integers {
            let bigint = env.create_bigint_from_i64(last_insert_rowid)?;
            result.set_named_property("lastInsertRowid", bigint)?;
        } else {
            result.set_named_property("lastInsertRowid", last_insert_rowid as f64)?;
        }

        Ok(result)
    }

    /// Get a single row. Returns the row object or undefined if no rows.
    #[napi]
    pub fn get(&self, env: Env, params: JsObject) -> Result<Option<JsObject>> {
        let values = extract_params(&env, &params)?;
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        let columns = get_column_names(&stmt);
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut rows = stmt
            .query(params_ref.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        match rows.next().map_err(|e| Error::from_reason(format!("{e}")))? {
            Some(row) => {
                let obj = row_to_js_object(&env, row, &columns, self.safe_integers)?;
                Ok(Some(obj))
            }
            None => Ok(None),
        }
    }

    /// Get all rows as an array of objects.
    #[napi]
    pub fn all(&self, env: Env, params: JsObject) -> Result<Vec<JsObject>> {
        let values = extract_params(&env, &params)?;
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        let columns = get_column_names(&stmt);
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut rows = stmt
            .query(params_ref.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(|e| Error::from_reason(format!("{e}")))? {
            let obj = row_to_js_object(&env, row, &columns, self.safe_integers)?;
            result.push(obj);
        }

        Ok(result)
    }

    /// Create a lazy row iterator. Materializes all rows in Rust, converts lazily to JS.
    #[napi]
    pub fn iterate(&self, env: Env, params: JsObject) -> Result<RowIterator> {
        let values = extract_params(&env, &params)?;
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        let columns = get_column_names(&stmt);
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut rows = stmt
            .query(params_ref.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        let mut all_rows = Vec::new();
        while let Some(row) = rows.next().map_err(|e| Error::from_reason(format!("{e}")))? {
            let mut row_values = Vec::with_capacity(columns.len());
            for i in 0..columns.len() {
                let val: rusqlite::types::Value = row
                    .get(i)
                    .map_err(|e| Error::from_reason(format!("{e}")))?;
                row_values.push(val);
            }
            all_rows.push(row_values);
        }

        Ok(RowIterator::new_internal(
            all_rows,
            columns,
            self.safe_integers,
        ))
    }
}

/// Extract params from a JsObject (JS Array) into rusqlite Values.
fn extract_params(env: &Env, params: &JsObject) -> Result<Vec<rusqlite::types::Value>> {
    let len = params.get_array_length()?;
    if len == 0 {
        return Ok(Vec::new());
    }
    js_array_params_to_sqlite(env, unsafe { params.raw() }, len)
}

impl Statement {
    /// Internal constructor (called from Database::prepare)
    pub(crate) fn new_internal(conn: Arc<RefCell<Connection>>, sql: String) -> Self {
        Self {
            conn,
            sql,
            safe_integers: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn test_run_returns_changes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        let changes = conn
            .execute("INSERT INTO test (id, name) VALUES (1, 'hello')", [])
            .unwrap();
        assert_eq!(changes, 1);
    }

    #[test]
    fn test_get_returns_single_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        conn.execute("INSERT INTO test (id, name) VALUES (1, 'hello')", [])
            .unwrap();
        let name: String = conn
            .query_row("SELECT name FROM test WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "hello");
    }

    #[test]
    fn test_get_returns_none_for_no_match() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        let result = conn.query_row("SELECT name FROM test WHERE id = 999", [], |row| {
            row.get::<_, String>(0)
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_all_returns_multiple_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO test VALUES (1, 'a');
             INSERT INTO test VALUES (2, 'b');
             INSERT INTO test VALUES (3, 'c');",
        )
        .unwrap();
        let mut stmt = conn.prepare("SELECT * FROM test ORDER BY id").unwrap();
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], (1, "a".to_string()));
    }

    #[test]
    fn test_null_values() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id INTEGER, name TEXT);
             INSERT INTO test VALUES (1, NULL);",
        )
        .unwrap();
        let val: rusqlite::types::Value = conn
            .query_row("SELECT name FROM test WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert!(matches!(val, rusqlite::types::Value::Null));
    }

    #[test]
    fn test_parameter_binding() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER, name TEXT)")
            .unwrap();
        conn.execute(
            "INSERT INTO test VALUES (?1, ?2)",
            rusqlite::params![42, "hello"],
        )
        .unwrap();
        let name: String = conn
            .query_row("SELECT name FROM test WHERE id = ?1", [42], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(name, "hello");
    }

    #[test]
    fn test_prepare_cached() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER)").unwrap();
        {
            let mut stmt = conn.prepare_cached("INSERT INTO test VALUES (?1)").unwrap();
            stmt.execute([1]).unwrap();
        }
        {
            let mut stmt = conn.prepare_cached("INSERT INTO test VALUES (?1)").unwrap();
            stmt.execute([2]).unwrap();
        }
        let count: i64 = conn
            .query_row("SELECT count(*) FROM test", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_error_on_wrong_param_count() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER, name TEXT)")
            .unwrap();
        let mut stmt = conn.prepare("INSERT INTO test VALUES (?1, ?2)").unwrap();
        let result = stmt.execute([1]);
        assert!(result.is_err());
    }
}
