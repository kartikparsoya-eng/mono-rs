use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::filter::{Value, compare_values};

pub type Row = serde_json::Map<String, serde_json::Value>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub row: Row,
    pub relationships: HashMap<String, Vec<Node>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChangeType {
    Add,
    Remove,
    Child,
    Edit,
}

#[derive(Clone, Debug)]
pub struct ChildData {
    pub relationship_name: String,
    pub change: Box<Change>,
}

#[derive(Clone, Debug)]
pub enum Change {
    Add(Node),
    Remove(Node),
    Child { node: Node, child: ChildData },
    Edit { node: Node, old_node: Node },
}

impl Change {
    pub fn node(&self) -> &Node {
        match self {
            Change::Add(n) | Change::Remove(n) => n,
            Change::Child { node, .. } | Change::Edit { node, .. } => node,
        }
    }

    pub fn change_type(&self) -> ChangeType {
        match self {
            Change::Add(_) => ChangeType::Add,
            Change::Remove(_) => ChangeType::Remove,
            Change::Child { .. } => ChangeType::Child,
            Change::Edit { .. } => ChangeType::Edit,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Constraint {
    #[serde(flatten)]
    pub columns: HashMap<String, serde_json::Value>,
}

impl Constraint {
    /// Create a single-column constraint (most common case).
    pub fn single(key: String, value: serde_json::Value) -> Self {
        let mut columns = HashMap::new();
        columns.insert(key, value);
        Self { columns }
    }

    /// Create a multi-column constraint from key-value pairs.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, serde_json::Value)>) -> Self {
        Self { columns: pairs.into_iter().collect() }
    }

    /// Check if a row matches all columns in this constraint.
    pub fn matches_row(&self, row: &Row) -> bool {
        self.columns.iter().all(|(k, v)| row.get(k) == Some(v))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Start {
    pub row: Row,
    pub basis: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FetchRequest {
    pub constraint: Option<Constraint>,
    pub start: Option<Start>,
    #[serde(default)]
    pub reverse: bool,
}

#[derive(Clone, Debug)]
pub struct SortSpec {
    pub field: String,
    pub direction: SortDirection,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Convert a serde_json::Value to our filter::Value for comparison.
fn json_to_value(v: &serde_json::Value) -> Value {
    Value::from_json(v)
}

/// Compare two rows by the given sort specification.
pub fn compare_rows(a: &Row, b: &Row, sort: &[SortSpec]) -> std::cmp::Ordering {
    for spec in sort {
        let a_val = a.get(&spec.field).map(json_to_value).unwrap_or(Value::Null);
        let b_val = b.get(&spec.field).map(json_to_value).unwrap_or(Value::Null);
        let cmp = compare_values(&a_val, &b_val);
        if cmp != std::cmp::Ordering::Equal {
            return match spec.direction {
                SortDirection::Asc => cmp,
                SortDirection::Desc => cmp.reverse(),
            };
        }
    }
    std::cmp::Ordering::Equal
}

// Custom Serialize/Deserialize for Change to match TS tuple format:
// [type, node, extra]
// ADD: [0, node, null], REMOVE: [1, node, null],
// EDIT: [3, node, old_node], CHILD: [2, node, child_data]
impl Serialize for Change {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut tup = serializer.serialize_tuple(3)?;
        match self {
            Change::Add(node) => {
                tup.serialize_element(&0u8)?;
                tup.serialize_element(node)?;
                tup.serialize_element(&serde_json::Value::Null)?;
            }
            Change::Remove(node) => {
                tup.serialize_element(&1u8)?;
                tup.serialize_element(node)?;
                tup.serialize_element(&serde_json::Value::Null)?;
            }
            Change::Child { node, child } => {
                tup.serialize_element(&2u8)?;
                tup.serialize_element(node)?;
                #[derive(Serialize)]
                struct ChildSer<'a> {
                    relationship_name: &'a str,
                    change: &'a Change,
                }
                tup.serialize_element(&ChildSer {
                    relationship_name: &child.relationship_name,
                    change: &child.change,
                })?;
            }
            Change::Edit { node, old_node } => {
                tup.serialize_element(&3u8)?;
                tup.serialize_element(node)?;
                tup.serialize_element(old_node)?;
            }
        }
        tup.end()
    }
}

impl<'de> Deserialize<'de> for Change {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
        if v.len() != 3 {
            return Err(serde::de::Error::custom("Change tuple must have 3 elements"));
        }
        let change_type = v[0]
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("Change type must be a number"))?;
        match change_type {
            0 => {
                let node: Node = serde_json::from_value(v[1].clone())
                    .map_err(serde::de::Error::custom)?;
                Ok(Change::Add(node))
            }
            1 => {
                let node: Node = serde_json::from_value(v[1].clone())
                    .map_err(serde::de::Error::custom)?;
                Ok(Change::Remove(node))
            }
            2 => {
                let node: Node = serde_json::from_value(v[1].clone())
                    .map_err(serde::de::Error::custom)?;
                #[derive(Deserialize)]
                struct ChildDe {
                    relationship_name: String,
                    change: Change,
                }
                let child: ChildDe = serde_json::from_value(v[2].clone())
                    .map_err(serde::de::Error::custom)?;
                Ok(Change::Child {
                    node,
                    child: ChildData {
                        relationship_name: child.relationship_name,
                        change: Box::new(child.change),
                    },
                })
            }
            3 => {
                let node: Node = serde_json::from_value(v[1].clone())
                    .map_err(serde::de::Error::custom)?;
                let old_node: Node = serde_json::from_value(v[2].clone())
                    .map_err(serde::de::Error::custom)?;
                Ok(Change::Edit { node, old_node })
            }
            _ => Err(serde::de::Error::custom(format!(
                "Unknown change type: {}",
                change_type
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(pairs: &[(&str, serde_json::Value)]) -> Row {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn make_node(row: Row) -> Node {
        Node {
            row,
            relationships: HashMap::new(),
        }
    }

    #[test]
    fn test_compare_rows_asc() {
        let sort = vec![SortSpec {
            field: "age".to_string(),
            direction: SortDirection::Asc,
        }];
        let a = make_row(&[("age", serde_json::json!(20))]);
        let b = make_row(&[("age", serde_json::json!(30))]);
        assert_eq!(compare_rows(&a, &b, &sort), std::cmp::Ordering::Less);
        assert_eq!(compare_rows(&b, &a, &sort), std::cmp::Ordering::Greater);
        assert_eq!(compare_rows(&a, &a, &sort), std::cmp::Ordering::Equal);
    }

    #[test]
    fn test_compare_rows_desc() {
        let sort = vec![SortSpec {
            field: "age".to_string(),
            direction: SortDirection::Desc,
        }];
        let a = make_row(&[("age", serde_json::json!(20))]);
        let b = make_row(&[("age", serde_json::json!(30))]);
        assert_eq!(compare_rows(&a, &b, &sort), std::cmp::Ordering::Greater);
    }

    #[test]
    fn test_compare_rows_multi_field() {
        let sort = vec![
            SortSpec { field: "name".to_string(), direction: SortDirection::Asc },
            SortSpec { field: "age".to_string(), direction: SortDirection::Asc },
        ];
        let a = make_row(&[("name", serde_json::json!("Alice")), ("age", serde_json::json!(20))]);
        let b = make_row(&[("name", serde_json::json!("Alice")), ("age", serde_json::json!(30))]);
        assert_eq!(compare_rows(&a, &b, &sort), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_change_node_accessor() {
        let node = make_node(make_row(&[("id", serde_json::json!(1))]));
        let change = Change::Add(node.clone());
        assert_eq!(change.node().row, node.row);
        assert_eq!(change.change_type(), ChangeType::Add);
    }

    #[test]
    fn test_change_serde_roundtrip() {
        let node = make_node(make_row(&[("id", serde_json::json!(1))]));
        let change = Change::Add(node);
        let json = serde_json::to_string(&change).unwrap();
        let deserialized: Change = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.change_type(), ChangeType::Add);
        assert_eq!(deserialized.node().row.get("id").unwrap(), &serde_json::json!(1));
    }
}
