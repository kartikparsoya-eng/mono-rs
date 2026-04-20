use std::collections::HashMap;

use napi::bindgen_prelude::*;
use napi::{Env, JsObject, NapiRaw};
use napi_derive::napi;
use serde_json;

// ─── Value type matching JS Value ───────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

impl Value {
    /// Parse a serde_json::Value into our Value enum
    fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) => Value::String(s.clone()),
            _ => Value::Null,
        }
    }
}

// ─── Predicate AST ──────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Predicate {
    Eq(String, Value),
    Neq(String, Value),
    Gt(String, Value),
    Gte(String, Value),
    Lt(String, Value),
    Lte(String, Value),
    In(String, Vec<Value>),
    Like(String, String),
    IsNull(String),
    IsNotNull(String),
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
}

impl Predicate {
    /// Parse from a JSON value. Expected format:
    /// { "op": "eq", "field": "name", "value": "foo" }
    /// { "op": "and", "conditions": [...] }
    /// { "op": "in", "field": "id", "values": [1, 2, 3] }
    pub fn from_json(v: &serde_json::Value) -> Result<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| Error::from_reason("Predicate must be an object"))?;

        let op = obj
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::from_reason("Predicate must have 'op' field"))?;

        match op {
            "eq" | "neq" | "gt" | "gte" | "lt" | "lte" => {
                let field = get_field(obj)?;
                let value = get_value(obj)?;
                Ok(match op {
                    "eq" => Predicate::Eq(field, value),
                    "neq" => Predicate::Neq(field, value),
                    "gt" => Predicate::Gt(field, value),
                    "gte" => Predicate::Gte(field, value),
                    "lt" => Predicate::Lt(field, value),
                    "lte" => Predicate::Lte(field, value),
                    _ => unreachable!(),
                })
            }
            "in" => {
                let field = get_field(obj)?;
                let values = obj
                    .get("values")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| Error::from_reason("'in' predicate must have 'values' array"))?
                    .iter()
                    .map(Value::from_json)
                    .collect();
                Ok(Predicate::In(field, values))
            }
            "like" => {
                let field = get_field(obj)?;
                let pattern = obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::from_reason("'like' predicate must have string 'value'"))?
                    .to_string();
                Ok(Predicate::Like(field, pattern))
            }
            "isNull" => Ok(Predicate::IsNull(get_field(obj)?)),
            "isNotNull" => Ok(Predicate::IsNotNull(get_field(obj)?)),
            "and" => {
                let conditions = get_conditions(obj)?;
                Ok(Predicate::And(conditions))
            }
            "or" => {
                let conditions = get_conditions(obj)?;
                Ok(Predicate::Or(conditions))
            }
            "not" => {
                let inner = obj
                    .get("condition")
                    .ok_or_else(|| Error::from_reason("'not' predicate must have 'condition'"))?;
                Ok(Predicate::Not(Box::new(Predicate::from_json(inner)?)))
            }
            _ => Err(Error::from_reason(format!("Unknown predicate op: {}", op))),
        }
    }
}

fn get_field(obj: &serde_json::Map<String, serde_json::Value>) -> Result<String> {
    obj.get("field")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::from_reason("Predicate must have 'field'"))
}

fn get_value(obj: &serde_json::Map<String, serde_json::Value>) -> Result<Value> {
    obj.get("value")
        .map(Value::from_json)
        .ok_or_else(|| Error::from_reason("Predicate must have 'value'"))
}

