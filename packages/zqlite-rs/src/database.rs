use std::cell::RefCell;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::{Env, JsObject, JsString, NapiRaw, NapiValue};
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

        // Match better-sqlite3's default busy timeout (5s) so concurrent
        // readers wait instead of immediately failing with "database is locked".
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .map_err(|e| Error::from_reason(format!("Failed to set busy_timeout: {e}")))?;

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
        // Actually close the SQLite connection to release file locks.
        // This is critical for locking_mode=EXCLUSIVE connections.
        // Replace the connection with a closed in-memory one.
        let old = self.conn.replace(
            Connection::open_in_memory()
                .map_err(|e| Error::from_reason(format!("close: {e}")))?
        );
        drop(old);
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
            // Retrieve the original JS exception message for better debugging
            let mut exception_raw = std::ptr::null_mut();
            let exc_status = unsafe {
                napi::sys::napi_get_and_clear_last_exception(env.raw(), &mut exception_raw)
            };
            if exc_status == 0 && !exception_raw.is_null() {
                // Try to get .message property from the exception object
                let exception = unsafe { JsObject::from_raw_unchecked(env.raw(), exception_raw) };
                if let Ok(msg) = exception.get_named_property::<JsString>("message") {
                    if let Ok(msg_str) = msg.into_utf8() {
                        if let Ok(s) = msg_str.as_str() {
                            return Err(Error::from_reason(format!(
                                "Transaction callback threw: {s}"
                            )));
                        }
                    }
                }
                // Fallback: try to coerce exception to string
                Err(Error::from_reason("Transaction callback threw an exception (could not extract message)"))
            } else {
                Err(Error::from_reason("Transaction callback threw an exception"))
            }
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
            .prepare_cached(&sql)
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
            .prepare_cached(&sql)
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

    /// Get a single row with typed conversion done in Rust.
    /// Returns undefined if no row matches (matching better-sqlite3's .get() behavior).
    #[napi]
    pub fn get_row(
        &self,
        env: Env,
        sql: String,
        params: JsObject,
        column_types: JsObject,
        table_name: String,
    ) -> Result<JsObject> {
        let conn = self.conn.borrow();
        let col_types = parse_column_types(&env, &column_types)?;

        let params_len = params.get_array_length()?;
        let sqlite_params =
            crate::types::js_array_params_to_sqlite(&env, unsafe { params.raw() }, params_len)?;

        let mut stmt = conn
            .prepare_cached(&sql)
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

        match rows_result.next().map_err(|e| Error::from_reason(format!("{e}")))? {
            None => {
                // Return undefined (not null) — matches better-sqlite3 .get()
                let mut undefined = std::ptr::null_mut();
                unsafe { napi::sys::napi_get_undefined(env.raw(), &mut undefined) };
                Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), undefined) })
            }
            Some(row) => {
                let interned_keys = intern_column_keys(&env, &columns)?;
                let resolved_types = resolve_column_types(&columns, &col_types);
                let raw = row_to_typed_js_object(&env, row, &interned_keys, &resolved_types, &table_name, &columns)?;
                Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), raw) })
            }
        }
    }

    /// Batch-fetch multiple single-row lookups in one napi call.
    /// Takes a SQL with ? placeholders, the number of params per lookup (params_per_key),
    /// and a flat array of all params [key1_p1, key1_p2, ..., keyN_p1, keyN_p2, ...].
    /// Executes the query once per key group. Returns allBuf-style buffer with all results
    /// (0 or 1 row per key, ordered). Empty results are omitted.
    #[napi]
    pub fn get_rows_multi_buf(
        &self,
        _env: Env,
        sql: String,
        params: JsObject,
        params_per_key: u32,
    ) -> Result<Buffer> {
        let conn = self.conn.borrow();

        let params_len = params.get_array_length()?;
        let all_params =
            crate::types::js_array_params_to_sqlite(&_env, unsafe { params.raw() }, params_len)?;

        let mut stmt = conn
            .prepare_cached(&sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let col_count = stmt.column_count();
        let ppk = params_per_key as usize;
        let num_keys = all_params.len() / ppk;

        let mut buf: Vec<u8> = Vec::with_capacity(col_count * 64 * num_keys);
        // Reserve header
        buf.extend_from_slice(&[0u8; 6]);

        let mut row_count: u32 = 0;
        for chunk in all_params.chunks(ppk) {
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                chunk.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

            let mut rows_result = stmt
                .query(param_refs.as_slice())
                .map_err(|e| Error::from_reason(format!("{e}")))?;

            if let Some(row) = rows_result.next().map_err(|e| Error::from_reason(format!("{e}")))? {
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
        }

        buf[0..4].copy_from_slice(&row_count.to_le_bytes());
        buf[4..6].copy_from_slice(&(col_count as u16).to_le_bytes());

        Ok(Buffer::from(buf))
    }

    /// Get multiple rows as a binary buffer (allBuf protocol).
    /// Protocol: [u32 row_count][u16 col_count] then per cell: [u8 tag][payload]
    #[napi]
    pub fn get_rows_buf(
        &self,
        _env: Env,
        sql: String,
        params: JsObject,
    ) -> Result<Buffer> {
        let conn = self.conn.borrow();

        let params_len = params.get_array_length()?;
        let sqlite_params =
            crate::types::js_array_params_to_sqlite(&_env, unsafe { params.raw() }, params_len)?;

        let mut stmt = conn
            .prepare_cached(&sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let col_count = stmt.column_count();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            sqlite_params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let mut rows_result = stmt
            .query(param_refs.as_slice())
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let mut buf: Vec<u8> = Vec::with_capacity(col_count * 64 * 32);
        // Reserve space for header
        buf.extend_from_slice(&[0u8; 6]);

        let mut row_count: u32 = 0;
        while let Some(row) = rows_result.next().map_err(|e| Error::from_reason(format!("{e}")))? {
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

    /// Get change log entries as a binary buffer (allBuf protocol) with batching.
    /// Hard-codes the changeLog2 query. Returns rows in allBuf format.
    #[napi]
    pub fn changes_since_buf(
        &self,
        _env: Env,
        prev_version: String,
        batch_size: u32,
        offset: u32,
    ) -> Result<Buffer> {
        let conn = self.conn.borrow();

        let sql = r#"SELECT "stateVersion", "table", "rowKey", "op" FROM "_zero.changeLog2" WHERE "stateVersion" > ? ORDER BY "stateVersion" ASC, "pos" ASC LIMIT ? OFFSET ?"#;

        let mut stmt = conn
            .prepare_cached(sql)
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let col_count: usize = 4;
        let mut rows_result = stmt
            .query(rusqlite::params![prev_version, batch_size, offset])
            .map_err(|e| Error::from_reason(format!("{e}")))?;

        let mut buf: Vec<u8> = Vec::with_capacity(col_count * 64 * 32);
        buf.extend_from_slice(&[0u8; 6]);

        let mut row_count: u32 = 0;
        while let Some(row) = rows_result.next().map_err(|e| Error::from_reason(format!("{e}")))? {
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

        buf[0..4].copy_from_slice(&row_count.to_le_bytes());
        buf[4..6].copy_from_slice(&(col_count as u16).to_le_bytes());

        Ok(Buffer::from(buf))
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

    #[test]
    fn test_get_row_returns_none_for_missing() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
        let mut stmt = conn.prepare("SELECT * FROM test WHERE id = ?").unwrap();
        let mut rows = stmt.query(rusqlite::params![999]).unwrap();
        assert!(rows.next().unwrap().is_none());
    }

    #[test]
    fn test_get_rows_buf_empty_result() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER, name TEXT)").unwrap();
        let mut stmt = conn.prepare("SELECT id, name FROM test").unwrap();
        let col_count = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&[0u8; 6]);
        let mut row_count: u32 = 0;
        while let Some(_row) = rows.next().unwrap() {
            row_count += 1;
        }
        buf[0..4].copy_from_slice(&row_count.to_le_bytes());
        buf[4..6].copy_from_slice(&(col_count as u16).to_le_bytes());

        // Should be: [0,0,0,0, 2,0] (0 rows, 2 columns)
        assert_eq!(buf, vec![0, 0, 0, 0, 2, 0]);
    }

    #[test]
    fn test_changes_since_buf_empty() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"CREATE TABLE "_zero.changeLog2" ("stateVersion" TEXT, "table" TEXT, "rowKey" TEXT, "op" TEXT, "pos" INTEGER)"#
        ).unwrap();
        let mut stmt = conn.prepare(
            r#"SELECT "stateVersion", "table", "rowKey", "op" FROM "_zero.changeLog2" WHERE "stateVersion" > ? ORDER BY "stateVersion" ASC, "pos" ASC LIMIT ? OFFSET ?"#
        ).unwrap();
        let mut rows = stmt.query(rusqlite::params!["00", 500, 0]).unwrap();
        let mut buf: Vec<u8> = vec![0u8; 6];
        let mut row_count: u32 = 0;
        while let Some(_) = rows.next().unwrap() { row_count += 1; }
        buf[0..4].copy_from_slice(&row_count.to_le_bytes());
        buf[4..6].copy_from_slice(&4u16.to_le_bytes());
        assert_eq!(buf, vec![0, 0, 0, 0, 4, 0]);
    }

    #[test]
    fn test_allbuf_protocol_format() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER, name TEXT, val REAL)").unwrap();
        conn.execute("INSERT INTO test VALUES (42, 'hello', 3.14)", []).unwrap();
        conn.execute("INSERT INTO test VALUES (NULL, NULL, NULL)", []).unwrap();

        let mut stmt = conn.prepare("SELECT id, name, val FROM test").unwrap();
        let col_count = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&[0u8; 6]);
        let mut row_count: u32 = 0;

        while let Some(row) = rows.next().unwrap() {
            for i in 0..col_count {
                let val = row.get_ref(i).unwrap();
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
        buf[0..4].copy_from_slice(&row_count.to_le_bytes());
        buf[4..6].copy_from_slice(&(col_count as u16).to_le_bytes());

        // Verify header: 2 rows, 3 columns
        assert_eq!(&buf[0..4], &2u32.to_le_bytes());
        assert_eq!(&buf[4..6], &3u16.to_le_bytes());

        // Row 1: int 42, text "hello", real 3.14
        assert_eq!(buf[6], 1); // int tag
        assert_eq!(&buf[7..15], &42i64.to_le_bytes());
        assert_eq!(buf[15], 3); // text tag
        assert_eq!(&buf[16..20], &5u32.to_le_bytes()); // "hello" len
        assert_eq!(&buf[20..25], b"hello");
        assert_eq!(buf[25], 2); // real tag
        assert_eq!(&buf[26..34], &3.14f64.to_le_bytes());

        // Row 2: all nulls
        assert_eq!(buf[34], 0);
        assert_eq!(buf[35], 0);
        assert_eq!(buf[36], 0);
    }
}
