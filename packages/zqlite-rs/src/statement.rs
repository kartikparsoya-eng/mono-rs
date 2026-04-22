use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::{Env, JsObject, NapiRaw, NapiValue};
use napi_derive::napi;
use rusqlite::Connection;

use crate::row_iterator::RowIterator;
use crate::types::{get_column_names, intern_column_keys, js_array_params_to_sqlite, row_to_js_object, row_to_js_object_fast};

// FFI declarations for scanstatus APIs.
// These are compiled into the bundled SQLite (SQLITE_ENABLE_STMT_SCANSTATUS)
// but not re-exported through the libsqlite3-sys pre-generated bindings.
extern "C" {
    fn sqlite3_stmt_scanstatus_v2(
        pStmt: *mut libsqlite3_sys::sqlite3_stmt,
        idx: c_int,
        iScanStatusOp: c_int,
        flags: c_int,
        pOut: *mut c_void,
    ) -> c_int;
    fn sqlite3_stmt_scanstatus_reset(pStmt: *mut libsqlite3_sys::sqlite3_stmt);
}

/// Holds a raw SQLite prepared statement for scanstatus queries.
/// The raw stmt is prepared via sqlite3_prepare_v2 (not rusqlite's cached statements)
/// because we need the raw pointer for sqlite3_stmt_scanstatus_v2 FFI calls.
struct RawScanStmt(*mut libsqlite3_sys::sqlite3_stmt);

impl Drop for RawScanStmt {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libsqlite3_sys::sqlite3_finalize(self.0) };
        }
    }
}

// Scanstatus opcode → output type mapping (from SQLite docs):
// NLOOP(0), NVISIT(1), NCYCLE(7) → sqlite3_int64
// EST(2) → f64
// NAME(3), EXPLAIN(4) → const char*
// SELECTID(5), PARENTID(6) → i32

