use std::cell::RefCell;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::{Env, JsObject, NapiRaw, NapiValue};
use napi_derive::napi;
use rusqlite::{Connection, OpenFlags};

use crate::statement::Statement;
use crate::types::{get_column_names, set_sqlite_value, parse_column_types, row_to_typed_js_object, intern_column_keys, resolve_column_types};

#[napi(object)]
#[derive(Default)]
pub struct DatabaseOptions {
    pub readonly: Option<bool>,
    pub file_must_exist: Option<bool>,
}

#[napi]
pub struct Database {
    conn: Arc<RefCell<Connection>>,
    path: String,
    page_size: i64,
    is_readonly: bool,
}

#[napi]
impl Database {
    #[napi(constructor)]
    pub fn new(path: String, options: Option<DatabaseOptions>) -> Result<Self> {
        let opts = options.unwrap_or_default();
        let readonly = opts.readonly.unwrap_or(false);
        let file_must_exist = opts.file_must_exist.unwrap_or(false);

        let mut flags = if readonly {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };

        if !file_must_exist && !readonly {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }

        flags |= OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;

        let conn = if path == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open_with_flags(&path, flags)
        }
        .map_err(|e| Error::from_reason(format!("{e}")))?;

        let page_size: i64 = conn
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .map_err(|e| Error::from_reason(format!("Failed to read page_size: {e}")))?;

