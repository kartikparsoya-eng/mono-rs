use std::cell::RefCell;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::{Env, JsObject, NapiRaw, NapiValue};
use napi_derive::napi;
use rusqlite::Connection;

use crate::row_iterator::RowIterator;
use crate::types::{get_column_names, intern_column_keys, js_array_params_to_sqlite, row_to_js_object, row_to_js_object_fast};

#[napi]
pub struct Statement {
    conn: Arc<RefCell<Connection>>,
    sql: String,
    safe_integers: bool,
    cached_columns: RefCell<Option<Vec<String>>>,
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
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;
        let values = extract_params(&env, &params, &stmt)?;

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
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;
        let values = extract_params(&env, &params, &stmt)?;

        let columns = self.get_columns(&stmt);
        let interned_keys = intern_column_keys(&env, &columns)?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut rows = stmt
            .query(params_ref.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        match rows.next().map_err(|e| Error::from_reason(format!("{e}")))? {
            Some(row) => {
                let raw = row_to_js_object_fast(&env, row, &interned_keys, self.safe_integers)?;
                Ok(Some(unsafe { JsObject::from_raw_unchecked(env.raw(), raw) }))
            }
            None => Ok(None),
        }
    }

    /// Get all rows as an array of objects.
    #[napi]
    pub fn all(&self, env: Env, params: JsObject) -> Result<JsObject> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;
        let values = extract_params(&env, &params, &stmt)?;

        let columns = self.get_columns(&stmt);
        let interned_keys = intern_column_keys(&env, &columns)?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut rows = stmt
            .query(params_ref.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        // Build JS array directly using raw napi for minimal overhead
        let mut result_arr = std::ptr::null_mut();
        unsafe { napi::sys::napi_create_array(env.raw(), &mut result_arr) };

        let mut idx: u32 = 0;
        while let Some(row) = rows.next().map_err(|e| Error::from_reason(format!("{e}")))? {
            let obj = row_to_js_object_fast(&env, row, &interned_keys, self.safe_integers)?;
            unsafe { napi::sys::napi_set_element(env.raw(), result_arr, idx, obj) };
            idx += 1;
        }

        Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), result_arr) })
    }

    /// Get all rows as a binary buffer for fast JS decoding.
    /// Protocol: [u32 row_count][u16 col_count] then per cell: [u8 tag][payload]
    /// Tags: 0=null, 1=i64(8 bytes LE), 2=f64(8 bytes LE), 3=text(u32 len + bytes), 4=blob(u32 len + bytes)
    #[napi]
    pub fn all_buf(&self, env: Env, params: JsObject) -> Result<Buffer> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;
        let values = extract_params(&env, &params, &stmt)?;

        let col_count = stmt.column_count();
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut rows = stmt
            .query(params_ref.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;

        // Pre-allocate buffer (estimate 64 bytes per cell)
        let mut buf: Vec<u8> = Vec::with_capacity(col_count * 64 * 32);
        // Reserve space for header (row_count u32 + col_count u16)
        buf.extend_from_slice(&[0u8; 6]);

        let mut row_count: u32 = 0;
        while let Some(row) = rows.next().map_err(|e| Error::from_reason(format!("{e}")))? {
            for i in 0..col_count {
                let val = row.get_ref(i).map_err(|e| Error::from_reason(format!("{e}")))?;
                match val {
                    rusqlite::types::ValueRef::Null => buf.push(0),
                    rusqlite::types::ValueRef::Integer(n) => {
                        buf.push(1);
                        buf.extend_from_slice(&n.to_le_bytes());
                    }
                    rusqlite::types::ValueRef::Real(f) => {
                        buf.push(2);
                        buf.extend_from_slice(&f.to_le_bytes());
                    }
                    rusqlite::types::ValueRef::Text(s) => {
                        buf.push(3);
                        buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                        buf.extend_from_slice(s);
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        buf.push(4);
                        buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
                        buf.extend_from_slice(b);
                    }
                }
            }
            row_count += 1;
        }

        // Write header
        buf[0..4].copy_from_slice(&row_count.to_le_bytes());
        buf[4..6].copy_from_slice(&(col_count as u16).to_le_bytes());

        Ok(Buffer::from(buf))
    }

    /// Create a lazy row iterator. Materializes all rows in Rust, converts lazily to JS.
    #[napi]
    pub fn iterate(&self, env: Env, params: JsObject) -> Result<RowIterator> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare_cached(&self.sql)
            .map_err(|e| Error::from_reason(format!("{e}: {}", self.sql)))?;
        let values = extract_params(&env, &params, &stmt)?;

        let columns = self.get_columns(&stmt);
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
fn extract_params(env: &Env, params: &JsObject, stmt: &rusqlite::CachedStatement) -> Result<Vec<rusqlite::types::Value>> {
    let len = params.get_array_length()?;
    if len == 0 {
        return Ok(Vec::new());
    }
    // better-sqlite3 compat: if the single argument is itself an array, flatten it.
    // This handles stmt.run([a, b, c]) being called as run(...params) in TS,
    // resulting in params = [[a, b, c]].
    if len == 1 {
        let mut first_element = std::ptr::null_mut();
        let status = unsafe {
            napi::sys::napi_get_element(env.raw(), params.raw(), 0, &mut first_element)
        };
        if status == 0 {
            // Check if it's an array
            let mut is_array = false;
            let arr_status = unsafe {
                napi::sys::napi_is_array(env.raw(), first_element, &mut is_array)
            };
            if arr_status == 0 && is_array {
                let mut inner_len: u32 = 0;
                let len_status = unsafe {
                    napi::sys::napi_get_array_length(env.raw(), first_element, &mut inner_len)
                };
                if len_status == 0 {
                    return js_array_params_to_sqlite(env, first_element, inner_len);
                }
            }

            // Check if it's a named-params object (not array, not buffer, not null)
            let mut vt = 0;
            unsafe { napi::sys::napi_typeof(env.raw(), first_element, &mut vt) };
            if vt == 6 {
                // napi_object
                let mut is_buffer = false;
                unsafe { napi::sys::napi_is_buffer(env.raw(), first_element, &mut is_buffer) };
                if !is_buffer {
                    return extract_named_params(env, first_element, stmt);
                }
            }
        }
    }
    js_array_params_to_sqlite(env, unsafe { params.raw() }, len)
}