#[napi]
pub struct Statement {
    conn: Arc<RefCell<Connection>>,
    sql: String,
    safe_integers: bool,
    cached_columns: RefCell<Option<Vec<String>>>,
    /// Lazily-prepared raw statement for scanstatus FFI calls.
    scan_stmt: RefCell<Option<RawScanStmt>>,
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
            .prepare(&self.sql)
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
    pub fn get(&self, env: Env, params: JsObject) -> Result<JsObject> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(&self.sql)
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
                Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), raw) })
            }
            None => {
                // Return undefined (not null) to match better-sqlite3 behavior
                let mut undefined_val = std::ptr::null_mut();
                unsafe { napi::sys::napi_get_undefined(env.raw(), &mut undefined_val) };
                Ok(unsafe { JsObject::from_raw_unchecked(env.raw(), undefined_val) })
            }
        }
    }

    /// Get all rows as an array of objects.
    #[napi]
    pub fn all(&self, env: Env, params: JsObject) -> Result<JsObject> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(&self.sql)
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
            .prepare(&self.sql)
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

    /// Get scanstatus information for a prepared statement.
    /// Wraps sqlite3_stmt_scanstatus_v2. Returns undefined when idx is out of range.
    ///
    /// Opcodes (matching SQLite constants):
    ///   0=NLOOP(i64), 1=NVISIT(i64), 2=EST(f64), 3=NAME(str),
    ///   4=EXPLAIN(str), 5=SELECTID(i32), 6=PARENTID(i32), 7=NCYCLE(i64)
    #[napi]
    pub fn scan_status(&self, idx: i32, op: i32, flags: i32) -> Result<Either<Either<f64, String>, Undefined>> {
        if let Err(e) = self.ensure_scan_stmt() {
            return Err(Error::from_reason(e));
        }
        let scan = self.scan_stmt.borrow();
        let raw_stmt = match scan.as_ref() {
            Some(s) => s.0,
            None => return Ok(Either::B(())),
        };

        match op {
            // NLOOP(0), NVISIT(1), NCYCLE(7) -> sqlite3_int64
            0 | 1 | 7 => {
                let mut val: i64 = 0;
                let rc = unsafe {
                    sqlite3_stmt_scanstatus_v2(
                        raw_stmt,
                        idx,
                        op,
                        flags,
                        &mut val as *mut i64 as *mut c_void,
                    )
                };
                if rc != 0 {
                    return Ok(Either::B(()));
                }
                Ok(Either::A(Either::A(val as f64)))
            }
            // EST(2) -> f64
            2 => {
                let mut val: f64 = 0.0;
                let rc = unsafe {
                    sqlite3_stmt_scanstatus_v2(
                        raw_stmt,
                        idx,
                        op,
                        flags,
                        &mut val as *mut f64 as *mut c_void,
                    )
                };
                if rc != 0 {
                    return Ok(Either::B(()));
                }
                Ok(Either::A(Either::A(val)))
            }
            // NAME(3), EXPLAIN(4) -> const char*
            3 | 4 => {
                let mut val: *const c_char = std::ptr::null();
                let rc = unsafe {
                    sqlite3_stmt_scanstatus_v2(
                        raw_stmt,
                        idx,
                        op,
                        flags,
                        &mut val as *mut *const c_char as *mut c_void,
                    )
                };
                if rc != 0 || val.is_null() {
                    return Ok(Either::B(()));
                }
                let s = unsafe { CStr::from_ptr(val) }.to_string_lossy();
                Ok(Either::A(Either::B(s.into_owned())))
            }
            // SELECTID(5), PARENTID(6) -> int
            5 | 6 => {
                let mut val: i32 = 0;
                let rc = unsafe {
                    sqlite3_stmt_scanstatus_v2(
                        raw_stmt,
                        idx,
                        op,
                        flags,
                        &mut val as *mut i32 as *mut c_void,
                    )
                };
                if rc != 0 {
                    return Ok(Either::B(()));
                }
                Ok(Either::A(Either::A(val as f64)))
            }
            _ => Ok(Either::B(())),
        }
    }

    /// Reset scanstatus counters for the prepared statement.
    #[napi]
    pub fn scan_status_reset(&self) -> Result<()> {
        let scan = self.scan_stmt.borrow();
        if let Some(s) = scan.as_ref() {
            unsafe { sqlite3_stmt_scanstatus_reset(s.0) };
        }
        Ok(())
    }

    /// Create a lazy row iterator. Materializes all rows in Rust, converts lazily to JS.
    #[napi]
    pub fn iterate(&self, env: Env, params: JsObject) -> Result<RowIterator> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(&self.sql)
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
fn extract_params(env: &Env, params: &JsObject, stmt: &rusqlite::Statement) -> Result<Vec<rusqlite::types::Value>> {
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
    stmt: &rusqlite::Statement,
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
            scan_stmt: RefCell::new(None),
        }
    }

    /// Get column names from the prepared statement (no caching to handle schema changes).
    fn get_columns(&self, stmt: &rusqlite::Statement) -> Vec<String> {
        get_column_names(stmt)
    }

    /// Ensure the raw scan statement is prepared for scanstatus calls.
    /// Uses the same database handle as the main connection.
    fn ensure_scan_stmt(&self) -> std::result::Result<(), String> {
        let mut scan = self.scan_stmt.borrow_mut();
        if scan.is_some() {
            return Ok(());
        }
        let conn = self.conn.borrow();
        // SAFETY: handle() returns the raw sqlite3* managed by rusqlite.
        // It remains valid as long as the Connection is alive, which it is
        // because we hold Arc<RefCell<Connection>>.
        let db_handle = unsafe { conn.handle() };
        let sql_bytes = self.sql.as_bytes();
        let mut raw_stmt: *mut libsqlite3_sys::sqlite3_stmt = std::ptr::null_mut();
        let mut tail: *const c_char = std::ptr::null();
        let rc = unsafe {
            libsqlite3_sys::sqlite3_prepare_v2(
                db_handle,
                sql_bytes.as_ptr() as *const c_char,
                sql_bytes.len() as c_int,
                &mut raw_stmt,
                &mut tail,
            )
        };
        if rc != 0 || raw_stmt.is_null() {
            return Err(format!("sqlite3_prepare_v2 failed with code {rc}"));
        }
        *scan = Some(RawScanStmt(raw_stmt));
        Ok(())
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
