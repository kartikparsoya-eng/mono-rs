use std::collections::HashMap;

use napi_derive::napi;

/// Generic key-value storage backed by a Rust HashMap.
/// Stores raw JSON strings — no parsing or validation.
#[napi]
pub struct RustStorage {
    data: HashMap<String, String>,
}

#[napi]
impl RustStorage {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    #[napi]
    pub fn get(&self, key: String) -> Option<String> {
        self.data.get(&key).cloned()
    }

    #[napi]
    pub fn set(&mut self, key: String, value: String) {
        self.data.insert(key, value);
    }

    #[napi]
    pub fn del(&mut self, key: String) {
        self.data.remove(&key);
    }

    /// Scan all keys with a given prefix. Returns array of [key, value] pairs.
    #[napi]
    pub fn scan(&self, prefix: String) -> Vec<Vec<String>> {
        let mut results: Vec<Vec<String>> = self
            .data
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| vec![k.clone(), v.clone()])
            .collect();
        results.sort_by(|a, b| a[0].cmp(&b[0]));
        results
    }

    #[napi]
    pub fn clear(&mut self) {
        self.data.clear();
    }

    #[napi]
    pub fn size(&self) -> u32 {
        self.data.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_set_del() {
        let mut s = RustStorage::new();
        assert_eq!(s.get("a".into()), None);
        s.set("a".into(), "1".into());
        assert_eq!(s.get("a".into()), Some("1".into()));
        s.del("a".into());
        assert_eq!(s.get("a".into()), None);
    }

    #[test]
    fn test_scan() {
        let mut s = RustStorage::new();
        s.set("foo:1".into(), "a".into());
        s.set("foo:2".into(), "b".into());
        s.set("bar:1".into(), "c".into());
        let results = s.scan("foo:".into());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], vec!["foo:1", "a"]);
        assert_eq!(results[1], vec!["foo:2", "b"]);
    }
}
