use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub type Row = serde_json::Map<String, serde_json::Value>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub row: Row,
    pub relationships: HashMap<String, Vec<Node>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SourceChangeType {
    Add,
    Remove,
    Edit,
}

#[derive(Clone, Debug)]
pub enum SourceChange {
    Add(Row),
    Remove(Row),
    Edit { row: Row, old_row: Row },
}

impl SourceChange {
    pub fn change_type(&self) -> SourceChangeType {
        match self {
            SourceChange::Add(_) => SourceChangeType::Add,
            SourceChange::Remove(_) => SourceChangeType::Remove,
            SourceChange::Edit { .. } => SourceChangeType::Edit,
        }
    }

    pub fn row(&self) -> &Row {
        match self {
            SourceChange::Add(r) | SourceChange::Remove(r) => r,
            SourceChange::Edit { row, .. } => row,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Overlay {
    pub epoch: u64,
    pub change: SourceChange,
}

#[derive(Clone, Debug, Default)]
pub struct Overlays {
    pub add: Option<Row>,
    pub remove: Option<Row>,
}

#[derive(Clone, Debug)]
pub enum Change {
    Add(Node),
    Remove(Node),
    Child { node: Node, child: Box<ChildChange> },
    Edit { node: Node, old_node: Node },
}

#[derive(Clone, Debug)]
pub struct ChildChange {
    pub relationship_name: String,
    pub change: Change,
}

impl Change {
    pub fn node(&self) -> &Node {
        match self {
            Change::Add(n) | Change::Remove(n) => n,
            Change::Child { node, .. } | Change::Edit { node, .. } => node,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FetchRequest {
    pub constraint: Option<FetchConstraint>,
    pub start: Option<FetchStart>,
    #[serde(default)]
    pub reverse: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchConstraint {
    pub columns: std::collections::HashMap<String, serde_json::Value>,
}

impl FetchConstraint {
    pub fn single(key: String, value: serde_json::Value) -> Self {
        let mut columns = std::collections::HashMap::new();
        columns.insert(key, value);
        Self { columns }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchStart {
    pub row: Row,
    pub basis: String,
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

fn value_type_order(v: &serde_json::Value) -> u8 {
    match v {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(_) => 1,
        serde_json::Value::Number(_) => 2,
        serde_json::Value::String(_) => 3,
        _ => 4,
    }
}

fn compare_json_values(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ta = value_type_order(a);
    let tb = value_type_order(b);
    if ta != tb {
        return ta.cmp(&tb);
    }
    match (a, b) {
        (serde_json::Value::Null, serde_json::Value::Null) => Ordering::Equal,
        (serde_json::Value::Bool(a), serde_json::Value::Bool(b)) => a.cmp(b),
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
            let fa = a.as_f64().unwrap_or(0.0);
            let fb = b.as_f64().unwrap_or(0.0);
            fa.partial_cmp(&fb).unwrap_or(Ordering::Equal)
        }
        (serde_json::Value::String(a), serde_json::Value::String(b)) => a.cmp(b),
        _ => Ordering::Equal,
    }
}

pub fn compare_rows(a: &Row, b: &Row, sort: &[SortSpec]) -> std::cmp::Ordering {
    let null = serde_json::Value::Null;
    for spec in sort {
        let a_val = a.get(&spec.field).unwrap_or(&null);
        let b_val = b.get(&spec.field).unwrap_or(&null);
        let cmp = compare_json_values(a_val, b_val);
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
// ADD: [0, node, null], REMOVE: [1, node, null],
// CHILD: [2, node, child_data], EDIT: [3, node, old_node]
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
                    child: Box::new(ChildChange {
                        relationship_name: child.relationship_name,
                        change: child.change,
                    }),
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
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn make_node(row: Row) -> Node {
        Node {
            row,
            relationships: HashMap::new(),
        }
    }

    #[test]
    fn test_source_change_accessors() {
        let row = make_row(&[("id", serde_json::json!(1))]);

        let add = SourceChange::Add(row.clone());
        assert_eq!(add.change_type(), SourceChangeType::Add);
        assert_eq!(add.row().get("id").unwrap(), &serde_json::json!(1));

        let remove = SourceChange::Remove(row.clone());
        assert_eq!(remove.change_type(), SourceChangeType::Remove);
        assert_eq!(remove.row().get("id").unwrap(), &serde_json::json!(1));

        let old_row = make_row(&[("id", serde_json::json!(0))]);
        let edit = SourceChange::Edit {
            row: row.clone(),
            old_row,
        };
        assert_eq!(edit.change_type(), SourceChangeType::Edit);
        assert_eq!(edit.row().get("id").unwrap(), &serde_json::json!(1));
    }

    #[test]
    fn test_compare_rows() {
        let sort_asc = vec![SortSpec {
            field: "age".to_string(),
            direction: SortDirection::Asc,
        }];
        let a = make_row(&[("age", serde_json::json!(20))]);
        let b = make_row(&[("age", serde_json::json!(30))]);
        assert_eq!(compare_rows(&a, &b, &sort_asc), std::cmp::Ordering::Less);
        assert_eq!(
            compare_rows(&b, &a, &sort_asc),
            std::cmp::Ordering::Greater
        );
        assert_eq!(compare_rows(&a, &a, &sort_asc), std::cmp::Ordering::Equal);

        let sort_desc = vec![SortSpec {
            field: "age".to_string(),
            direction: SortDirection::Desc,
        }];
        assert_eq!(
            compare_rows(&a, &b, &sort_desc),
            std::cmp::Ordering::Greater
        );

        // Null < Number
        let c = make_row(&[]);
        assert_eq!(compare_rows(&c, &a, &sort_asc), std::cmp::Ordering::Less);

        // Multi-field
        let sort_multi = vec![
            SortSpec {
                field: "name".to_string(),
                direction: SortDirection::Asc,
            },
            SortSpec {
                field: "age".to_string(),
                direction: SortDirection::Asc,
            },
        ];
        let x = make_row(&[
            ("name", serde_json::json!("Alice")),
            ("age", serde_json::json!(20)),
        ]);
        let y = make_row(&[
            ("name", serde_json::json!("Alice")),
            ("age", serde_json::json!(30)),
        ]);
        assert_eq!(compare_rows(&x, &y, &sort_multi), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_change_serde_roundtrip() {
        let node = make_node(make_row(&[("id", serde_json::json!(1))]));

        // Add
        let change = Change::Add(node.clone());
        let json = serde_json::to_string(&change).unwrap();
        let de: Change = serde_json::from_str(&json).unwrap();
        assert_eq!(de.node().row.get("id").unwrap(), &serde_json::json!(1));

        // Remove
        let change = Change::Remove(node.clone());
        let json = serde_json::to_string(&change).unwrap();
        let de: Change = serde_json::from_str(&json).unwrap();
        assert_eq!(de.node().row.get("id").unwrap(), &serde_json::json!(1));

        // Edit
        let old = make_node(make_row(&[("id", serde_json::json!(0))]));
        let change = Change::Edit {
            node: node.clone(),
            old_node: old,
        };
        let json = serde_json::to_string(&change).unwrap();
        let de: Change = serde_json::from_str(&json).unwrap();
        assert_eq!(de.node().row.get("id").unwrap(), &serde_json::json!(1));

        // Child
        let inner = Change::Add(make_node(make_row(&[("cid", serde_json::json!(99))])));
        let change = Change::Child {
            node,
            child: Box::new(ChildChange {
                relationship_name: "items".to_string(),
                change: inner,
            }),
        };
        let json = serde_json::to_string(&change).unwrap();
        let de: Change = serde_json::from_str(&json).unwrap();
        assert_eq!(de.node().row.get("id").unwrap(), &serde_json::json!(1));
    }

    #[test]
    fn test_overlays_default() {
        let o = Overlays::default();
        assert!(o.add.is_none());
        assert!(o.remove.is_none());
    }
}
