use napi::bindgen_prelude::*;
use napi::{Env, JsObject, NapiRaw};
use rusqlite::types::{Value, ValueRef};
use rusqlite::Row;

// ─── Phase 3: Column-type-aware row conversion ───

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnType {
    Boolean,
    Number,
    String,
    Json,
    Null,
}

impl ColumnType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "boolean" => ColumnType::Boolean,
            "number" => ColumnType::Number,
            "string" => ColumnType::String,
            "json" => ColumnType::Json,
            _ => ColumnType::Null,
        }
    }
}

/// Parse a JS object {colName: typeStr} into a Vec of (name, ColumnType).
pub fn parse_column_types(env: &Env, obj: &JsObject) -> Result<Vec<(std::string::String, ColumnType)>> {
    let names = obj.get_property_names()?;
    let len = names.get_array_length()?;
    let mut result = Vec::with_capacity(len as usize);
    for i in 0..len {
        let key: napi::JsString = names.get_element(i)?;
        let key_str = key.into_utf8()?.into_owned()?;
        let val: napi::JsString = obj.get_named_property(&key_str)?;
        let val_str = val.into_utf8()?.into_owned()?;
        result.push((key_str, ColumnType::from_str(&val_str)));
    }
    Ok(result)
}

/// Recursively convert a serde_json::Value to a raw napi_value.
pub fn serde_json_to_js(env: &Env, value: &serde_json::Value) -> Result<napi::sys::napi_value> {
    use serde_json::Value as JV;
    match value {
        JV::Null => {
            let mut result = std::ptr::null_mut();
            unsafe { napi::sys::napi_get_null(env.raw(), &mut result) };
            Ok(result)
        }
        JV::Bool(b) => {
            let mut result = std::ptr::null_mut();
            unsafe { napi::sys::napi_get_boolean(env.raw(), *b, &mut result) };
            Ok(result)
        }
        JV::Number(n) => {
            let mut result = std::ptr::null_mut();
            let f = n.as_f64().unwrap_or(0.0);
            unsafe { napi::sys::napi_create_double(env.raw(), f, &mut result) };
            Ok(result)
        }
        JV::String(s) => {
            let mut result = std::ptr::null_mut();
            unsafe {
                napi::sys::napi_create_string_utf8(
                    env.raw(),
                    s.as_ptr() as *const std::ffi::c_char,
                    s.len() as isize,
                    &mut result,
                )
            };
            Ok(result)
        }
        JV::Array(arr) => {
            let mut result = std::ptr::null_mut();
            unsafe { napi::sys::napi_create_array_with_length(env.raw(), arr.len(), &mut result) };
            for (i, item) in arr.iter().enumerate() {
                let js_item = serde_json_to_js(env, item)?;
                unsafe { napi::sys::napi_set_element(env.raw(), result, i as u32, js_item) };
            }
            Ok(result)
        }
        JV::Object(map) => {
            let mut result = std::ptr::null_mut();
            unsafe { napi::sys::napi_create_object(env.raw(), &mut result) };
            for (key, val) in map {
                let js_val = serde_json_to_js(env, val)?;
                let key_cstr = std::ffi::CString::new(key.as_str()).unwrap();
                unsafe {
                    napi::sys::napi_set_named_property(env.raw(), result, key_cstr.as_ptr(), js_val)
                };
            }
            Ok(result)
        }
    }
}

