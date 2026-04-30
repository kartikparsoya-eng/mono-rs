use std::collections::HashMap;

use napi_derive::napi;

use crate::filter::{compare_values, Value};

// ─── Types ──────────────────────────────────────────────────────────────────

type Row = HashMap<String, Value>;

// ─── Internal helpers ───────────────────────────────────────────────────────

/// Checks if a parent row matches a child row on compound keys.
/// Returns false if any key field is Value::Null on either side.
fn is_join_match(
    parent: &Row,
    parent_key: &[String],
    child: &Row,
    child_key: &[String],
) -> bool {
    if parent_key.len() != child_key.len() {
        return false;
    }
    for (pk, ck) in parent_key.iter().zip(child_key.iter()) {
        let pv = parent.get(pk).unwrap_or(&Value::Null);
        let cv = child.get(ck).unwrap_or(&Value::Null);
        if matches!(pv, Value::Null) || matches!(cv, Value::Null) {
            return false;
        }
        if compare_values(pv, cv) != std::cmp::Ordering::Equal {
            return false;
        }
    }
    true
}

/// Builds a join constraint from a source row.
/// Returns None if any source key field is null.
fn build_join_constraint(
    source_row: &Row,
    source_key: &[String],
    target_key: &[String],
) -> Option<Row> {
    if source_key.len() != target_key.len() {
        return None;
    }
    let mut constraint = HashMap::new();
    for (sk, tk) in source_key.iter().zip(target_key.iter()) {
        let val = source_row.get(sk).unwrap_or(&Value::Null);
        if matches!(val, Value::Null) {
            return None;
        }
        constraint.insert(tk.clone(), val.clone());
    }
    Some(constraint)
}

/// Checks if two rows are equal on a compound key.
fn row_equals_for_compound_key(a: &Row, b: &Row, key: &[String]) -> bool {
    for k in key {
        let av = a.get(k).unwrap_or(&Value::Null);
        let bv = b.get(k).unwrap_or(&Value::Null);
        if compare_values(av, bv) != std::cmp::Ordering::Equal {
            return false;
        }
    }
    true
}

// ─── JSON helpers ───────────────────────────────────────────────────────────

fn parse_row(json: &serde_json::Value) -> Row {
    let mut row = HashMap::new();
    if let Some(obj) = json.as_object() {
        for (k, v) in obj {
            row.insert(k.clone(), Value::from_json(v));
        }
    }
    row
}

fn row_to_json(row: &Row) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in row {
        map.insert(
            k.clone(),
            match v {
                Value::Null => serde_json::Value::Null,
                Value::Bool(b) => serde_json::Value::Bool(*b),
                Value::Int(i) => serde_json::json!(*i),
                Value::Number(n) => serde_json::json!(*n),
                Value::String(s) => serde_json::Value::String(s.clone()),
            },
        );
    }
    serde_json::Value::Object(map)
}

// ─── napi-exposed functions ─────────────────────────────────────────────────

#[napi]
pub fn rust_build_join_constraint(
    source_row_json: String,
    source_key: Vec<String>,
    target_key: Vec<String>,
) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(&source_row_json).ok()?;
    let row = parse_row(&json);
    let constraint = build_join_constraint(&row, &source_key, &target_key)?;
    Some(serde_json::to_string(&row_to_json(&constraint)).unwrap())
}