fn get_conditions(obj: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<Predicate>> {
    obj.get("conditions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::from_reason("Must have 'conditions' array"))?
        .iter()
        .map(Predicate::from_json)
        .collect()
}

// ─── Evaluation ─────────────────────────────────────────────────────────────

pub fn evaluate(predicate: &Predicate, row: &HashMap<String, Value>) -> bool {
    match predicate {
        Predicate::Eq(field, value) => row.get(field).map_or(false, |v| v == value),
        Predicate::Neq(field, value) => row.get(field).map_or(true, |v| v != value),
        Predicate::Gt(field, value) => {
            row.get(field).map_or(false, |v| compare_values(v, value) == std::cmp::Ordering::Greater)
        }
        Predicate::Gte(field, value) => {
            row.get(field).map_or(false, |v| compare_values(v, value) != std::cmp::Ordering::Less)
        }
        Predicate::Lt(field, value) => {
            row.get(field).map_or(false, |v| compare_values(v, value) == std::cmp::Ordering::Less)
        }
        Predicate::Lte(field, value) => {
            row.get(field).map_or(false, |v| compare_values(v, value) != std::cmp::Ordering::Greater)
        }
        Predicate::In(field, values) => row.get(field).map_or(false, |v| values.contains(v)),
        Predicate::Like(field, pattern) => {
            row.get(field).map_or(false, |v| match v {
                Value::String(s) => like_match(s, pattern),
                _ => false,
            })
        }
        Predicate::IsNull(field) => {
            row.get(field).map_or(true, |v| *v == Value::Null)
        }
        Predicate::IsNotNull(field) => {
            row.get(field).map_or(false, |v| *v != Value::Null)
        }
        Predicate::And(conditions) => conditions.iter().all(|c| evaluate(c, row)),
        Predicate::Or(conditions) => conditions.iter().any(|c| evaluate(c, row)),
        Predicate::Not(condition) => !evaluate(condition, row),
    }
}

/// Compare two Values. null < everything else. Same-type comparisons.
pub fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        // Cross-type: treat as equal (shouldn't happen in practice)
        _ => Ordering::Equal,
    }
}

/// SQL LIKE pattern matching. % matches any sequence, _ matches any single char.
pub fn like_match(text: &str, pattern: &str) -> bool {
    let text_chars: Vec<char> = text.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    like_match_impl(&text_chars, &pattern_chars, 0, 0)
}

fn like_match_impl(text: &[char], pattern: &[char], ti: usize, pi: usize) -> bool {
    if pi == pattern.len() {
        return ti == text.len();
    }
    match pattern[pi] {
        '%' => {
            // % matches zero or more characters
            for i in ti..=text.len() {
                if like_match_impl(text, pattern, i, pi + 1) {
                    return true;
                }
            }
            false
        }
        '_' => {
            // _ matches exactly one character
            if ti < text.len() {
                like_match_impl(text, pattern, ti + 1, pi + 1)
            } else {
                false
            }
        }
        c => {
            // Case-insensitive comparison (SQL LIKE is typically case-insensitive for ASCII)
            if ti < text.len() && text[ti].to_ascii_lowercase() == c.to_ascii_lowercase() {
                like_match_impl(text, pattern, ti + 1, pi + 1)
            } else {
                false
            }
        }
    }
}

// ─── napi Binding ───────────────────────────────────────────────────────────

/// Change types matching TS: ADD=0, REMOVE=1, EDIT=2, CHILD=3
const CHANGE_ADD: u32 = 0;
const CHANGE_REMOVE: u32 = 1;
const CHANGE_EDIT: u32 = 2;
const CHANGE_CHILD: u32 = 3;

/// Result of filtering an edit change
#[napi(string_enum)]
pub enum EditSplitType {
    /// Old matched, new matches → pass through as edit
    Both,
    /// Old matched, new doesn't → emit as remove (of old node)
    Remove,
    /// Old didn't match, new does → emit as add (of new node)
    Add,
}

#[napi(object)]
pub struct EditSplitInfo {
    pub index: u32,
    pub split_type: EditSplitType,
}

#[napi(object)]
pub struct FilterResult {
    /// Indices of changes that pass through unchanged
    pub passed: Vec<u32>,
    /// Edit changes that need splitting
    pub splits: Vec<EditSplitInfo>,
}

#[napi]
pub struct RustFilterPredicate {
    predicate: Predicate,
    /// Fields referenced by this predicate (for efficient row extraction)
    fields: Vec<String>,
}