/// Convert a rusqlite Row to a raw JS object with column-type-aware conversion.
/// This is the hot-path function used by query_all/query_batched.
/// Uses pre-interned keys and pre-resolved column types for maximum performance.
pub fn row_to_typed_js_object(
    env: &Env,
    row: &rusqlite::Row,
    interned_keys: &[napi::sys::napi_value],
    resolved_types: &[ColumnType],
    table_name: &str,
    columns: &[std::string::String],
) -> Result<napi::sys::napi_value> {
    let mut obj = std::ptr::null_mut();
    unsafe { napi::sys::napi_create_object(env.raw(), &mut obj) };

    for (i, &key) in interned_keys.iter().enumerate() {
        let col_type = resolved_types[i];

        let raw_value = row
            .get_ref(i)
            .map_err(|e| Error::from_reason(e.to_string()))?;

        let js_val = match raw_value {
            rusqlite::types::ValueRef::Null => {
                let mut v = std::ptr::null_mut();
                unsafe { napi::sys::napi_get_null(env.raw(), &mut v) };
                v
            }
            rusqlite::types::ValueRef::Integer(n) => match col_type {
                ColumnType::Boolean => {
                    let mut v = std::ptr::null_mut();
                    unsafe { napi::sys::napi_get_boolean(env.raw(), n != 0, &mut v) };
                    v
                }
                ColumnType::Number => {
                    const MAX_SAFE: i64 = 9007199254740991;
                    const MIN_SAFE: i64 = -9007199254740991;
                    if n > MAX_SAFE || n < MIN_SAFE {
                        return Err(Error::from_reason(format!(
                            "value {} (in {}.{}) is outside of supported bounds",
                            n, table_name, &columns[i]
                        )));
                    }
                    let mut v = std::ptr::null_mut();
                    unsafe { napi::sys::napi_create_double(env.raw(), n as f64, &mut v) };
                    v
                }
                _ => {
                    const MAX_SAFE: i64 = 9007199254740991;
                    const MIN_SAFE: i64 = -9007199254740991;
                    if n > MAX_SAFE || n < MIN_SAFE {
                        return Err(Error::from_reason(format!(
                            "value {} (in {}.{}) is outside of supported bounds",
                            n, table_name, &columns[i]
                        )));
                    }
                    let mut v = std::ptr::null_mut();
                    unsafe { napi::sys::napi_create_double(env.raw(), n as f64, &mut v) };
                    v
                }
            },
            rusqlite::types::ValueRef::Real(f) => {
                let mut v = std::ptr::null_mut();
                unsafe { napi::sys::napi_create_double(env.raw(), f, &mut v) };
                v
            }
            rusqlite::types::ValueRef::Text(bytes) => {
                let s = std::str::from_utf8(bytes)
                    .map_err(|e| Error::from_reason(e.to_string()))?;
                match col_type {
                    ColumnType::Json => {
                        let parsed: serde_json::Value = serde_json::from_str(s).map_err(|_| {
                            Error::from_reason(format!(
                                "invalid json value for column {}:RAW:{}",
                                &columns[i], s
                            ))
                        })?;
                        serde_json_to_js(env, &parsed)?
                    }
                    _ => {
                        let mut v = std::ptr::null_mut();
                        unsafe {
                            napi::sys::napi_create_string_utf8(
                                env.raw(),
                                s.as_ptr() as *const std::ffi::c_char,
                                s.len() as isize,
                                &mut v,
                            )
                        };
                        v
                    }
                }
            }
            rusqlite::types::ValueRef::Blob(bytes) => {
                let mut v = std::ptr::null_mut();
                let mut data_ptr = std::ptr::null_mut();
                unsafe {
                    napi::sys::napi_create_buffer_copy(
                        env.raw(),
                        bytes.len(),
                        bytes.as_ptr() as *const _,
                        &mut data_ptr,
                        &mut v,
                    )
                };
                v
            }
        };

        unsafe { napi::sys::napi_set_property(env.raw(), obj, key, js_val) };
    }

    Ok(obj)
}

/// Pre-resolve column types into a flat Vec indexed by column position.
pub fn resolve_column_types(
    columns: &[std::string::String],
    column_types: &[(std::string::String, ColumnType)],
) -> Vec<ColumnType> {
    columns
        .iter()
        .map(|col_name| {
            column_types
                .iter()
                .find(|(name, _)| name == col_name)
                .map(|(_, t)| *t)
                .unwrap_or(ColumnType::String)
        })
        .collect()
}

/// Set a SQLite value as a named property on a JsObject.
///
/// Type coercion contract (matching better-sqlite3):
/// - NULL → null
/// - INTEGER → number (f64) by default, bigint if safe_integers is true
/// - REAL → number (f64)
/// - TEXT → string
/// - BLOB → Buffer
pub fn set_sqlite_value(
    env: &Env,
    obj: &mut JsObject,
    key: &str,
    val: ValueRef,
    safe_integers: bool,
) -> Result<()> {
    match val {
        ValueRef::Null => {
            // Set null using raw napi sys (env.get_null() removed in v3)
            let mut null_val = std::ptr::null_mut();
            let status = unsafe { napi::sys::napi_get_null(env.raw(), &mut null_val) };
            if status != 0 {
                return Err(Error::from_reason("Failed to get null"));
            }
            let null_unknown = unsafe { Unknown::from_raw_unchecked(env.raw(), null_val) };
            obj.set_named_property(key, null_unknown)?;
        }
        ValueRef::Integer(i) => {
            if safe_integers {
                let bigint = env.create_bigint_from_i64(i)?;
                obj.set_named_property(key, bigint)?;
            } else {
                obj.set_named_property(key, i as f64)?;
            }
        }
        ValueRef::Real(f) => {
            obj.set_named_property(key, f)?;
        }
        ValueRef::Text(s) => {
            let s = std::str::from_utf8(s)
                .map_err(|e| Error::from_reason(format!("Invalid UTF-8 in text column: {e}")))?;
            obj.set_named_property(key, s)?;
        }
        ValueRef::Blob(b) => {
            let buf = Buffer::from(b.to_vec());
            obj.set_named_property(key, buf)?;
        }
    }
    Ok(())
}

