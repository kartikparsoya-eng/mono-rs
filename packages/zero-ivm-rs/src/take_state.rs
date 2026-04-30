use std::collections::HashMap;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use serde::{Deserialize, Serialize};

// ─── Types ──────────────────────────────────────────────────────────────────

/// Matches TS TakeState = { size: number, bound: Row | undefined }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TakeState {
    pub size: u32,
    pub bound: Option<HashMap<String, serde_json::Value>>,
}

/// Sort direction for row comparison
#[derive(Clone, Debug, PartialEq)]
pub struct SortSpec {
    pub field: String,
    pub direction: SortDirection,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SortDirection {
    Asc,
    Desc,
}

// ─── napi class ─────────────────────────────────────────────────────────────

#[napi]
pub struct RustTakeState {
    states: HashMap<String, TakeState>,
    max_bound: Option<HashMap<String, serde_json::Value>>,
    sort_spec: Vec<SortSpec>,
}

#[napi]
impl RustTakeState {
    #[napi(constructor)]
    pub fn new(sort_json: String) -> Result<Self> {
        // sort_json: JSON array of [field, "asc"|"desc"] pairs (Ordering type from TS)
        let ordering: Vec<(String, String)> = serde_json::from_str(&sort_json)
            .map_err(|e| Error::from_reason(format!("Invalid sort spec: {}", e)))?;
        let sort_spec = ordering
            .into_iter()
            .map(|(field, dir)| SortSpec {
                field,
                direction: if dir == "desc" {
                    SortDirection::Desc
                } else {
                    SortDirection::Asc
                },
            })
            .collect();
        Ok(Self {
            states: HashMap::new(),
            max_bound: None,
            sort_spec,
        })
    }

    /// Returns JSON string of TakeState or null
    #[napi]
    pub fn get_state(&self, key: String) -> Option<String> {
        self.states
            .get(&key)
            .map(|s| serde_json::to_string(s).unwrap())
    }

    #[napi]
    pub fn set_state(&mut self, key: String, state_json: String) -> Result<()> {
        let state: TakeState = serde_json::from_str(&state_json)
            .map_err(|e| Error::from_reason(format!("Invalid state: {}", e)))?;
        self.states.insert(key, state);
        Ok(())
    }

    #[napi]
    pub fn del_state(&mut self, key: String) {
        self.states.remove(&key);
    }

    #[napi]
    pub fn get_max_bound(&self) -> Option<String> {
        self.max_bound
            .as_ref()
            .map(|b| serde_json::to_string(b).unwrap())
    }

    #[napi]
    pub fn set_max_bound(&mut self, bound_json: String) -> Result<()> {
        let bound: HashMap<String, serde_json::Value> = serde_json::from_str(&bound_json)
            .map_err(|e| Error::from_reason(format!("Invalid bound: {}", e)))?;
        self.max_bound = Some(bound);
        Ok(())
    }

    #[napi]
    pub fn clear_max_bound(&mut self) {
        self.max_bound = None;
    }

    /// Compare two rows using the sort spec. Returns -1, 0, or 1.
    #[napi]
    pub fn compare_rows(&self, a_json: String, b_json: String) -> Result<i32> {
        let a: HashMap<String, serde_json::Value> = serde_json::from_str(&a_json)
            .map_err(|e| Error::from_reason(format!("Invalid row a: {}", e)))?;
        let b: HashMap<String, serde_json::Value> = serde_json::from_str(&b_json)
            .map_err(|e| Error::from_reason(format!("Invalid row b: {}", e)))?;
        Ok(self.compare_rows_internal(&a, &b))
    }

    /// Batch operation: set state + conditionally update max bound in one call
    #[napi]
    pub fn set_state_and_maybe_update_max(
        &mut self,
        key: String,
        state_json: String,
    ) -> Result<()> {
        let state: TakeState = serde_json::from_str(&state_json)
            .map_err(|e| Error::from_reason(format!("Invalid state: {}", e)))?;
        if let Some(ref bound) = state.bound {
            let should_update = match &self.max_bound {
                None => true,
                Some(max) => self.compare_rows_internal(bound, max) > 0,
            };
            if should_update {
                self.max_bound = Some(bound.clone());
            }
        }
        self.states.insert(key, state);
        Ok(())
    }