impl RustFilterPredicate {
    fn collect_fields(pred: &Predicate) -> Vec<String> {
        let mut fields = Vec::new();
        Self::collect_fields_impl(pred, &mut fields);
        fields.sort();
        fields.dedup();
        fields
    }

    fn collect_fields_impl(pred: &Predicate, fields: &mut Vec<String>) {
        match pred {
            Predicate::Eq(f, _)
            | Predicate::Neq(f, _)
            | Predicate::Gt(f, _)
            | Predicate::Gte(f, _)
            | Predicate::Lt(f, _)
            | Predicate::Lte(f, _)
            | Predicate::In(f, _)
            | Predicate::Like(f, _)
            | Predicate::IsNull(f)
            | Predicate::IsNotNull(f) => {
                fields.push(f.clone());
            }
            Predicate::And(conds) | Predicate::Or(conds) => {
                for c in conds {
                    Self::collect_fields_impl(c, fields);
                }
            }
            Predicate::Not(c) => Self::collect_fields_impl(c, fields),
        }
    }
}

#[napi]
impl RustFilterPredicate {
    #[napi(constructor)]
    pub fn new(predicate_json: String) -> Result<Self> {
        let json_value: serde_json::Value = serde_json::from_str(&predicate_json)
            .map_err(|e| Error::from_reason(format!("Invalid predicate JSON: {}", e)))?;
        let predicate = Predicate::from_json(&json_value)?;
        let fields = Self::collect_fields(&predicate);
        Ok(Self { predicate, fields })
    }

    /// Evaluate a single row (as a JS object) against the predicate.
    /// Returns true if the row passes the filter.
    #[napi]
    pub fn evaluate_row(&self, env: Env, row: JsObject) -> Result<bool> {
        let map = self.extract_row_fields(&env, &row)?;
        Ok(evaluate(&self.predicate, &map))
    }

    /// Filter a batch of changes. Returns which pass and which edits need splitting.
    ///
    /// Changes are JS arrays: [type, node, oldNode/childData/null]
    /// Node is: { row: {...}, relationships: {...} }
    /// We only read node.row fields referenced by the predicate.
    #[napi]
    pub fn filter_push_batch(&self, env: Env, changes: JsObject) -> Result<FilterResult> {
        let mut passed: Vec<u32> = Vec::new();
        let mut splits: Vec<EditSplitInfo> = Vec::new();

        let len = get_array_length(&env, &changes)?;

        for i in 0..len {
            let change: JsObject = changes.get_element(i)?;

            // change[0] = type
            let change_type = get_element_u32(&env, &change, 0)?;

            match change_type {
                CHANGE_ADD | CHANGE_REMOVE | CHANGE_CHILD => {
                    // change[1] = node
                    let node: JsObject = change.get_element(1)?;
                    let row: JsObject = node.get_named_property("row")?;
                    let map = self.extract_row_fields(&env, &row)?;
                    if evaluate(&self.predicate, &map) {
                        passed.push(i);
                    }
                }
                CHANGE_EDIT => {
                    // change[1] = new node, change[2] = old node
                    let new_node: JsObject = change.get_element(1)?;
                    let old_node: JsObject = change.get_element(2)?;

                    let new_row: JsObject = new_node.get_named_property("row")?;
                    let old_row: JsObject = old_node.get_named_property("row")?;

                    let new_map = self.extract_row_fields(&env, &new_row)?;
                    let old_map = self.extract_row_fields(&env, &old_row)?;

                    let old_matches = evaluate(&self.predicate, &old_map);
                    let new_matches = evaluate(&self.predicate, &new_map);

                    if old_matches && new_matches {
                        splits.push(EditSplitInfo {
                            index: i,
                            split_type: EditSplitType::Both,
                        });
                    } else if old_matches && !new_matches {
                        splits.push(EditSplitInfo {
                            index: i,
                            split_type: EditSplitType::Remove,
                        });
                    } else if !old_matches && new_matches {
                        splits.push(EditSplitInfo {
                            index: i,
                            split_type: EditSplitType::Add,
                        });
                    }
                    // Neither matches → drop
                }
                _ => {
                    // Unknown type, pass through
                    passed.push(i);
                }
            }
        }

        Ok(FilterResult { passed, splits })
    }

