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
    /// Integer values preserved with full i64 precision.
    /// SQLite-sourced PKs > 2^53 (e.g. snowflake IDs) require this to avoid
    /// precision loss that would collapse distinct keys when coerced to f64.
    Int(i64),
    Number(f64),
    String(String),
}

impl Value {
    /// Parse a serde_json::Value into our Value enum.
    /// Numbers are dispatched to Int first if they fit in i64; otherwise f64.
    /// This preserves precision for values outside f64's safe-integer range
    /// (|n| > 2^53), which matters for snowflake-style PKs.
    pub fn from_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Number(f)
                } else {
                    Value::Number(0.0)
                }
            }
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
    Like(String, String, bool),
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
            "like" | "ilike" => {
                let case_insensitive = op == "ilike";
                let field = get_field(obj)?;
                let pattern = obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::from_reason("'like' predicate must have string 'value'"))?
                    .to_string();
                Ok(Predicate::Like(field, pattern, case_insensitive))
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

/// Compare two Values with boolean/number coercion.
/// SQLite stores booleans as 0/1 (Number), but the AST uses Bool.
/// This function handles cross-type comparisons.
fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(ab), Value::Number(n)) | (Value::Number(n), Value::Bool(ab)) => {
            let bool_as_num = if *ab { 1.0 } else { 0.0 };
            *n == bool_as_num
        }
        (Value::Bool(ab), Value::Int(i)) | (Value::Int(i), Value::Bool(ab)) => {
            let bool_as_int: i64 = if *ab { 1 } else { 0 };
            *i == bool_as_int
        }
        // Mixed Int/Number: compare as f64 (lossy for |i| > 2^53 but consistent
        // with SQL semantics where REAL and INTEGER compare numerically).
        (Value::Int(i), Value::Number(n)) | (Value::Number(n), Value::Int(i)) => {
            (*i as f64) == *n
        }
        _ => a == b,
    }
}

pub fn evaluate(predicate: &Predicate, row: &HashMap<String, Value>) -> bool {
    match predicate {
        Predicate::Eq(field, value) => row.get(field).map_or(false, |v| {
            if *v == Value::Null { return false; }
            values_eq(v, value)
        }),
        Predicate::Neq(field, value) => row.get(field).map_or(false, |v| {
            if *v == Value::Null { return false; }
            !values_eq(v, value)
        }),
        Predicate::Gt(field, value) => {
            row.get(field).map_or(false, |v| {
                if *v == Value::Null { return false; }
                compare_values(v, value) == std::cmp::Ordering::Greater
            })
        }
        Predicate::Gte(field, value) => {
            row.get(field).map_or(false, |v| {
                if *v == Value::Null { return false; }
                compare_values(v, value) != std::cmp::Ordering::Less
            })
        }
        Predicate::Lt(field, value) => {
            row.get(field).map_or(false, |v| {
                if *v == Value::Null { return false; }
                compare_values(v, value) == std::cmp::Ordering::Less
            })
        }
        Predicate::Lte(field, value) => {
            row.get(field).map_or(false, |v| {
                if *v == Value::Null { return false; }
                compare_values(v, value) != std::cmp::Ordering::Greater
            })
        }
        Predicate::In(field, values) => row.get(field).map_or(false, |v| {
            if *v == Value::Null { return false; }
            values.iter().any(|val| values_eq(v, val))
        }),
        Predicate::Like(field, pattern, ci) => {
            row.get(field).map_or(false, |v| match v {
                Value::String(s) => like_match(s, pattern, *ci),
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
        Predicate::Not(condition) => {
            if is_null_for_row(condition, row) {
                return false;
            }
            !evaluate(condition, row)
        },
    }
}

/// Returns true if the predicate references a field whose row value is NULL/missing.
/// Used by `Not` to propagate SQL NULL semantics (NOT NULL = NULL → false).
fn is_null_for_row(predicate: &Predicate, row: &HashMap<String, Value>) -> bool {
    match predicate {
        Predicate::Eq(f, _)
        | Predicate::Neq(f, _)
        | Predicate::Gt(f, _)
        | Predicate::Gte(f, _)
        | Predicate::Lt(f, _)
        | Predicate::Lte(f, _)
        | Predicate::In(f, _)
        | Predicate::Like(f, _, _) => {
            matches!(row.get(f), None | Some(Value::Null))
        }
        Predicate::IsNull(_) | Predicate::IsNotNull(_) => false,
        Predicate::And(conds) => conds.iter().any(|c| is_null_for_row(c, row)),
        Predicate::Or(conds) => conds.iter().any(|c| is_null_for_row(c, row)),
        Predicate::Not(inner) => is_null_for_row(inner, row),
    }
}

/// Evaluate a predicate against a row of serde_json::Value (used by ExistsOperator).
pub fn evaluate_json_row(predicate: &Predicate, row: &serde_json::Map<String, serde_json::Value>) -> bool {
    let converted: HashMap<String, Value> = row.iter()
        .map(|(k, v)| (k.clone(), Value::from_json(v)))
        .collect();
    evaluate(predicate, &converted)
}

/// Compare two Values. null < everything else. Same-type comparisons.
///
/// Int/Int: full-precision i64 ordering (preserves snowflake IDs > 2^53).
/// Number/Number: f64 partial_cmp.
/// Mixed Int/Number: convert i64 → f64 then partial_cmp (lossy for |i| > 2^53,
/// but mixed types only arise from heterogeneous JSON input where we cannot
/// avoid the conversion).
pub fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Value::Int(i), Value::Number(n)) => {
            (*i as f64).partial_cmp(n).unwrap_or(Ordering::Equal)
        }
        (Value::Number(n), Value::Int(i)) => {
            n.partial_cmp(&(*i as f64)).unwrap_or(Ordering::Equal)
        }
        (Value::String(a), Value::String(b)) => a.cmp(b),
        // Cross-type: treat as equal (shouldn't happen in practice)
        _ => Ordering::Equal,
    }
}

