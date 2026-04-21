use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnType {
    Boolean,
    Number,
    String,
    Null,
    Json,
}

pub type Ordering = Vec<(std::string::String, std::string::String)>;

pub struct Start {
    pub row: serde_json::Map<std::string::String, Value>,
    pub basis: std::string::String,
}

pub type Constraint = serde_json::Map<std::string::String, Value>;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum Condition {
    #[serde(rename = "simple")]
    Simple {
        left: ValuePosition,
        op: std::string::String,
        right: ValuePosition,
    },
    #[serde(rename = "and")]
    And { conditions: Vec<Condition> },
    #[serde(rename = "or")]
    Or { conditions: Vec<Condition> },
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ValuePosition {
    #[serde(rename = "column")]
    Column { name: std::string::String },
    #[serde(rename = "literal")]
    Literal { value: Value },
}

pub fn to_sqlite_type(v: &Value, col_type: &ColumnType) -> Value {
    match col_type {
        ColumnType::Boolean => {
            if v.is_null() {
                Value::Null
            } else if v.as_bool().unwrap_or(false) {
                Value::Number(1.into())
            } else {
                Value::Number(0.into())
            }
        }
        ColumnType::Number | ColumnType::String | ColumnType::Null => v.clone(),
        ColumnType::Json => Value::String(serde_json::to_string(v).unwrap_or_default()),
    }
}

fn get_js_type(v: &Value) -> ColumnType {
    match v {
        Value::Null => ColumnType::Null,
        Value::Bool(_) => ColumnType::Boolean,
        Value::Number(_) => ColumnType::Number,
        Value::String(_) => ColumnType::String,
        _ => ColumnType::Json,
    }
}

fn ident(name: &str) -> std::string::String {
    format!("\"{}\"", name)
}

fn constraints_to_sql(
    constraint: Option<&Constraint>,
    column_types: &HashMap<std::string::String, ColumnType>,
    parts: &mut Vec<std::string::String>,
    params: &mut Vec<Value>,
) {
    let constraint = match constraint {
        Some(c) => c,
        None => return,
    };
    for (key, value) in constraint {
        let col_type = column_types.get(key).unwrap_or(&ColumnType::String);
        parts.push(format!("{} = ?", ident(key)));
        params.push(to_sqlite_type(value, col_type));
    }
}

fn order_by_to_sql(order: &Ordering, reverse: bool) -> std::string::String {
    let items: Vec<std::string::String> = order
        .iter()
        .map(|(field, dir)| {
            let actual_dir = if reverse {
                if dir == "asc" {
                    "desc"
                } else {
                    "asc"
                }
            } else {
                dir.as_str()
            };
            format!("{} {}", ident(field), actual_dir)
        })
        .collect();
    format!("ORDER BY {}", items.join(", "))
}

fn gather_start_constraints(
    start: &Start,
    reverse: bool,
    order: &Ordering,
    column_types: &HashMap<std::string::String, ColumnType>,
    params: &mut Vec<Value>,
) -> std::string::String {
    let mut or_groups: Vec<std::string::String> = Vec::new();

    for i in 0..order.len() {
        let mut group: Vec<std::string::String> = Vec::new();
        let (i_field, i_direction) = &order[i];

        for j in 0..=i {
            if j == i {
                let col_type = column_types.get(i_field).unwrap_or(&ColumnType::String);
                let cv = to_sqlite_type(
                    start.row.get(i_field).unwrap_or(&Value::Null),
                    col_type,
                );
                let forward_gt = (i_direction == "asc" && !reverse)
                    || (i_direction == "desc" && reverse);
                if forward_gt {
                    group.push(format!("(? IS NULL OR {} > ?)", ident(i_field)));
                    params.push(cv.clone());
                    params.push(cv);
                } else {
                    group.push(format!(
                        "({} IS NULL OR {} < ?)",
                        ident(i_field),
                        ident(i_field)
                    ));
                    params.push(cv);
                }
            } else {
                let (j_field, _) = &order[j];
                let col_type = column_types.get(j_field).unwrap_or(&ColumnType::String);
                let jv = to_sqlite_type(
                    start.row.get(j_field).unwrap_or(&Value::Null),
                    col_type,
                );
                group.push(format!("{} IS ?", ident(j_field)));
                params.push(jv);
            }
        }
        or_groups.push(format!("({})", group.join(" AND ")));
    }

    if start.basis == "at" {
        let eq_parts: Vec<std::string::String> = order
            .iter()
            .map(|(field, _)| {
                let col_type = column_types.get(field).unwrap_or(&ColumnType::String);
                let v = to_sqlite_type(
                    start.row.get(field).unwrap_or(&Value::Null),
                    col_type,
                );
                params.push(v);
                format!("{} IS ?", ident(field))
            })
            .collect();
        or_groups.push(format!("({})", eq_parts.join(" AND ")));
    }

    format!("({})", or_groups.join(" OR "))
}

fn value_position_to_sql(vp: &ValuePosition, params: &mut Vec<Value>) -> std::string::String {
    match vp {
        ValuePosition::Column { name } => ident(name),
        ValuePosition::Literal { value } => {
            let col_type = get_js_type(value);
            params.push(to_sqlite_type(value, &col_type));
            "?".to_string()
        }
    }
}

fn filters_to_sql(filters: &Condition, params: &mut Vec<Value>) -> std::string::String {
    match filters {
        Condition::Simple { left, op, right } => {
            let sql_op = match op.as_str() {
                "ILIKE" => "LIKE",
                "NOT ILIKE" => "NOT LIKE",
                other => other,
            };
            if sql_op == "IN" || sql_op == "NOT IN" {
                let left_sql = value_position_to_sql(left, params);
                if let ValuePosition::Literal { value } = right {
                    let json_str = serde_json::to_string(value).unwrap_or_default();
                    params.push(Value::String(json_str));
                    return format!(
                        "{} {} (SELECT value FROM json_each(?))",
                        left_sql, sql_op
                    );
                }
            }
            let left_sql = value_position_to_sql(left, params);
            let right_sql = value_position_to_sql(right, params);
            format!("{} {} {}", left_sql, sql_op, right_sql)
        }
        Condition::And { conditions } => {
            if conditions.is_empty() {
                "TRUE".to_string()
            } else {
                let parts: Vec<std::string::String> = conditions
                    .iter()
                    .map(|c| filters_to_sql(c, params))
                    .collect();
                format!("({})", parts.join(" AND "))
            }
        }
        Condition::Or { conditions } => {
            if conditions.is_empty() {
                "FALSE".to_string()
            } else {
                let parts: Vec<std::string::String> = conditions
                    .iter()
                    .map(|c| filters_to_sql(c, params))
                    .collect();
                format!("({})", parts.join(" OR "))
            }
        }
    }
}

pub fn build_select_query(
    table_name: &str,
    columns: &[String],
    column_types: &HashMap<String, ColumnType>,
    constraint: Option<&Constraint>,
    filters: Option<&Condition>,
    order: Option<&Ordering>,
    reverse: bool,
    start: Option<&Start>,
) -> (String, Vec<Value>) {
    let mut params: Vec<Value> = Vec::new();

    let cols_sql = columns
        .iter()
        .map(|c| ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("SELECT {} FROM {}", cols_sql, ident(table_name));

    let mut where_parts: Vec<String> = Vec::new();

    constraints_to_sql(constraint, column_types, &mut where_parts, &mut params);

    if let Some(s) = start {
        let ord = order.expect("start requires ordering");
        where_parts.push(gather_start_constraints(
            s,
            reverse,
            ord,
            column_types,
            &mut params,
        ));
    }

    if let Some(f) = filters {
        where_parts.push(filters_to_sql(f, &mut params));
    }

    if !where_parts.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_parts.join(" AND "));
    }

    if let Some(ord) = order {
        if !ord.is_empty() {
            sql.push(' ');
            sql.push_str(&order_by_to_sql(ord, reverse));
        }
    }

    (sql, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn col_types() -> HashMap<String, ColumnType> {
        let mut m = HashMap::new();
        m.insert("id".into(), ColumnType::String);
        m.insert("name".into(), ColumnType::String);
        m.insert("age".into(), ColumnType::Number);
        m.insert("active".into(), ColumnType::Boolean);
        m.insert("meta".into(), ColumnType::Json);
        m
    }

    #[test]
    fn test_basic_select() {
        let cols = vec!["id".into(), "name".into()];
        let (sql, params) =
            build_select_query("users", &cols, &col_types(), None, None, None, false, None);
        assert_eq!(sql, r#"SELECT "id", "name" FROM "users""#);
        assert!(params.is_empty());
    }

    #[test]
    fn test_with_constraint() {
        let cols = vec!["id".into(), "name".into()];
        let mut c = Constraint::new();
        c.insert("id".into(), json!("abc"));
        let (sql, params) = build_select_query(
            "users",
            &cols,
            &col_types(),
            Some(&c),
            None,
            None,
            false,
            None,
        );
        assert_eq!(sql, r#"SELECT "id", "name" FROM "users" WHERE "id" = ?"#);
        assert_eq!(params, vec![json!("abc")]);
    }

    #[test]
    fn test_with_ordering() {
        let cols = vec!["id".into()];
        let order = vec![("name".into(), "asc".into()), ("age".into(), "desc".into())];
        let (sql, _) = build_select_query(
            "users",
            &cols,
            &col_types(),
            None,
            None,
            Some(&order),
            false,
            None,
        );
        assert_eq!(
            sql,
            r#"SELECT "id" FROM "users" ORDER BY "name" asc, "age" desc"#
        );
    }

    #[test]
    fn test_with_reverse() {
        let cols = vec!["id".into()];
        let order = vec![("name".into(), "asc".into()), ("age".into(), "desc".into())];
        let (sql, _) = build_select_query(
            "users",
            &cols,
            &col_types(),
            None,
            None,
            Some(&order),
            true,
            None,
        );
        assert_eq!(
            sql,
            r#"SELECT "id" FROM "users" ORDER BY "name" desc, "age" asc"#
        );
    }

    #[test]
    fn test_with_start_simple() {
        let cols = vec!["id".into()];
        let order = vec![("id".into(), "asc".into())];
        let mut row = serde_json::Map::new();
        row.insert("id".into(), json!("x"));
        let start = Start {
            row,
            basis: "after".into(),
        };
        let (sql, params) = build_select_query(
            "users",
            &cols,
            &col_types(),
            None,
            None,
            Some(&order),
            false,
            Some(&start),
        );
        assert_eq!(
            sql,
            r#"SELECT "id" FROM "users" WHERE (((? IS NULL OR "id" > ?))) ORDER BY "id" asc"#
        );
        assert_eq!(params, vec![json!("x"), json!("x")]);
    }

    #[test]
    fn test_with_start_compound() {
        let cols = vec!["id".into()];
        let order = vec![
            ("name".into(), "asc".into()),
            ("age".into(), "desc".into()),
        ];
        let mut row = serde_json::Map::new();
        row.insert("name".into(), json!("a"));
        row.insert("age".into(), json!(10));
        let start = Start {
            row,
            basis: "at".into(),
        };
        let (sql, params) = build_select_query(
            "users",
            &cols,
            &col_types(),
            None,
            None,
            Some(&order),
            false,
            Some(&start),
        );
        assert_eq!(
            sql,
            concat!(
                r#"SELECT "id" FROM "users" WHERE "#,
                r#"(((? IS NULL OR "name" > ?)) OR "#,
                r#"("name" IS ? AND ("age" IS NULL OR "age" < ?)) OR "#,
                r#"("name" IS ? AND "age" IS ?)) "#,
                r#"ORDER BY "name" asc, "age" desc"#,
            )
        );
        assert_eq!(
            params,
            vec![json!("a"), json!("a"), json!("a"), json!(10), json!("a"), json!(10)]
        );
    }

    #[test]
    fn test_with_filters_simple() {
        let cols = vec!["id".into()];
        let filter = Condition::Simple {
            left: ValuePosition::Column {
                name: "age".into(),
            },
            op: ">".into(),
            right: ValuePosition::Literal { value: json!(18) },
        };
        let (sql, params) = build_select_query(
            "users",
            &cols,
            &col_types(),
            None,
            Some(&filter),
            None,
            false,
            None,
        );
        assert_eq!(sql, r#"SELECT "id" FROM "users" WHERE "age" > ?"#);
        assert_eq!(params, vec![json!(18)]);
    }

    #[test]
    fn test_with_filters_and_or() {
        let cols = vec!["id".into()];
        let filter = Condition::Or {
            conditions: vec![
                Condition::Simple {
                    left: ValuePosition::Column {
                        name: "name".into(),
                    },
                    op: "=".into(),
                    right: ValuePosition::Literal { value: json!("a") },
                },
                Condition::And {
                    conditions: vec![
                        Condition::Simple {
                            left: ValuePosition::Column {
                                name: "age".into(),
                            },
                            op: ">".into(),
                            right: ValuePosition::Literal { value: json!(5) },
                        },
                        Condition::Simple {
                            left: ValuePosition::Column {
                                name: "age".into(),
                            },
                            op: "<".into(),
                            right: ValuePosition::Literal { value: json!(10) },
                        },
                    ],
                },
            ],
        };
        let (sql, params) = build_select_query(
            "users",
            &cols,
            &col_types(),
            None,
            Some(&filter),
            None,
            false,
            None,
        );
        assert_eq!(
            sql,
            r#"SELECT "id" FROM "users" WHERE ("name" = ? OR ("age" > ? AND "age" < ?))"#
        );
        assert_eq!(params, vec![json!("a"), json!(5), json!(10)]);
    }

    #[test]
    fn test_to_sqlite_type() {
        assert_eq!(to_sqlite_type(&json!(true), &ColumnType::Boolean), json!(1));
        assert_eq!(to_sqlite_type(&json!(false), &ColumnType::Boolean), json!(0));
        assert_eq!(to_sqlite_type(&Value::Null, &ColumnType::Boolean), Value::Null);
        assert_eq!(
            to_sqlite_type(&json!({"a": 1}), &ColumnType::Json),
            json!(r#"{"a":1}"#)
        );
        assert_eq!(to_sqlite_type(&json!(42), &ColumnType::Number), json!(42));
        assert_eq!(to_sqlite_type(&json!("hi"), &ColumnType::String), json!("hi"));
    }
}
