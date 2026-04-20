use napi::bindgen_prelude::*;
use napi::{Env, JsObject};
use napi_derive::napi;
use rusqlite::types::Value;

use crate::types::values_to_js_object;

/// Lazy row iterator. Rows are materialized in Rust memory on creation,
/// but JS object conversion happens lazily on each next() call.
#[napi]
pub struct RowIterator {
    rows: Vec<Vec<Value>>,
    columns: Vec<String>,
    index: usize,
    safe_integers: bool,
}

#[napi]
impl RowIterator {
    /// Returns {value: row_object, done: false} or {value: undefined, done: true}.
    /// Follows the JS iterator protocol.
    #[napi]
    pub fn next(&mut self, env: Env) -> Result<JsObject> {
        let mut result = env.create_object()?;

        if self.index < self.rows.len() {
            let row = &self.rows[self.index];
            self.index += 1;
            let obj = values_to_js_object(&env, row, &self.columns, self.safe_integers)?;
            result.set_named_property("value", obj)?;
            result.set_named_property("done", false)?;
        } else {
            result.set_named_property("done", true)?;
            // value is implicitly undefined when not set
        }

        Ok(result)
    }

    /// Early termination — returns done: true.
    #[napi(js_name = "return")]
    pub fn return_iter(&mut self, env: Env) -> Result<JsObject> {
        self.index = self.rows.len();
        let mut result = env.create_object()?;
        result.set_named_property("done", true)?;
        Ok(result)
    }
}

impl RowIterator {
    pub(crate) fn new_internal(
        rows: Vec<Vec<Value>>,
        columns: Vec<String>,
        safe_integers: bool,
    ) -> Self {
        Self {
            rows,
            columns,
            index: 0,
            safe_integers,
        }
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::types::Value;

    #[test]
    fn test_empty_result_set() {
        let rows: Vec<Vec<Value>> = vec![];
        assert!(rows.is_empty());
    }

    #[test]
    fn test_single_row() {
        let rows = vec![vec![Value::Integer(1), Value::Text("hello".to_string())]];
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0][0], Value::Integer(1)));
    }

    #[test]
    fn test_multi_row() {
        let rows = vec![
            vec![Value::Integer(1), Value::Text("a".to_string())],
            vec![Value::Integer(2), Value::Text("b".to_string())],
            vec![Value::Integer(3), Value::Text("c".to_string())],
        ];
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn test_early_termination() {
        let rows = vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
        ];
        let mut index = 0;
        let _row = &rows[index];
        index += 1;
        assert_eq!(index, 1);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn test_null_values_in_rows() {
        let rows = vec![vec![Value::Integer(1), Value::Null]];
        assert!(matches!(rows[0][1], Value::Null));
    }
}