#[napi]
pub fn rust_is_join_match(
    parent_row_json: String,
    parent_key: Vec<String>,
    child_row_json: String,
    child_key: Vec<String>,
) -> bool {
    let pj: serde_json::Value = match serde_json::from_str(&parent_row_json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let cj: serde_json::Value = match serde_json::from_str(&child_row_json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    is_join_match(&parse_row(&pj), &parent_key, &parse_row(&cj), &child_key)
}

#[napi]
pub fn rust_row_equals_for_compound_key(
    a_json: String,
    b_json: String,
    key: Vec<String>,
) -> bool {
    let aj: serde_json::Value = match serde_json::from_str(&a_json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let bj: serde_json::Value = match serde_json::from_str(&b_json) {
        Ok(v) => v,
        Err(_) => return false,
    };
    row_equals_for_compound_key(&parse_row(&aj), &parse_row(&bj), &key)
}

/// Batch function for the pushChildChange hot path.
/// Combines constraint building + batch matching in a single napi call.
#[napi]
pub fn rust_join_push_child_batch(
    child_row_json: String,
    child_key: Vec<String>,
    parent_key: Vec<String>,
    parent_rows_json: Vec<String>,
) -> String {
    let child_json: serde_json::Value = match serde_json::from_str(&child_row_json) {
        Ok(v) => v,
        Err(_) => return r#"{"constraint":null,"matchResults":[]}"#.to_string(),
    };
    let child_row = parse_row(&child_json);

    let constraint = build_join_constraint(&child_row, &child_key, &parent_key);
    let constraint_json = match &constraint {
        Some(c) => row_to_json(c),
        None => serde_json::Value::Null,
    };

    let match_results: Vec<bool> = parent_rows_json
        .iter()
        .map(|pj| match serde_json::from_str::<serde_json::Value>(pj) {
            Ok(v) => is_join_match(&parse_row(&v), &parent_key, &child_row, &child_key),
            Err(_) => false,
        })
        .collect();

    let result = serde_json::json!({
        "constraint": constraint_json,
        "matchResults": match_results,
    });
    result.to_string()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Value;

    fn make_row(pairs: &[(&str, Value)]) -> Row {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn test_is_join_match_single_key() {
        let parent = make_row(&[
            ("id", Value::Number(1.0)),
            ("name", Value::String("Alice".into())),
        ]);
        let child = make_row(&[
            ("parentId", Value::Number(1.0)),
            ("val", Value::String("x".into())),
        ]);
        assert!(is_join_match(
            &parent,
            &["id".into()],
            &child,
            &["parentId".into()]
        ));
    }

    #[test]
    fn test_is_join_match_compound_key() {
        let parent = make_row(&[
            ("a", Value::Number(1.0)),
            ("b", Value::String("x".into())),
        ]);
        let child = make_row(&[
            ("ca", Value::Number(1.0)),
            ("cb", Value::String("x".into())),
        ]);
        assert!(is_join_match(
            &parent,
            &["a".into(), "b".into()],
            &child,
            &["ca".into(), "cb".into()]
        ));
    }

    #[test]
    fn test_is_join_match_null_returns_false() {
        let parent = make_row(&[("id", Value::Null)]);
        let child = make_row(&[("parentId", Value::Null)]);
        assert!(!is_join_match(
            &parent,
            &["id".into()],
            &child,
            &["parentId".into()]
        ));
    }

    #[test]
    fn test_is_join_match_mismatch() {
        let parent = make_row(&[("id", Value::Number(1.0))]);
        let child = make_row(&[("parentId", Value::Number(2.0))]);
        assert!(!is_join_match(
            &parent,
            &["id".into()],
            &child,
            &["parentId".into()]
        ));
    }

    #[test]
    fn test_build_join_constraint_basic() {
        let row = make_row(&[
            ("issueId", Value::Number(42.0)),
            ("other", Value::String("x".into())),
        ]);
        let result = build_join_constraint(&row, &["issueId".into()], &["id".into()]);
        assert!(result.is_some());
        let c = result.unwrap();
        assert_eq!(c.get("id"), Some(&Value::Number(42.0)));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn test_build_join_constraint_null_fk_returns_none() {
        let row = make_row(&[("issueId", Value::Null)]);
        let result = build_join_constraint(&row, &["issueId".into()], &["id".into()]);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_join_constraint_missing_field_returns_none() {
        let row = make_row(&[("other", Value::Number(1.0))]);
        let result = build_join_constraint(&row, &["issueId".into()], &["id".into()]);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_join_constraint_compound() {
        let row = make_row(&[
            ("a", Value::Number(1.0)),
            ("b", Value::String("x".into())),
        ]);
        let result = build_join_constraint(
            &row,
            &["a".into(), "b".into()],
            &["ta".into(), "tb".into()],
        );
        let c = result.unwrap();
        assert_eq!(c.get("ta"), Some(&Value::Number(1.0)));
        assert_eq!(c.get("tb"), Some(&Value::String("x".into())));
    }

    #[test]
    fn test_row_equals_for_compound_key_equal() {
        let a = make_row(&[
            ("id", Value::Number(1.0)),
            ("name", Value::String("A".into())),
        ]);
        let b = make_row(&[
            ("id", Value::Number(1.0)),
            ("name", Value::String("A".into())),
            ("extra", Value::Bool(true)),
        ]);
        assert!(row_equals_for_compound_key(
            &a,
            &b,
            &["id".into(), "name".into()]
        ));
    }

    #[test]
    fn test_row_equals_for_compound_key_not_equal() {
        let a = make_row(&[("id", Value::Number(1.0))]);
        let b = make_row(&[("id", Value::Number(2.0))]);
        assert!(!row_equals_for_compound_key(&a, &b, &["id".into()]));
    }

    #[test]
    fn test_row_equals_for_compound_key_null_equals_null() {
        let a = make_row(&[("id", Value::Null)]);
        let b = make_row(&[("id", Value::Null)]);
        assert!(row_equals_for_compound_key(&a, &b, &["id".into()]));
    }

    #[test]
    fn test_parse_row_roundtrip() {
        let json_str = r#"{"id": 1, "name": "Alice", "active": true, "deleted": null}"#;
        let v: serde_json::Value = serde_json::from_str(json_str).unwrap();
        let row = parse_row(&v);
        // JSON integers now dispatch to Value::Int (not Number) to preserve
        // i64 precision for snowflake-style PKs > 2^53.
        assert_eq!(row.get("id"), Some(&Value::Int(1)));
        assert_eq!(row.get("name"), Some(&Value::String("Alice".into())));
        assert_eq!(row.get("active"), Some(&Value::Bool(true)));
        assert_eq!(row.get("deleted"), Some(&Value::Null));
    }

    #[test]
    fn test_napi_build_join_constraint() {
        let result = rust_build_join_constraint(
            r#"{"issueId": 42}"#.into(),
            vec!["issueId".into()],
            vec!["id".into()],
        );
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["id"], 42.0);
    }

    #[test]
    fn test_napi_build_join_constraint_null_returns_none() {
        let result = rust_build_join_constraint(
            r#"{"issueId": null}"#.into(),
            vec!["issueId".into()],
            vec!["id".into()],
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_napi_is_join_match() {
        assert!(rust_is_join_match(
            r#"{"id": 1}"#.into(),
            vec!["id".into()],
            r#"{"parentId": 1}"#.into(),
            vec!["parentId".into()],
        ));
        assert!(!rust_is_join_match(
            r#"{"id": 1}"#.into(),
            vec!["id".into()],
            r#"{"parentId": 2}"#.into(),
            vec!["parentId".into()],
        ));
    }

    #[test]
    fn test_napi_row_equals() {
        assert!(rust_row_equals_for_compound_key(
            r#"{"id": 1, "name": "A"}"#.into(),
            r#"{"id": 1, "name": "A", "extra": true}"#.into(),
            vec!["id".into(), "name".into()],
        ));
        assert!(!rust_row_equals_for_compound_key(
            r#"{"id": 1}"#.into(),
            r#"{"id": 2}"#.into(),
            vec!["id".into()],
        ));
    }

    #[test]
    fn test_napi_push_child_batch() {
        let result = rust_join_push_child_batch(
            r#"{"parentId": 1, "val": "x"}"#.into(),
            vec!["parentId".into()],
            vec!["id".into()],
            vec![
                r#"{"id": 1, "name": "Alice"}"#.into(),
                r#"{"id": 2, "name": "Bob"}"#.into(),
                r#"{"id": 1, "name": "Alice2"}"#.into(),
            ],
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(!parsed["constraint"].is_null());
        assert_eq!(parsed["constraint"]["id"], 1.0);
        let matches = parsed["matchResults"].as_array().unwrap();
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0], true);
        assert_eq!(matches[1], false);
        assert_eq!(matches[2], true);
    }

    #[test]
    fn test_napi_push_child_batch_null_fk() {
        let result = rust_join_push_child_batch(
            r#"{"parentId": null}"#.into(),
            vec!["parentId".into()],
            vec!["id".into()],
            vec![r#"{"id": 1}"#.into()],
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed["constraint"].is_null());
    }
}