/// Convert a napi_value to a rusqlite Value for binding parameters.
/// Uses raw napi sys calls to inspect type without needing Unknown<'_> in signatures.
pub fn napi_value_to_sqlite_param(env: &Env, raw: napi::sys::napi_value) -> Result<Value> {
    let value_type = {
        let mut vt = 0;
        let status =
            unsafe { napi::sys::napi_typeof(env.raw(), raw, &mut vt) };
        if status != 0 {
            return Err(Error::from_reason("Failed to get value type"));
        }
        vt
    };

    match value_type {
        // napi_undefined = 0, napi_null = 1
        0 | 1 => Ok(Value::Null),
        // napi_boolean = 2
        2 => {
            let mut result = false;
            let status =
                unsafe { napi::sys::napi_get_value_bool(env.raw(), raw, &mut result) };
            if status != 0 {
                return Err(Error::from_reason("Failed to get boolean value"));
            }
            Ok(Value::Integer(if result { 1 } else { 0 }))
        }
        // napi_number = 3
        3 => {
            let mut n: f64 = 0.0;
            let status =
                unsafe { napi::sys::napi_get_value_double(env.raw(), raw, &mut n) };
            if status != 0 {
                return Err(Error::from_reason("Failed to get number value"));
            }
            if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
                Ok(Value::Integer(n as i64))
            } else {
                Ok(Value::Real(n))
            }
        }
        // napi_string = 4
        4 => {
            // Get string length
            let mut len = 0;
            unsafe {
                napi::sys::napi_get_value_string_utf8(
                    env.raw(),
                    raw,
                    std::ptr::null_mut(),
                    0,
                    &mut len,
                );
            }
            let mut buf = vec![0u8; len + 1];
            let mut copied = 0;
            unsafe {
                napi::sys::napi_get_value_string_utf8(
                    env.raw(),
                    raw,
                    buf.as_mut_ptr() as *mut std::ffi::c_char,
                    buf.len(),
                    &mut copied,
                );
            }
            buf.truncate(copied);
            let s = String::from_utf8(buf)
                .map_err(|e| Error::from_reason(format!("Invalid UTF-8 in string param: {e}")))?;
            Ok(Value::Text(s))
        }
        // napi_symbol = 5 (unsupported)
        5 => Err(Error::from_reason(
            "Cannot bind Symbol as SQLite parameter",
        )),
        // napi_object = 6 — check if Buffer
        6 => {
            let mut is_buffer = false;
            unsafe {
                napi::sys::napi_is_buffer(env.raw(), raw, &mut is_buffer);
            }
            if is_buffer {
                let mut data = std::ptr::null_mut();
                let mut length = 0;
                unsafe {
                    napi::sys::napi_get_buffer_info(
                        env.raw(),
                        raw,
                        &mut data,
                        &mut length,
                    );
                }
                let slice = unsafe { std::slice::from_raw_parts(data as *const u8, length) };
                Ok(Value::Blob(slice.to_vec()))
            } else {
                Err(Error::from_reason(
                    "Cannot bind object as SQLite parameter (expected null, number, bigint, string, or Buffer)",
                ))
            }
        }
        // napi_external = 7 (unsupported)
        7 => Err(Error::from_reason(
            "Cannot bind external as SQLite parameter",
        )),
        // napi_bigint = 9
        9 => {
            let mut value: i64 = 0;
            let mut lossless = false;
            let status = unsafe {
                napi::sys::napi_get_value_bigint_int64(env.raw(), raw, &mut value, &mut lossless)
            };
            if status != 0 {
                return Err(Error::from_reason("Failed to get bigint value"));
            }
            Ok(Value::Integer(value))
        }
        _ => Err(Error::from_reason(format!(
            "Cannot bind value type {value_type} as SQLite parameter"
        ))),
    }
}

