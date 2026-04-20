use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

pub type Row = HashMap<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub table: String,
    pub prev_values: Vec<Row>,
    pub next_value: Option<Row>,
    pub row_key: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableSpec {
    pub name: String,
    pub columns: Vec<String>,
    pub primary_key: Vec<String>,
    pub unique_keys: Vec<Vec<String>>,
    pub min_row_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ZqlSpec {
    pub columns: HashMap<String, ColumnSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ColumnSpec {
    #[serde(rename = "type")]
    pub col_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableAndZqlSpec {
    pub table_spec: TableSpec,
    pub zql_spec: ZqlSpec,
}

#[derive(Debug)]
pub enum DiffError {
    Reset(String),
    Truncate(String),
    Unknown(String),
}

impl std::fmt::Display for DiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiffError::Reset(msg) => write!(f, "Reset: {}", msg),
            DiffError::Truncate(msg) => write!(f, "Truncate: {}", msg),
            DiffError::Unknown(msg) => write!(f, "Unknown: {}", msg),
        }
    }
}

impl std::error::Error for DiffError {}

/// Changelog entry: (stateVersion, table, rowKey, op)
type ChangelogEntry = (String, String, serde_json::Value, String);

const BATCH_SIZE: usize = 500;
const RESET_OP: &str = "r";
const TRUNCATE_OP: &str = "t";
const SET_OP: &str = "s";

/// Read changelog entries from `_zero.changeLog2` since `prev_version`.
pub fn read_changelog_entries(
    conn: &Connection,
    prev_version: &str,
) -> Result<Vec<ChangelogEntry>, DiffError> {
    let mut results = Vec::new();
    let mut offset: usize = 0;

    loop {
        let mut stmt = conn
            .prepare_cached(
                r#"SELECT stateVersion, "table", rowKey, op
                   FROM "_zero.changeLog2"
                   WHERE stateVersion > ?
                   ORDER BY stateVersion, pos
                   LIMIT ? OFFSET ?"#,
            )
            .map_err(|e| DiffError::Unknown(e.to_string()))?;

        let batch: Vec<ChangelogEntry> = stmt
            .query_map(
                rusqlite::params![prev_version, BATCH_SIZE as i64, offset as i64],
                |row| {
                    let state_version: String = row.get(0)?;
                    let table: String = row.get(1)?;
                    let row_key_str: String = row.get(2)?;
                    let op: String = row.get(3)?;
                    let row_key: serde_json::Value = serde_json::from_str(&row_key_str)
                        .unwrap_or(serde_json::Value::String(row_key_str));
                    Ok((state_version, table, row_key, op))
                },
            )
            .map_err(|e| DiffError::Unknown(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| DiffError::Unknown(e.to_string()))?;

        let batch_len = batch.len();
        results.extend(batch);

        if batch_len < BATCH_SIZE {
            break;
        }
        offset += BATCH_SIZE;
    }

    Ok(results)
}

/// Fetch a single row by primary key.
pub fn get_row(
    conn: &Connection,
    table_spec: &TableSpec,
    row_key: &serde_json::Value,
) -> Result<Option<Row>, DiffError> {
    let cols = table_spec.columns.join(", ");
    let where_clause: Vec<String> = table_spec
        .primary_key
        .iter()
        .map(|pk| format!("\"{}\" = ?", pk))
        .collect();
    let sql = format!(
        "SELECT {} FROM \"{}\" WHERE {}",
        cols,
        table_spec.name,
        where_clause.join(" AND ")
    );

    let key_values = extract_key_values(row_key, &table_spec.primary_key)?;

    let mut stmt = conn
        .prepare_cached(&sql)
        .map_err(|e| DiffError::Unknown(e.to_string()))?;

    let result = stmt
        .query_row(rusqlite::params_from_iter(key_values.iter()), |row| {
            let mut map = Row::new();
            for (i, col) in table_spec.columns.iter().enumerate() {
                let val = sqlite_value_to_json(row, i)?;
                map.insert(col.clone(), val);
            }
            Ok(map)
        })
        .optional()
        .map_err(|e| DiffError::Unknown(e.to_string()))?;

    Ok(result)
}

/// Fetch rows matching any unique key constraints.
/// Filters out keys where any column value is null.
pub fn get_rows(
    conn: &Connection,
    table_spec: &TableSpec,
    unique_keys: &[Vec<String>],
    row: &Row,
) -> Result<Vec<Row>, DiffError> {
    // Filter out unique keys where any column has a null value in the row
    let valid_keys: Vec<&Vec<String>> = unique_keys
        .iter()
        .filter(|uk| {
            uk.iter().all(|col| {
                row.get(col)
                    .map(|v| !v.is_null())
                    .unwrap_or(false)
            })
        })
        .collect();

    if valid_keys.is_empty() {
        return Ok(Vec::new());
    }

    let cols = table_spec.columns.join(", ");

    // Build OR query: (k1=? AND k2=?) OR (k3=?)
    let or_clauses: Vec<String> = valid_keys
        .iter()
        .map(|uk| {
            let and_parts: Vec<String> = uk
                .iter()
                .map(|col| format!("\"{}\" = ?", col))
                .collect();
            format!("({})", and_parts.join(" AND "))
        })
        .collect();

    let sql = format!(
        "SELECT {} FROM \"{}\" WHERE {}",
        cols,
        table_spec.name,
        or_clauses.join(" OR ")
    );

    // Collect param values
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    for uk in &valid_keys {
        for col in uk.iter() {
            let val = row.get(col).cloned().unwrap_or(serde_json::Value::Null);
            params.push(json_to_sqlite_value(&val));
        }
    }

    let mut stmt = conn
        .prepare_cached(&sql)
        .map_err(|e| DiffError::Unknown(e.to_string()))?;

    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            let mut map = Row::new();
            for (i, col) in table_spec.columns.iter().enumerate() {
                let val = sqlite_value_to_json(r, i)?;
                map.insert(col.clone(), val);
            }
            Ok(map)
        })
        .map_err(|e| DiffError::Unknown(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| DiffError::Unknown(e.to_string()))?;

    Ok(rows)
}