    /// Extract only the fields we need from a row JS object
    fn extract_row_fields(&self, env: &Env, row: &JsObject) -> Result<HashMap<String, Value>> {
        let mut map = HashMap::with_capacity(self.fields.len());
        for field in &self.fields {
            let value = get_property_value(env, row, field)?;
            map.insert(field.clone(), value);
        }
        Ok(map)
    }
}

// ─── Raw napi helpers ───────────────────────────────────────────────────────

/// Get the length of a JS array
fn get_array_length(env: &Env, arr: &JsObject) -> Result<u32> {
    let mut len: u32 = 0;
    let status = unsafe { napi::sys::napi_get_array_length(env.raw(), arr.raw(), &mut len) };
    if status != 0 {
        return Err(Error::from_reason("Failed to get array length"));
    }
    Ok(len)
}

/// Get an element from an array as u32 (for change type)
fn get_element_u32(env: &Env, arr: &JsObject, index: u32) -> Result<u32> {
    let mut result = std::ptr::null_mut();
    let status =
        unsafe { napi::sys::napi_get_element(env.raw(), arr.raw(), index, &mut result) };
    if status != 0 {
        return Err(Error::from_reason("Failed to get element"));
    }
    let mut val: f64 = 0.0;
    let status = unsafe { napi::sys::napi_get_value_double(env.raw(), result, &mut val) };
    if status != 0 {
        return Err(Error::from_reason("Failed to coerce element to number"));
    }
    Ok(val as u32)
}