/// Pre-intern column names as JS string napi_values for reuse across rows.
pub fn intern_column_keys(env: &Env, columns: &[String]) -> Result<Vec<napi::sys::napi_value>> {
    let mut keys = Vec::with_capacity(columns.len());
    for col in columns {
        let mut key = std::ptr::null_mut();
        let status = unsafe {
            napi::sys::napi_create_string_utf8(
                env.raw(),
                col.as_ptr() as *const std::ffi::c_char,
                col.len() as isize,
                &mut key,
            )
        };
        if status != 0 {
            return Err(Error::from_reason(format!("Failed to intern key: {col}")));
        }
        keys.push(key);
    }
    Ok(keys)
}

/// Convert a rusqlite Row to a raw JS object using pre-interned keys.
/// This is the fast path — no CString allocation, no high-level wrappers.
pub fn row_to_js_object_fast(
    env: &Env,
    row: &Row,
    interned_keys: &[napi::sys::napi_value],
    safe_integers: bool,
) -> Result<napi::sys::napi_value> {
    let mut obj = std::ptr::null_mut();
    unsafe { napi::sys::napi_create_object(env.raw(), &mut obj) };

    for (i, &key) in interned_keys.iter().enumerate() {
        let val = row
            .get_ref(i)
            .map_err(|e| Error::from_reason(format!("Failed to get column {i}: {e}")))?;

        let js_val = sqlite_value_to_raw_napi(env, val, safe_integers)?;
        unsafe { napi::sys::napi_set_property(env.raw(), obj, key, js_val) };
    }

    Ok(obj)
}

/// Convert a SQLite ValueRef to a raw napi_value without high-level wrappers.
fn sqlite_value_to_raw_napi(
    env: &Env,
    val: ValueRef,
    safe_integers: bool,
) -> Result<napi::sys::napi_value> {
    let mut v = std::ptr::null_mut();
    match val {
        ValueRef::Null => {
            unsafe { napi::sys::napi_get_null(env.raw(), &mut v) };
        }
        ValueRef::Integer(i) => {
            if safe_integers {
                let mut words = [0u64; 1];
                let (sign, magnitude) = if i < 0 { (1, (-i) as u64) } else { (0, i as u64) };
                words[0] = magnitude;
                unsafe {
                    napi::sys::napi_create_bigint_words(
                        env.raw(),
                        sign,
                        1,
                        words.as_ptr(),
                        &mut v,
                    )
                };
            } else {
                unsafe { napi::sys::napi_create_double(env.raw(), i as f64, &mut v) };
            }
        }
        ValueRef::Real(f) => {
            unsafe { napi::sys::napi_create_double(env.raw(), f, &mut v) };
        }
        ValueRef::Text(s) => {
            let s = std::str::from_utf8(s)
                .map_err(|e| Error::from_reason(format!("Invalid UTF-8: {e}")))?;
            unsafe {
                napi::sys::napi_create_string_utf8(
                    env.raw(),
                    s.as_ptr() as *const std::ffi::c_char,
                    s.len() as isize,
                    &mut v,
                )
            };
        }
        ValueRef::Blob(b) => {
            let mut data_ptr = std::ptr::null_mut();
            unsafe {
                napi::sys::napi_create_buffer_copy(
                    env.raw(),
                    b.len(),
                    b.as_ptr() as *const _,
                    &mut data_ptr,
                    &mut v,
                )
            };
        }
    }
    Ok(v)
}

/// Convert a rusqlite Row to a JS object with column names as keys (legacy, uses high-level API).
pub fn row_to_js_object(
    env: &Env,
    row: &Row,
    columns: &[String],
    safe_integers: bool,
) -> Result<JsObject> {
    let mut obj = env.create_object()?;
    for (i, col) in columns.iter().enumerate() {
        let val = row
            .get_ref(i)
            .map_err(|e| Error::from_reason(format!("Failed to get column {i}: {e}")))?;
        set_sqlite_value(env, &mut obj, col, val, safe_integers)?;
    }
    Ok(obj)
}

/// Convert a Vec of owned rusqlite Values to a JS object (for RowIterator).
pub fn values_to_js_object(
    env: &Env,
    values: &[Value],
    columns: &[String],
    safe_integers: bool,
) -> Result<JsObject> {
    let mut obj = env.create_object()?;
    for (i, col) in columns.iter().enumerate() {
        let val_ref = match &values[i] {
            Value::Null => ValueRef::Null,
            Value::Integer(i) => ValueRef::Integer(*i),
            Value::Real(f) => ValueRef::Real(*f),
            Value::Text(s) => ValueRef::Text(s.as_bytes()),
            Value::Blob(b) => ValueRef::Blob(b),
        };
        set_sqlite_value(env, &mut obj, col, val_ref, safe_integers)?;
    }
    Ok(obj)
}