/// Convert SQLite types to ZQL types (boolean 0/1 -> bool, json string -> parsed).
pub fn from_sqlite_types(zql_spec: &ZqlSpec, row: &mut Row) {
    for (col, spec) in &zql_spec.columns {
        if let Some(val) = row.get_mut(col) {
            match spec.col_type.as_str() {
                "boolean" => {
                    *val = match val {
                        serde_json::Value::Number(n) => {
                            serde_json::Value::Bool(n.as_i64().unwrap_or(0) != 0)
                        }
                        _ => val.clone(),
                    };
                }
                "json" => {
                    if let serde_json::Value::String(s) = val {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                            *val = parsed;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Main entry point: read diff between prev and curr snapshots.
pub fn read_diff(
    prev_conn: &Connection,
    curr_conn: &Connection,
    prev_version: &str,
    syncable_tables: &HashMap<String, TableAndZqlSpec>,
    all_table_names: &HashSet<String>,
    permissions_table: &str,
) -> Result<Vec<Change>, DiffError> {
    let entries = read_changelog_entries(curr_conn, prev_version)?;
    let mut changes: Vec<Change> = Vec::new();

    for (_, table, row_key, op) in &entries {
        // RESET_OP
        if op == RESET_OP {
            return Err(DiffError::Reset(format!("schema-change on table {}", table)));
        }
        // TRUNCATE_OP
        if op == TRUNCATE_OP {
            return Err(DiffError::Truncate(format!("truncation on table {}", table)));
        }

        // Skip non-syncable tables that are known internal tables
        let table_and_spec = match syncable_tables.get(table.as_str()) {
            Some(spec) => spec,
            None => {
                if all_table_names.contains(table.as_str()) {
                    // Known table but not syncable — skip
                    continue;
                } else {
                    return Err(DiffError::Unknown(format!("unknown table: {}", table)));
                }
            }
        };

        let table_spec = &table_and_spec.table_spec;
        let zql_spec = &table_and_spec.zql_spec;

        if op == SET_OP {
            // Fetch next value from curr
            let mut next_value = get_row(curr_conn, table_spec, row_key)?;

            // Fetch prev values (unique key violations) from prev
            let mut prev_values = if let Some(ref nv) = next_value {
                get_rows(prev_conn, table_spec, &table_spec.unique_keys, nv)?
            } else {
                Vec::new()
            };

            // Filter no-ops
            if prev_values.is_empty() && next_value.is_none() {
                continue;
            }

            // Check permissions table
            if table == permissions_table && !prev_values.is_empty() {
                if let Some(ref nv) = next_value {
                    if let (Some(prev_perms), Some(next_perms)) = (
                        prev_values.first().and_then(|pv| pv.get("permissions")),
                        nv.get("permissions"),
                    ) {
                        if prev_perms != next_perms {
                            return Err(DiffError::Reset("permissions-change".to_string()));
                        }
                    }
                }
            }

            // Apply type conversions
            if let Some(ref mut nv) = next_value {
                from_sqlite_types(zql_spec, nv);
            }
            for pv in &mut prev_values {
                from_sqlite_types(zql_spec, pv);
            }

            changes.push(Change {
                table: table.clone(),
                prev_values,
                next_value,
                row_key: row_key.clone(),
            });
        } else {
            // Delete op: fetch prev value from prev_conn
            let mut prev_row = get_row(prev_conn, table_spec, row_key)?;

            // Filter no-ops (delete of non-existent row)
            if prev_row.is_none() {
                continue;
            }

            // Apply type conversions
            if let Some(ref mut pr) = prev_row {
                from_sqlite_types(zql_spec, pr);
            }

            let prev_values = prev_row.map_or_else(Vec::new, |r| vec![r]);

            changes.push(Change {
                table: table.clone(),
                prev_values,
                next_value: None,
                row_key: row_key.clone(),
            });
        }
    }

    Ok(changes)
}

// --- Helpers ---

fn extract_key_values(
    row_key: &serde_json::Value,
    primary_key: &[String],
) -> Result<Vec<rusqlite::types::Value>, DiffError> {
    let mut values = Vec::new();
    match row_key {
        serde_json::Value::Array(arr) => {
            for (i, pk) in primary_key.iter().enumerate() {
                let val = arr.get(i).cloned().unwrap_or(serde_json::Value::Null);
                let _ = pk; // pk used for ordering context
                values.push(json_to_sqlite_value(&val));
            }
        }
        serde_json::Value::Object(obj) => {
            for pk in primary_key {
                let val = obj.get(pk).cloned().unwrap_or(serde_json::Value::Null);
                values.push(json_to_sqlite_value(&val));
            }
        }
        // Single-column PK
        _ => {
            values.push(json_to_sqlite_value(row_key));
        }
    }
    Ok(values)
}

fn json_to_sqlite_value(val: &serde_json::Value) -> rusqlite::types::Value {
    match val {
        serde_json::Value::Null => rusqlite::types::Value::Null,
        serde_json::Value::Bool(b) => rusqlite::types::Value::Integer(if *b { 1 } else { 0 }),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                rusqlite::types::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                rusqlite::types::Value::Real(f)
            } else {
                rusqlite::types::Value::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => rusqlite::types::Value::Text(s.clone()),
        _ => rusqlite::types::Value::Text(val.to_string()),
    }
}

fn sqlite_value_to_json(
    row: &rusqlite::Row,
    idx: usize,
) -> Result<serde_json::Value, rusqlite::Error> {
    use rusqlite::types::ValueRef;
    let val_ref = row.get_ref(idx)?;
    let json_val = match val_ref {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => serde_json::Value::Number(serde_json::Number::from(i)),
        ValueRef::Real(f) => serde_json::json!(f),
        ValueRef::Text(t) => {
            let s = std::str::from_utf8(t).unwrap_or("");
            serde_json::Value::String(s.to_string())
        }
        ValueRef::Blob(b) => {
            serde_json::Value::String(base64_encode(b))
        }
    };
    Ok(json_val)
}

fn base64_encode(data: &[u8]) -> String {
    // Simple base64 — use a minimal implementation
    use std::fmt::Write;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        let _ = write!(result, "{}", CHARS[((triple >> 18) & 0x3F) as usize] as char);
        let _ = write!(result, "{}", CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            let _ = write!(result, "{}", CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            let _ = write!(result, "{}", CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE "_zero.changeLog2" (
                stateVersion TEXT,
                "table" TEXT,
                rowKey TEXT,
                op TEXT,
                pos INTEGER
            );
            CREATE TABLE users (
                id TEXT PRIMARY KEY,
                name TEXT,
                active INTEGER
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn users_table_spec() -> TableSpec {
        TableSpec {
            name: "users".to_string(),
            columns: vec!["id".to_string(), "name".to_string(), "active".to_string()],
            primary_key: vec!["id".to_string()],
            unique_keys: vec![vec!["id".to_string()]],
            min_row_version: None,
        }
    }

    fn users_zql_spec() -> ZqlSpec {
        let mut columns = HashMap::new();
        columns.insert(
            "id".to_string(),
            ColumnSpec { col_type: "string".to_string() },
        );
        columns.insert(
            "name".to_string(),
            ColumnSpec { col_type: "string".to_string() },
        );
        columns.insert(
            "active".to_string(),
            ColumnSpec { col_type: "boolean".to_string() },
        );
        ZqlSpec { columns }
    }

    fn syncable_tables() -> HashMap<String, TableAndZqlSpec> {
        let mut m = HashMap::new();
        m.insert(
            "users".to_string(),
            TableAndZqlSpec {
                table_spec: users_table_spec(),
                zql_spec: users_zql_spec(),
            },
        );
        m
    }

    fn all_table_names() -> HashSet<String> {
        let mut s = HashSet::new();
        s.insert("users".to_string());
        s
    }

    #[test]
    fn test_read_changelog_entries() {
        let conn = create_test_db();
        conn.execute_batch(
            r#"
            INSERT INTO "_zero.changeLog2" VALUES ('02', 'users', '"u1"', 's', 0);
            INSERT INTO "_zero.changeLog2" VALUES ('02', 'users', '"u2"', 's', 1);
            INSERT INTO "_zero.changeLog2" VALUES ('03', 'users', '"u3"', 'd', 0);
            "#,
        )
        .unwrap();

        let entries = read_changelog_entries(&conn, "01").unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].1, "users");
        assert_eq!(entries[2].3, "d");
    }

    #[test]
    fn test_get_row() {
        let conn = create_test_db();
        conn.execute("INSERT INTO users VALUES ('u1', 'Alice', 1)", [])
            .unwrap();

        let spec = users_table_spec();
        let row_key = serde_json::json!("u1");
        let row = get_row(&conn, &spec, &row_key).unwrap();
        assert!(row.is_some());
        let row = row.unwrap();
        assert_eq!(row.get("name").unwrap(), &serde_json::json!("Alice"));
    }

    #[test]
    fn test_get_row_missing() {
        let conn = create_test_db();
        let spec = users_table_spec();
        let row_key = serde_json::json!("nonexistent");
        let row = get_row(&conn, &spec, &row_key).unwrap();
        assert!(row.is_none());
    }

    #[test]
    fn test_get_rows_null_filter() {
        let conn = create_test_db();
        conn.execute("INSERT INTO users VALUES ('u1', 'Alice', 1)", [])
            .unwrap();

        let spec = users_table_spec();
        // unique key with a null column value should be filtered out
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::Value::Null);
        row.insert("name".to_string(), serde_json::json!("Alice"));

        let unique_keys = vec![vec!["id".to_string()]];
        let results = get_rows(&conn, &spec, &unique_keys, &row).unwrap();
        assert_eq!(results.len(), 0); // filtered because id is null
    }

    #[test]
    fn test_read_diff_set_op() {
        let prev_conn = create_test_db();
        let curr_conn = create_test_db();

        // prev has old row
        prev_conn
            .execute("INSERT INTO users VALUES ('u1', 'Alice', 1)", [])
            .unwrap();
        // curr has updated row + changelog
        curr_conn
            .execute("INSERT INTO users VALUES ('u1', 'Bob', 0)", [])
            .unwrap();
        curr_conn
            .execute_batch(
                r#"INSERT INTO "_zero.changeLog2" VALUES ('02', 'users', '"u1"', 's', 0);"#,
            )
            .unwrap();

        let changes = read_diff(
            &prev_conn,
            &curr_conn,
            "01",
            &syncable_tables(),
            &all_table_names(),
            "_zero.permissions",
        )
        .unwrap();

        assert_eq!(changes.len(), 1);
        let c = &changes[0];
        assert_eq!(c.table, "users");
        assert!(c.next_value.is_some());
        let nv = c.next_value.as_ref().unwrap();
        assert_eq!(nv.get("name").unwrap(), &serde_json::json!("Bob"));
        // active should be converted to boolean false
        assert_eq!(nv.get("active").unwrap(), &serde_json::json!(false));
        // prev_values should have Alice
        assert_eq!(c.prev_values.len(), 1);
        assert_eq!(
            c.prev_values[0].get("active").unwrap(),
            &serde_json::json!(true)
        );
    }

    #[test]
    fn test_read_diff_delete_op() {
        let prev_conn = create_test_db();
        let curr_conn = create_test_db();

        prev_conn
            .execute("INSERT INTO users VALUES ('u1', 'Alice', 1)", [])
            .unwrap();
        curr_conn
            .execute_batch(
                r#"INSERT INTO "_zero.changeLog2" VALUES ('02', 'users', '"u1"', 'd', 0);"#,
            )
            .unwrap();

        let changes = read_diff(
            &prev_conn,
            &curr_conn,
            "01",
            &syncable_tables(),
            &all_table_names(),
            "_zero.permissions",
        )
        .unwrap();

        assert_eq!(changes.len(), 1);
        assert!(changes[0].next_value.is_none());
        assert_eq!(changes[0].prev_values.len(), 1);
    }

    #[test]
    fn test_read_diff_reset_op() {
        let prev_conn = create_test_db();
        let curr_conn = create_test_db();

        curr_conn
            .execute_batch(
                r#"INSERT INTO "_zero.changeLog2" VALUES ('02', 'users', '"u1"', 'r', 0);"#,
            )
            .unwrap();

        let result = read_diff(
            &prev_conn,
            &curr_conn,
            "01",
            &syncable_tables(),
            &all_table_names(),
            "_zero.permissions",
        );

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DiffError::Reset(_)));
    }

    #[test]
    fn test_read_diff_truncate_op() {
        let prev_conn = create_test_db();
        let curr_conn = create_test_db();

        curr_conn
            .execute_batch(
                r#"INSERT INTO "_zero.changeLog2" VALUES ('02', 'users', '"u1"', 't', 0);"#,
            )
            .unwrap();

        let result = read_diff(
            &prev_conn,
            &curr_conn,
            "01",
            &syncable_tables(),
            &all_table_names(),
            "_zero.permissions",
        );

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DiffError::Truncate(_)));
    }

    #[test]
    fn test_read_diff_noop_filtered() {
        let prev_conn = create_test_db();
        let curr_conn = create_test_db();

        // Delete op but row doesn't exist in prev either
        curr_conn
            .execute_batch(
                r#"INSERT INTO "_zero.changeLog2" VALUES ('02', 'users', '"u1"', 'd', 0);"#,
            )
            .unwrap();

        let changes = read_diff(
            &prev_conn,
            &curr_conn,
            "01",
            &syncable_tables(),
            &all_table_names(),
            "_zero.permissions",
        )
        .unwrap();

        assert_eq!(changes.len(), 0);
    }

    #[test]
    fn test_from_sqlite_types_boolean() {
        let zql_spec = users_zql_spec();
        let mut row = Row::new();
        row.insert("id".to_string(), serde_json::json!("u1"));
        row.insert("name".to_string(), serde_json::json!("Alice"));
        row.insert("active".to_string(), serde_json::json!(1));

        from_sqlite_types(&zql_spec, &mut row);

        assert_eq!(row.get("active").unwrap(), &serde_json::json!(true));

        // Test false
        row.insert("active".to_string(), serde_json::json!(0));
        from_sqlite_types(&zql_spec, &mut row);
        assert_eq!(row.get("active").unwrap(), &serde_json::json!(false));
    }

    #[test]
    fn test_from_sqlite_types_json() {
        let mut columns = HashMap::new();
        columns.insert(
            "data".to_string(),
            ColumnSpec { col_type: "json".to_string() },
        );
        let zql_spec = ZqlSpec { columns };

        let mut row = Row::new();
        row.insert("data".to_string(), serde_json::json!(r#"{"key":"value"}"#));

        from_sqlite_types(&zql_spec, &mut row);

        assert_eq!(
            row.get("data").unwrap(),
            &serde_json::json!({"key": "value"})
        );
    }
}