        Ok(Self {
            conn: Arc::new(RefCell::new(conn)),
            path,
            page_size,
            is_readonly: readonly,
        })
    }

    #[napi]
    pub fn exec(&self, sql: String) -> Result<()> {
        self.conn
            .borrow()
            .execute_batch(&sql)
            .map_err(|e| Error::from_reason(format!("{e}")))
    }

    #[napi]
    pub fn pragma(&self, env: Env, sql: String) -> Result<Vec<JsObject>> {
        let conn = self.conn.borrow();
        let full_sql = format!("PRAGMA {sql}");
        let mut stmt = conn
            .prepare(&full_sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let columns = get_column_names(&stmt);
        let mut rows_out = Vec::new();

        let rows = stmt
            .query_map([], |row| {
                Ok((0..columns.len())
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<std::result::Result<Vec<_>, _>>())
            })
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        for row_result in rows {
            let values = row_result
                .map_err(|e| Error::from_reason(format!("{e}")))?
                .map_err(|e| Error::from_reason(format!("{e}")))?;

            let mut obj = env.create_object()?;
            for (i, col) in columns.iter().enumerate() {
                set_sqlite_value(&env, &mut obj, col, values[i].to_value_ref(), false)?;
            }
            rows_out.push(obj);
        }

        Ok(rows_out)
    }

    #[napi]
    pub fn prepare(&self, sql: String) -> Result<Statement> {
        {
            let conn = self.conn.borrow();
            conn.prepare(&sql)
                .map_err(|e| Error::from_reason(format!("{e}")))?;
        }
        Ok(Statement::new_internal(self.conn.clone(), sql))
    }

    #[napi]
    pub fn close(&self) -> Result<()> {
        // Optimize is handled by the TS wrapper for logging purposes
        Ok(())
    }

    /// Transaction wraps BEGIN/COMMIT/ROLLBACK around a callback.
    /// Uses raw napi sys to call the JS function since Function<'_> has lifetime issues.
    #[napi]
    pub fn transaction(&self, env: Env, #[napi(ts_arg_type = "() => any")] callback: JsObject) -> Result<JsObject> {
        let conn = self.conn.borrow();
        conn.execute_batch("BEGIN")
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        // Call the JS function with no arguments using raw napi sys
        let mut result_raw = std::ptr::null_mut();
        let status = unsafe {
            napi::sys::napi_call_function(
                env.raw(),
                std::ptr::null_mut(), // undefined as `this`
                callback.raw(),
                0,
                std::ptr::null(),
                &mut result_raw,
            )
        };

        if status == 0 {
            // Success — commit
            conn.execute_batch("COMMIT")
                .map_err(|e| Error::from_reason(format!("{e}")))?;
            Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), result_raw) })
        } else {
            // Error — rollback
            let _ = conn.execute_batch("ROLLBACK");
            // Re-throw the JS exception
            Err(Error::from_reason("Transaction callback threw an exception"))
        }
    }

    #[napi]
    pub fn compact(&self, freeable_bytes_threshold: f64) -> Result<()> {
        let conn = self.conn.borrow();

        let freelist_count: i64 = conn
            .pragma_query_value(None, "freelist_count", |row| row.get(0))
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let freeable = freelist_count * self.page_size;

        if (freeable as f64) < freeable_bytes_threshold {
            return Err(Error::from_reason("SKIP_COMPACT:not_enough_freeable"));
        }

        let auto_vacuum: i64 = conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        if auto_vacuum != 2 {
            return Err(Error::from_reason(format!(
                "SKIP_COMPACT:wrong_auto_vacuum:{auto_vacuum}"
            )));
        }

        let page_count_before: i64 = conn
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        conn.execute_batch("PRAGMA incremental_vacuum")
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let page_count_after: i64 = conn
            .pragma_query_value(None, "page_count", |row| row.get(0))
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        Err(Error::from_reason(format!(
            "COMPACTED:{}:{}:{}",
            page_count_before * self.page_size,
            page_count_after * self.page_size,
            page_count_before - page_count_after
        )))
    }

    #[napi]
    pub fn unsafe_mode(&self, unsafe_val: bool) -> Result<()> {
        let conn = self.conn.borrow();
        if unsafe_val {
            conn.execute_batch(
                "PRAGMA synchronous = OFF; PRAGMA journal_mode = OFF; PRAGMA trusted_schema = ON",
            )
            .map_err(|e| Error::from_reason(format!("{e}")))?;
        } else {
            conn.execute_batch(
                "PRAGMA synchronous = NORMAL; PRAGMA journal_mode = WAL; PRAGMA trusted_schema = OFF",
            )
            .map_err(|e| Error::from_reason(format!("{e}")))?;
        }
        Ok(())
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.path.clone()
    }

    #[napi(getter)]
    pub fn in_transaction(&self) -> bool {
        !self.conn.borrow().is_autocommit()
    }

    #[napi(getter)]
    pub fn readonly(&self) -> bool {
        self.is_readonly
    }

    /// Execute a query and return all rows as a JS array of objects,
    /// with column-type-aware conversion done entirely in Rust.
    #[napi]
    pub fn query_all(
        &self,
        env: Env,
        sql: String,
        params: JsObject,
        column_types: JsObject,
        table_name: String,
    ) -> Result<JsObject> {
        let conn = self.conn.borrow();
        let col_types = parse_column_types(&env, &column_types)?;

        // Get params array length
        let params_len = params.get_array_length()?;
        let sqlite_params =
            crate::types::js_array_params_to_sqlite(&env, unsafe { params.raw() }, params_len)?;

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let col_count = stmt.column_count();
        let columns: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap().to_string())
            .collect();

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            sqlite_params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let mut rows_result = stmt
            .query(param_refs.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let mut js_rows: Vec<napi::sys::napi_value> = Vec::new();
        let interned_keys = intern_column_keys(&env, &columns)?;
        let resolved_types = resolve_column_types(&columns, &col_types);
        while let Some(row) = rows_result
            .next()
            .map_err(|e| Error::from_reason(format!("{e}")))?
        {
            let js_obj = row_to_typed_js_object(&env, row, &interned_keys, &resolved_types, &table_name, &columns)?;
            js_rows.push(js_obj);
        }

        let mut arr = std::ptr::null_mut();
        unsafe { napi::sys::napi_create_array_with_length(env.raw(), js_rows.len(), &mut arr) };
        for (i, val) in js_rows.iter().enumerate() {
            unsafe { napi::sys::napi_set_element(env.raw(), arr, i as u32, *val) };
        }

        Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), arr) })
    }

    /// Execute a query and return rows in batches (array of arrays).
    #[napi]
    pub fn query_batched(
        &self,
        env: Env,
        sql: String,
        params: JsObject,
        column_types: JsObject,
        table_name: String,
        batch_size: u32,
    ) -> Result<JsObject> {
        let conn = self.conn.borrow();
        let col_types = parse_column_types(&env, &column_types)?;

        let params_len = params.get_array_length()?;
        let sqlite_params =
            crate::types::js_array_params_to_sqlite(&env, unsafe { params.raw() }, params_len)?;

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let col_count = stmt.column_count();
        let columns: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap().to_string())
            .collect();

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            sqlite_params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let mut rows_result = stmt
            .query(param_refs.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let mut batches: Vec<napi::sys::napi_value> = Vec::new();
        let mut current_batch: Vec<napi::sys::napi_value> = Vec::with_capacity(batch_size as usize);
        let interned_keys = intern_column_keys(&env, &columns)?;
        let resolved_types = resolve_column_types(&columns, &col_types);

        while let Some(row) = rows_result
            .next()
            .map_err(|e| Error::from_reason(format!("{e}")))?
        {
            let js_obj = row_to_typed_js_object(&env, row, &interned_keys, &resolved_types, &table_name, &columns)?;
            current_batch.push(js_obj);

            if current_batch.len() >= batch_size as usize {
                // Flush batch to JS array
                let mut batch_arr = std::ptr::null_mut();
                unsafe {
                    napi::sys::napi_create_array_with_length(
                        env.raw(),
                        current_batch.len(),
                        &mut batch_arr,
                    )
                };
                for (i, val) in current_batch.iter().enumerate() {
                    unsafe { napi::sys::napi_set_element(env.raw(), batch_arr, i as u32, *val) };
                }
                batches.push(batch_arr);
                current_batch.clear();
            }
        }

        // Flush remaining rows
        if !current_batch.is_empty() {
            let mut batch_arr = std::ptr::null_mut();
            unsafe {
                napi::sys::napi_create_array_with_length(
                    env.raw(),
                    current_batch.len(),
                    &mut batch_arr,
                )
            };
            for (i, val) in current_batch.iter().enumerate() {
                unsafe { napi::sys::napi_set_element(env.raw(), batch_arr, i as u32, *val) };
            }
            batches.push(batch_arr);
        }

        // Create outer array of batches
        let mut result = std::ptr::null_mut();
        unsafe { napi::sys::napi_create_array_with_length(env.raw(), batches.len(), &mut result) };
        for (i, batch) in batches.iter().enumerate() {
            unsafe { napi::sys::napi_set_element(env.raw(), result, i as u32, *batch) };
        }

        Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), result) })
    }

    pub(crate) fn get_conn(&self) -> Arc<RefCell<Connection>> {
        self.conn.clone()
    }
}