/// Extract column names from a rusqlite Statement.
pub fn get_column_names(stmt: &rusqlite::Statement) -> Vec<String> {
    stmt.column_names().iter().map(|s| s.to_string()).collect()
}

/// Convert a JS Array (as raw napi_value) of params to rusqlite Values.
pub fn js_array_params_to_sqlite(
    env: &Env,
    params_raw: napi::sys::napi_value,
    len: u32,
) -> Result<Vec<Value>> {
    let mut result = Vec::with_capacity(len as usize);
    for i in 0..len {
        let mut element = std::ptr::null_mut();
        let status = unsafe {
            napi::sys::napi_get_element(env.raw(), params_raw, i, &mut element)
        };
        if status != 0 {
            return Err(Error::from_reason(format!(
                "Failed to get param at index {i}"
            )));
        }
        result.push(napi_value_to_sqlite_param(env, element)?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use rusqlite::types::Value;
    use rusqlite::types::ValueRef;
    use super::ColumnType;

    #[test]
    fn test_column_type_from_str() {
        assert_eq!(ColumnType::from_str("boolean"), ColumnType::Boolean);
        assert_eq!(ColumnType::from_str("number"), ColumnType::Number);
        assert_eq!(ColumnType::from_str("string"), ColumnType::String);
        assert_eq!(ColumnType::from_str("json"), ColumnType::Json);
        assert_eq!(ColumnType::from_str("null"), ColumnType::Null);
        assert_eq!(ColumnType::from_str("unknown"), ColumnType::Null);
    }

    #[test]
    fn test_safe_integer_bounds() {
        const MAX_SAFE: i64 = 9007199254740991;
        const MIN_SAFE: i64 = -9007199254740991;
        assert!(MAX_SAFE <= 9007199254740991);
        assert!(MIN_SAFE >= -9007199254740991);
        assert!(MAX_SAFE + 1 > 9007199254740991);
        assert!(MIN_SAFE - 1 < -9007199254740991);
    }

    #[test]
    fn test_serde_json_parsing() {
        let val: serde_json::Value =
            serde_json::from_str(r#"{"a": 1, "b": "hello", "c": [1,2,3]}"#).unwrap();
        assert!(val.is_object());

        let null_val: serde_json::Value = serde_json::from_str("null").unwrap();
        assert!(null_val.is_null());

        let arr_val: serde_json::Value = serde_json::from_str("[1, 2, 3]").unwrap();
        assert!(arr_val.is_array());
    }

    #[test]
    fn test_value_null() {
        let v = Value::Null;
        assert!(matches!(v, Value::Null));
    }

    #[test]
    fn test_value_integer() {
        let v = Value::Integer(42);
        assert!(matches!(v, Value::Integer(42)));
    }

    #[test]
    fn test_value_real() {
        let v = Value::Real(3.14);
        if let Value::Real(f) = v {
            assert!((f - 3.14).abs() < f64::EPSILON);
        } else {
            panic!("Expected Real");
        }
    }

    #[test]
    fn test_value_text() {
        let v = Value::Text("hello".to_string());
        assert!(matches!(v, Value::Text(ref s) if s == "hello"));
    }

    #[test]
    fn test_value_blob() {
        let v = Value::Blob(vec![1, 2, 3]);
        assert!(matches!(v, Value::Blob(ref b) if b == &[1, 2, 3]));
    }

    #[test]
    fn test_large_integer() {
        let large = (1i64 << 53) + 1;
        let v = Value::Integer(large);
        if let Value::Integer(i) = v {
            assert_eq!(i, large);
            assert_ne!(i as f64 as i64, i);
        } else {
            panic!("Expected Integer");
        }
    }

    #[test]
    fn test_value_ref_conversions() {
        let values = vec![
            Value::Null,
            Value::Integer(42),
            Value::Real(3.14),
            Value::Text("hello".to_string()),
            Value::Blob(vec![1, 2, 3]),
        ];
        for v in &values {
            let _ref = match v {
                Value::Null => ValueRef::Null,
                Value::Integer(i) => ValueRef::Integer(*i),
                Value::Real(f) => ValueRef::Real(*f),
                Value::Text(s) => ValueRef::Text(s.as_bytes()),
                Value::Blob(b) => ValueRef::Blob(b),
            };
        }
    }
}