    /// Clear all state (for destroy/reset)
    #[napi]
    pub fn clear(&mut self) {
        self.states.clear();
        self.max_bound = None;
    }

    /// Get the number of states stored
    #[napi]
    pub fn size(&self) -> u32 {
        self.states.len() as u32
    }
}

impl RustTakeState {
    fn compare_rows_internal(
        &self,
        a: &HashMap<String, serde_json::Value>,
        b: &HashMap<String, serde_json::Value>,
    ) -> i32 {
        for spec in &self.sort_spec {
            let av = a
                .get(&spec.field)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let bv = b
                .get(&spec.field)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let cmp = compare_json_values(&av, &bv);
            if cmp != 0 {
                return if spec.direction == SortDirection::Desc {
                    -cmp
                } else {
                    cmp
                };
            }
        }
        0
    }
}

// ─── Value comparison matching TS compareValues ─────────────────────────────

/// Compare JSON values matching TS compareValues semantics:
/// - null/undefined treated as null
/// - null === null → 0
/// - null < everything else
/// - strings: UTF-8 byte comparison
/// - numbers: a - b
/// - booleans: false < true (false → -1, true → 1)
fn compare_json_values(a: &serde_json::Value, b: &serde_json::Value) -> i32 {
    use serde_json::Value::*;

    // Both equal by identity
    if a == b {
        return 0;
    }

    // String comparison (UTF-8 byte order)
    if let (String(sa), String(sb)) = (a, b) {
        return match sa.as_bytes().cmp(sb.as_bytes()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        };
    }

    // Number comparison
    if let (Number(na), Number(nb)) = (a, b) {
        // Preserve i64 precision for SQLite-sourced PKs > 2^53. Routing through
        // f64 would collapse distinct snowflake IDs that differ only below bit 53.
        if let (Some(ia), Some(ib)) = (na.as_i64(), nb.as_i64()) {
            return match ia.cmp(&ib) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
        }
        let fa = na.as_f64().unwrap_or(0.0);
        let fb = nb.as_f64().unwrap_or(0.0);
        let diff = fa - fb;
        if diff < 0.0 {
            return -1;
        }
        if diff > 0.0 {
            return 1;
        }
        return 0;
    }

    // Boolean comparison: false < true
    if let (Bool(ba), Bool(bb)) = (a, b) {
        return match (ba, bb) {
            (false, true) => -1,
            (true, false) => 1,
            _ => 0,
        };
    }

    // Null handling (after same-type checks, matching TS order)
    if a.is_null() {
        return -1;
    }
    if b.is_null() {
        return 1;
    }

    // Different types that aren't null — shouldn't happen in practice
    0
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_comparison() {
        let null = serde_json::Value::Null;
        let num = serde_json::json!(42);
        let str_val = serde_json::json!("hello");

        assert_eq!(compare_json_values(&null, &null), 0);
        assert_eq!(compare_json_values(&null, &num), -1);
        assert_eq!(compare_json_values(&num, &null), 1);
        assert_eq!(compare_json_values(&null, &str_val), -1);
    }

    #[test]
    fn test_string_utf8_comparison() {
        let a = serde_json::json!("apple");
        let b = serde_json::json!("banana");
        let c = serde_json::json!("apple");

        assert_eq!(compare_json_values(&a, &b), -1);
        assert_eq!(compare_json_values(&b, &a), 1);
        assert_eq!(compare_json_values(&a, &c), 0);
    }

    #[test]
    fn test_number_comparison() {
        let a = serde_json::json!(10);
        let b = serde_json::json!(20);
        let c = serde_json::json!(10);

        assert_eq!(compare_json_values(&a, &b), -1);
        assert_eq!(compare_json_values(&b, &a), 1);
        assert_eq!(compare_json_values(&a, &c), 0);
    }

    #[test]
    fn test_boolean_comparison() {
        let t = serde_json::json!(true);
        let f = serde_json::json!(false);

        assert_eq!(compare_json_values(&f, &t), -1);
        assert_eq!(compare_json_values(&t, &f), 1);
        assert_eq!(compare_json_values(&t, &t), 0);
        assert_eq!(compare_json_values(&f, &f), 0);
    }

    #[test]
    fn test_sort_spec_asc_desc() {
        let state = RustTakeState {
            states: HashMap::new(),
            max_bound: None,
            sort_spec: vec![
                SortSpec {
                    field: "name".into(),
                    direction: SortDirection::Asc,
                },
                SortSpec {
                    field: "age".into(),
                    direction: SortDirection::Desc,
                },
            ],
        };

        let mut a = HashMap::new();
        a.insert("name".to_string(), serde_json::json!("Alice"));
        a.insert("age".to_string(), serde_json::json!(30));

        let mut b = HashMap::new();
        b.insert("name".to_string(), serde_json::json!("Alice"));
        b.insert("age".to_string(), serde_json::json!(25));

        // Same name, age 30 vs 25 with desc → 30 should come first (compare returns -1 for desc when a > b)
        assert!(state.compare_rows_internal(&a, &b) < 0); // 30 > 25, desc → -1

        let mut c = HashMap::new();
        c.insert("name".to_string(), serde_json::json!("Bob"));
        c.insert("age".to_string(), serde_json::json!(25));

        // Alice < Bob in asc → -1
        assert!(state.compare_rows_internal(&a, &c) < 0);
    }

    #[test]
    fn test_state_get_set_del() {
        let mut state = RustTakeState {
            states: HashMap::new(),
            max_bound: None,
            sort_spec: vec![],
        };

        assert!(state.get_state("k1".into()).is_none());

        state
            .set_state("k1".into(), r#"{"size":5,"bound":null}"#.into())
            .unwrap();
        let got = state.get_state("k1".into()).unwrap();
        let parsed: TakeState = serde_json::from_str(&got).unwrap();
        assert_eq!(parsed.size, 5);
        assert!(parsed.bound.is_none());

        state.del_state("k1".into());
        assert!(state.get_state("k1".into()).is_none());
    }

    #[test]
    fn test_max_bound_update() {
        let mut state = RustTakeState {
            states: HashMap::new(),
            max_bound: None,
            sort_spec: vec![SortSpec {
                field: "id".into(),
                direction: SortDirection::Asc,
            }],
        };

        // First set: should always update max (no existing max)
        state
            .set_state_and_maybe_update_max(
                "k1".into(),
                r#"{"size":1,"bound":{"id":5}}"#.into(),
            )
            .unwrap();
        let max = state.get_max_bound().unwrap();
        assert!(max.contains("5"));

        // Second set with higher bound: should update
        state
            .set_state_and_maybe_update_max(
                "k2".into(),
                r#"{"size":1,"bound":{"id":10}}"#.into(),
            )
            .unwrap();
        let max = state.get_max_bound().unwrap();
        assert!(max.contains("10"));

        // Third set with lower bound: should NOT update
        state
            .set_state_and_maybe_update_max(
                "k3".into(),
                r#"{"size":1,"bound":{"id":3}}"#.into(),
            )
            .unwrap();
        let max = state.get_max_bound().unwrap();
        assert!(max.contains("10")); // Still 10
    }

    #[test]
    fn test_clear() {
        let mut state = RustTakeState {
            states: HashMap::new(),
            max_bound: None,
            sort_spec: vec![],
        };

        state
            .set_state("k1".into(), r#"{"size":1,"bound":null}"#.into())
            .unwrap();
        state.max_bound = Some(HashMap::new());

        state.clear();
        assert!(state.get_state("k1".into()).is_none());
        assert!(state.get_max_bound().is_none());
        assert_eq!(state.size(), 0);
    }
}