// Helper trait for Value to ValueRef conversion
trait ToValueRef {
    fn to_value_ref(&self) -> rusqlite::types::ValueRef<'_>;
}

impl ToValueRef for rusqlite::types::Value {
    fn to_value_ref(&self) -> rusqlite::types::ValueRef<'_> {
        match self {
            rusqlite::types::Value::Null => rusqlite::types::ValueRef::Null,
            rusqlite::types::Value::Integer(i) => rusqlite::types::ValueRef::Integer(*i),
            rusqlite::types::Value::Real(f) => rusqlite::types::ValueRef::Real(*f),
            rusqlite::types::Value::Text(s) => rusqlite::types::ValueRef::Text(s.as_bytes()),
            rusqlite::types::Value::Blob(b) => rusqlite::types::ValueRef::Blob(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn test_open_memory() {
        let conn = Connection::open_in_memory().unwrap();
        let page_size: i64 = conn
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .unwrap();
        assert!(page_size > 0);
    }

    #[test]
    fn test_exec_create_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM test", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_pragma_returns_results() {
        let conn = Connection::open_in_memory().unwrap();
        let page_size: i64 = conn
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .unwrap();
        assert!(page_size == 4096 || page_size == 1024 || page_size == 8192);
    }

    #[test]
    fn test_transaction_commit() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY)")
            .unwrap();

        {
            let tx = conn.transaction().unwrap();
            tx.execute("INSERT INTO test (id) VALUES (1)", []).unwrap();
            tx.commit().unwrap();
        }

        let count: i64 = conn
            .query_row("SELECT count(*) FROM test", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_transaction_rollback() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY)")
            .unwrap();

        {
            let tx = conn.transaction().unwrap();
            tx.execute("INSERT INTO test (id) VALUES (1)", []).unwrap();
            tx.rollback().unwrap();
        }

        let count: i64 = conn
            .query_row("SELECT count(*) FROM test", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_error_message_includes_sql() {
        let conn = Connection::open_in_memory().unwrap();
        let result = conn.execute_batch("SELECT * FROM nonexistent_table");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nonexistent_table") || err.contains("no such table"));
    }

    #[test]
    fn test_is_autocommit() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(conn.is_autocommit());
    }

    #[test]
    fn test_close() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER)").unwrap();
        drop(conn);
    }
}