/// Get a named property from a JS object and convert to our Value type
fn get_property_value(env: &Env, obj: &JsObject, key: &str) -> Result<Value> {
    // Get the property raw value
    let key_cstr = std::ffi::CString::new(key)
        .map_err(|_| Error::from_reason("Invalid key"))?;
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        napi::sys::napi_get_named_property(env.raw(), obj.raw(), key_cstr.as_ptr(), &mut result)
    };
    if status != 0 {
        return Ok(Value::Null);
    }

    // Get type
    let mut value_type: napi::sys::napi_valuetype = 0;
    let status = unsafe { napi::sys::napi_typeof(env.raw(), result, &mut value_type) };
    if status != 0 {
        return Ok(Value::Null);
    }

    match value_type {
        // napi_undefined = 0, napi_null = 1
        0 | 1 => Ok(Value::Null),
        // napi_boolean = 2
        2 => {
            let mut val = false;
            unsafe { napi::sys::napi_get_value_bool(env.raw(), result, &mut val) };
            Ok(Value::Bool(val))
        }
        // napi_number = 3
        3 => {
            let mut val: f64 = 0.0;
            unsafe { napi::sys::napi_get_value_double(env.raw(), result, &mut val) };
            Ok(Value::Number(val))
        }
        // napi_string = 4
        4 => {
            let mut len: usize = 0;
            unsafe {
                napi::sys::napi_get_value_string_utf8(
                    env.raw(),
                    result,
                    std::ptr::null_mut(),
                    0,
                    &mut len,
                )
            };
            let mut buf = vec![0u8; len + 1];
            let mut written: usize = 0;
            unsafe {
                napi::sys::napi_get_value_string_utf8(
                    env.raw(),
                    result,
                    buf.as_mut_ptr() as *mut i8,
                    len + 1,
                    &mut written,
                )
            };
            let s = std::str::from_utf8(&buf[..written])
                .map_err(|_| Error::from_reason("Invalid UTF-8"))?
                .to_string();
            Ok(Value::String(s))
        }
        _ => Ok(Value::Null),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn test_eq_string() {
        let pred = Predicate::Eq("name".into(), Value::String("Alice".into()));
        let row = make_row(&[("name", Value::String("Alice".into()))]);
        assert!(evaluate(&pred, &row));

        let row2 = make_row(&[("name", Value::String("Bob".into()))]);
        assert!(!evaluate(&pred, &row2));
    }

    #[test]
    fn test_eq_number() {
        let pred = Predicate::Eq("age".into(), Value::Number(30.0));
        let row = make_row(&[("age", Value::Number(30.0))]);
        assert!(evaluate(&pred, &row));

        let row2 = make_row(&[("age", Value::Number(25.0))]);
        assert!(!evaluate(&pred, &row2));
    }

    #[test]
    fn test_neq() {
        let pred = Predicate::Neq("status".into(), Value::String("deleted".into()));
        let row = make_row(&[("status", Value::String("active".into()))]);
        assert!(evaluate(&pred, &row));

        let row2 = make_row(&[("status", Value::String("deleted".into()))]);
        assert!(!evaluate(&pred, &row2));
    }

    #[test]
    fn test_gt_lt() {
        let pred = Predicate::Gt("age".into(), Value::Number(18.0));
        let row = make_row(&[("age", Value::Number(21.0))]);
        assert!(evaluate(&pred, &row));

        let row2 = make_row(&[("age", Value::Number(18.0))]);
        assert!(!evaluate(&pred, &row2));

        let pred_lt = Predicate::Lt("age".into(), Value::Number(18.0));
        let row3 = make_row(&[("age", Value::Number(15.0))]);
        assert!(evaluate(&pred_lt, &row3));
    }

    #[test]
    fn test_gte_lte() {
        let pred = Predicate::Gte("age".into(), Value::Number(18.0));
        assert!(evaluate(&pred, &make_row(&[("age", Value::Number(18.0))])));
        assert!(evaluate(&pred, &make_row(&[("age", Value::Number(19.0))])));
        assert!(!evaluate(&pred, &make_row(&[("age", Value::Number(17.0))])));

        let pred = Predicate::Lte("age".into(), Value::Number(18.0));
        assert!(evaluate(&pred, &make_row(&[("age", Value::Number(18.0))])));
        assert!(evaluate(&pred, &make_row(&[("age", Value::Number(17.0))])));
        assert!(!evaluate(&pred, &make_row(&[("age", Value::Number(19.0))])));
    }

    #[test]
    fn test_in() {
        let pred = Predicate::In(
            "status".into(),
            vec![
                Value::String("active".into()),
                Value::String("pending".into()),
            ],
        );
        assert!(evaluate(&pred, &make_row(&[("status", Value::String("active".into()))])));
        assert!(evaluate(&pred, &make_row(&[("status", Value::String("pending".into()))])));
        assert!(!evaluate(&pred, &make_row(&[("status", Value::String("deleted".into()))])));
    }

    #[test]
    fn test_and_or() {
        let pred = Predicate::And(vec![
            Predicate::Eq("name".into(), Value::String("Alice".into())),
            Predicate::Gt("age".into(), Value::Number(18.0)),
        ]);
        let row = make_row(&[
            ("name", Value::String("Alice".into())),
            ("age", Value::Number(25.0)),
        ]);
        assert!(evaluate(&pred, &row));

        let row2 = make_row(&[
            ("name", Value::String("Alice".into())),
            ("age", Value::Number(16.0)),
        ]);
        assert!(!evaluate(&pred, &row2));

        let pred_or = Predicate::Or(vec![
            Predicate::Eq("name".into(), Value::String("Alice".into())),
            Predicate::Eq("name".into(), Value::String("Bob".into())),
        ]);
        assert!(evaluate(&pred_or, &make_row(&[("name", Value::String("Alice".into()))])));
        assert!(evaluate(&pred_or, &make_row(&[("name", Value::String("Bob".into()))])));
        assert!(!evaluate(&pred_or, &make_row(&[("name", Value::String("Carol".into()))])));
    }

    #[test]
    fn test_not() {
        let pred = Predicate::Not(Box::new(Predicate::Eq("x".into(), Value::Number(1.0))));
        assert!(evaluate(&pred, &make_row(&[("x", Value::Number(2.0))])));
        assert!(!evaluate(&pred, &make_row(&[("x", Value::Number(1.0))])));
    }

    #[test]
    fn test_like_pattern() {
        assert!(like_match("hello world", "%world"));
        assert!(like_match("hello world", "hello%"));
        assert!(like_match("hello world", "%lo wo%"));
        assert!(like_match("hello world", "hello_world"));
        assert!(!like_match("hello world", "hello_worlds"));
        assert!(like_match("abc", "a_c"));
        assert!(!like_match("abbc", "a_c"));
        assert!(like_match("", "%"));
        assert!(!like_match("", "_"));
    }

    #[test]
    fn test_is_null() {
        let pred = Predicate::IsNull("x".into());
        assert!(evaluate(&pred, &make_row(&[("x", Value::Null)])));
        assert!(!evaluate(&pred, &make_row(&[("x", Value::Number(1.0))])));
        // Missing field → treated as null
        assert!(evaluate(&pred, &make_row(&[])));
    }

    #[test]
    fn test_is_not_null() {
        let pred = Predicate::IsNotNull("x".into());
        assert!(!evaluate(&pred, &make_row(&[("x", Value::Null)])));
        assert!(evaluate(&pred, &make_row(&[("x", Value::Number(1.0))])));
        assert!(!evaluate(&pred, &make_row(&[])));
    }

    #[test]
    fn test_edit_split_logic() {
        // Simulating: row changes from matching to not matching = remove
        let pred = Predicate::Eq("status".into(), Value::String("active".into()));
        let old_row = make_row(&[("status", Value::String("active".into()))]);
        let new_row = make_row(&[("status", Value::String("deleted".into()))]);

        let old_matches = evaluate(&pred, &old_row);
        let new_matches = evaluate(&pred, &new_row);
        assert!(old_matches);
        assert!(!new_matches);
        // → should split to remove

        // Row changes from not matching to matching = add
        let old_row2 = make_row(&[("status", Value::String("deleted".into()))]);
        let new_row2 = make_row(&[("status", Value::String("active".into()))]);
        assert!(!evaluate(&pred, &old_row2));
        assert!(evaluate(&pred, &new_row2));
        // → should split to add

        // Both match = pass through
        let old_row3 = make_row(&[("status", Value::String("active".into()))]);
        let new_row3 = make_row(&[("status", Value::String("active".into()))]);
        assert!(evaluate(&pred, &old_row3));
        assert!(evaluate(&pred, &new_row3));
    }

    #[test]
    fn test_predicate_from_json() {
        let json = r#"{"op": "eq", "field": "name", "value": "Alice"}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let pred = Predicate::from_json(&v).unwrap();
        let row = make_row(&[("name", Value::String("Alice".into()))]);
        assert!(evaluate(&pred, &row));
    }

    #[test]
    fn test_predicate_and_from_json() {
        let json = r#"{
            "op": "and",
            "conditions": [
                {"op": "eq", "field": "name", "value": "Alice"},
                {"op": "gt", "field": "age", "value": 18}
            ]
        }"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let pred = Predicate::from_json(&v).unwrap();
        let row = make_row(&[
            ("name", Value::String("Alice".into())),
            ("age", Value::Number(25.0)),
        ]);
        assert!(evaluate(&pred, &row));
    }

    #[test]
    fn test_compare_values_null() {
        assert_eq!(compare_values(&Value::Null, &Value::Null), std::cmp::Ordering::Equal);
        assert_eq!(compare_values(&Value::Null, &Value::Number(1.0)), std::cmp::Ordering::Less);
        assert_eq!(compare_values(&Value::Number(1.0), &Value::Null), std::cmp::Ordering::Greater);
    }
}
