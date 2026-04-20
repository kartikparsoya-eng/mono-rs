use napi::bindgen_prelude::*;
use napi::{Env, JsObject, NapiRaw};
use rusqlite::types::{Value, ValueRef};
use rusqlite::Row;

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
                    buf.as_mut_ptr() as *mut i8,
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

/// Convert a rusqlite Row to a JS object with column names as keys.
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