/// SQL LIKE pattern matching. % matches any sequence, _ matches any single char.
pub fn like_match(text: &str, pattern: &str, case_insensitive: bool) -> bool {
    let text_chars: Vec<char> = text.chars().collect();
    let pattern_chars: Vec<char> = pattern.chars().collect();
    like_match_impl(&text_chars, &pattern_chars, 0, 0, case_insensitive)
}

/// Iterative two-pointer LIKE matching — O(N*M) worst case.
/// Tracks the last '%' position to backtrack greedily instead of recursing.
fn like_match_impl(text: &[char], pattern: &[char], mut ti: usize, mut pi: usize, case_insensitive: bool) -> bool {
    let (tlen, plen) = (text.len(), pattern.len());
    let mut star_pi: Option<usize> = None; // pattern index after last '%'
    let mut star_ti: usize = 0; // text index when we matched last '%'

    while ti < tlen || pi < plen {
        if pi < plen {
            match pattern[pi] {
                '%' => {
                    star_pi = Some(pi + 1);
                    star_ti = ti;
                    pi += 1;
                    continue;
                }
                '_' if ti < tlen => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                c if ti < tlen && if case_insensitive {
                    text[ti].to_ascii_lowercase() == c.to_ascii_lowercase()
                } else {
                    text[ti] == c
                } =>
                {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                _ => {} // mismatch — fall through to backtrack
            }
        }
        // Mismatch or pattern exhausted with text remaining: backtrack to last '%'
        if let Some(sp) = star_pi {
            star_ti += 1;
            if star_ti > tlen {
                return false; // text exhausted, no match possible
            }
            ti = star_ti;
            pi = sp;
        } else {
            return false;
        }
    }
    true
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
            | Predicate::Like(f, _, _)
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
                    buf.as_mut_ptr() as *mut std::ffi::c_char,
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
        // Case-sensitive (LIKE)
        assert!(like_match("hello world", "%world", false));
        assert!(like_match("hello world", "hello%", false));
        assert!(like_match("hello world", "%lo wo%", false));
        assert!(like_match("hello world", "hello_world", false));
        assert!(!like_match("hello world", "hello_worlds", false));
        assert!(like_match("abc", "a_c", false));
        assert!(!like_match("abbc", "a_c", false));
        assert!(like_match("", "%", false));
        assert!(!like_match("", "_", false));

        // Case-sensitive: must respect case
        assert!(!like_match("Hello", "hello", false));
        assert!(!like_match("ABC", "abc", false));
        assert!(like_match("ABC", "ABC", false));

        // Case-insensitive (ILIKE)
        assert!(like_match("Hello", "hello", true));
        assert!(like_match("ABC", "abc", true));
        assert!(like_match("Hello World", "%world", true));
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

    #[test]
    fn test_in_bool_number_coercion() {
        // SQLite stores booleans as 0/1, but the AST may use Bool.
        // The In predicate must coerce true<>1 and false<>0.
        let pred = Predicate::In(
            "active".into(),
            vec![Value::Bool(true)],
        );
        // Row has Number(1) - should match Bool(true) via coercion
        assert!(evaluate(&pred, &make_row(&[("active", Value::Number(1.0))])));
        // Row has Number(0) - should NOT match Bool(true)
        assert!(!evaluate(&pred, &make_row(&[("active", Value::Number(0.0))])));

        // Reverse: predicate has Number(1), row has Bool(true)
        let pred2 = Predicate::In(
            "active".into(),
            vec![Value::Number(1.0)],
        );
        assert!(evaluate(&pred2, &make_row(&[("active", Value::Bool(true))])));
        assert!(!evaluate(&pred2, &make_row(&[("active", Value::Bool(false))])));

        // false<>0 coercion
        let pred3 = Predicate::In(
            "active".into(),
            vec![Value::Bool(false)],
        );
        assert!(evaluate(&pred3, &make_row(&[("active", Value::Number(0.0))])));
        assert!(!evaluate(&pred3, &make_row(&[("active", Value::Number(1.0))])));

        // Eq also uses values_eq
        let pred_eq = Predicate::Eq("active".into(), Value::Bool(true));
        assert!(evaluate(&pred_eq, &make_row(&[("active", Value::Number(1.0))])));
        assert!(!evaluate(&pred_eq, &make_row(&[("active", Value::Number(0.0))])));
    }

    #[test]
    fn test_int_precision_above_2_pow_53() {
        // Two distinct i64 values that collapse to the same f64.
        // f64 has 53 bits of mantissa, so integers in [2^53, 2^54) can only
        // represent every other value — odd ones round to the nearest even.
        // 2^53 (= 9007199254740992) is exact; 2^53+1 rounds to 2^53.
        // Without preserving i64 precision, these distinct PKs collapse to equal.
        let a: i64 = 9_007_199_254_740_992; // 2^53, exact in f64
        let b: i64 = 9_007_199_254_740_993; // 2^53 + 1, rounds to 2^53 in f64

        // Sanity check: confirm these collapse under f64 conversion (the bug).
        assert_eq!(a as f64, b as f64, "f64 cast must be lossy here");

        // With Value::Int variant, ordering and equality must be preserved.
        let va = Value::Int(a);
        let vb = Value::Int(b);
        assert_eq!(compare_values(&va, &vb), std::cmp::Ordering::Less);
        assert_eq!(compare_values(&vb, &va), std::cmp::Ordering::Greater);
        assert_ne!(va, vb, "distinct i64 values must not be equal");

        // from_json must dispatch to Int (not Number) for integer JSON.
        let ja = serde_json::json!(a);
        let jb = serde_json::json!(b);
        let parsed_a = Value::from_json(&ja);
        let parsed_b = Value::from_json(&jb);
        assert_eq!(parsed_a, Value::Int(a));
        assert_eq!(parsed_b, Value::Int(b));
        assert_eq!(
            compare_values(&parsed_a, &parsed_b),
            std::cmp::Ordering::Less
        );

        // Sort a vec of these values and verify ordering.
        let mut vs = vec![Value::Int(b), Value::Int(a)];
        vs.sort_by(|x, y| compare_values(x, y));
        assert_eq!(vs, vec![Value::Int(a), Value::Int(b)]);

        // Also test a more extreme case: i64::MAX and i64::MAX - 1 both
        // overflow to 2^63 in f64.
        let big_a: i64 = i64::MAX;
        let big_b: i64 = i64::MAX - 1;
        assert_eq!(big_a as f64, big_b as f64);
        assert_eq!(
            compare_values(&Value::Int(big_b), &Value::Int(big_a)),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn test_int_number_cross_type_eq() {
        // Boolean coercion still works with Int (SQLite stores bool as INTEGER 0/1).
        let pred = Predicate::Eq("active".into(), Value::Bool(true));
        assert!(evaluate(&pred, &make_row(&[("active", Value::Int(1))])));
        assert!(!evaluate(&pred, &make_row(&[("active", Value::Int(0))])));

        // Int/Number cross-type equality (within f64 safe range).
        let pred = Predicate::Eq("n".into(), Value::Int(42));
        assert!(evaluate(&pred, &make_row(&[("n", Value::Number(42.0))])));

        // In predicate with mixed Int/Number list.
        let pred = Predicate::In(
            "id".into(),
            vec![Value::Int(1), Value::Int(2), Value::Number(3.0)],
        );
        assert!(evaluate(&pred, &make_row(&[("id", Value::Int(2))])));
        assert!(evaluate(&pred, &make_row(&[("id", Value::Number(3.0))])));
        assert!(!evaluate(&pred, &make_row(&[("id", Value::Int(4))])));
    }
}