/// Extract named parameters from a JS object using the statement's parameter names.
/// better-sqlite3 supports @name, :name, $name prefixes.
fn extract_named_params(
    env: &Env,
    obj_raw: napi::sys::napi_value,
    stmt: &rusqlite::CachedStatement,
) -> Result<Vec<rusqlite::types::Value>> {
    use crate::types::napi_value_to_sqlite_param;

    let param_count = stmt.parameter_count();
    let mut values = Vec::with_capacity(param_count);

    for i in 1..=param_count {
        let name = stmt.parameter_name(i).ok_or_else(|| {
            Error::from_reason(format!("Parameter at index {i} has no name"))
        })?;
        // Strip the prefix (@, :, $) to get the object key
        let key = &name[1..];

        // Get property from JS object
        let mut prop_val = std::ptr::null_mut();
        let c_key = std::ffi::CString::new(key)
            .map_err(|_| Error::from_reason(format!("Invalid key: {key}")))?;
        let status = unsafe {
            napi::sys::napi_get_named_property(
                env.raw(),
                obj_raw,
                c_key.as_ptr(),
                &mut prop_val,
            )
        };
        if status != 0 {
            return Err(Error::from_reason(format!(
                "Failed to get property '{key}' from named params object"
            )));
        }

        values.push(napi_value_to_sqlite_param(env, prop_val)?);
    }

    Ok(values)
}

impl Statement {
    /// Internal constructor (called from Database::prepare)
    pub(crate) fn new_internal(conn: Arc<RefCell<Connection>>, sql: String) -> Self {
        Self {
            conn,
            sql,
            safe_integers: false,
            cached_columns: RefCell::new(None),
        }
    }

    /// Get or compute cached column names.
    fn get_columns(&self, stmt: &rusqlite::Statement) -> Vec<String> {
        let mut cache = self.cached_columns.borrow_mut();
        if let Some(cols) = cache.as_ref() {
            cols.clone()
        } else {
            let cols = get_column_names(stmt);
            *cache = Some(cols.clone());
            cols
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
