use napi_derive::napi;
use serde::{Deserialize, Serialize};

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ExistsChangeInput {
    #[serde(rename = "type")]
    change_type: String,
    #[serde(rename = "childRelationship")]
    child_relationship: Option<String>,
    #[serde(rename = "childChangeType")]
    child_change_type: Option<String>,
    #[serde(rename = "fetchedSize")]
    fetched_size: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct ExistsActionOutput {
    action: String,
    exists: Option<bool>,
}

// ─── Decision logic ─────────────────────────────────────────────────────────

fn decide_exists_push(
    change: &ExistsChangeInput,
    relationship_name: &str,
    not_exists: bool,
) -> ExistsActionOutput {
    match change.change_type.as_str() {
        "add" | "edit" | "remove" => ExistsActionOutput {
            action: "pass_filter".to_string(),
            exists: None,
        },
        "child" => {
            let child_rel = change.child_relationship.as_deref().unwrap_or("");
            if child_rel != relationship_name {
                return ExistsActionOutput {
                    action: "pass_filter".to_string(),
                    exists: None,
                };
            }

            let child_type = change.child_change_type.as_deref().unwrap_or("");
            if child_type == "edit" || child_type == "child" {
                return ExistsActionOutput {
                    action: "pass_filter".to_string(),
                    exists: None,
                };
            }

            let size = change.fetched_size.unwrap_or(0);

            match child_type {
                "add" => {
                    if size == 1 {
                        if not_exists {
                            ExistsActionOutput {
                                action: "convert_remove".to_string(),
                                exists: None,
                            }
                        } else {
                            ExistsActionOutput {
                                action: "convert_add".to_string(),
                                exists: None,
                            }
                        }
                    } else {
                        ExistsActionOutput {
                            action: "pass_filter_with_exists".to_string(),
                            exists: Some(size > 0),
                        }
                    }
                }
                "remove" => {
                    if size == 0 {
                        if not_exists {
                            ExistsActionOutput {
                                action: "convert_add".to_string(),
                                exists: None,
                            }
                        } else {
                            ExistsActionOutput {
                                action: "convert_remove".to_string(),
                                exists: None,
                            }
                        }
                    } else {
                        ExistsActionOutput {
                            action: "pass_filter_with_exists".to_string(),
                            exists: Some(size > 0),
                        }
                    }
                }
                _ => ExistsActionOutput {
                    action: "pass_filter".to_string(),
                    exists: None,
                },
            }
        }
        _ => ExistsActionOutput {
            action: "pass_filter".to_string(),
            exists: None,
        },
    }
}

// ─── napi export ────────────────────────────────────────────────────────────

/// Batch decision function for Exists operator push path.
/// Takes JSON array of change descriptors, returns JSON array of action descriptors.
#[napi]
pub fn rust_exists_push_batch(
    changes_json: String,
    relationship_name: String,
    not_exists: bool,
) -> String {
    let changes: Vec<ExistsChangeInput> = match serde_json::from_str(&changes_json) {
        Ok(c) => c,
        Err(_) => return "[]".to_string(),
    };

    let actions: Vec<ExistsActionOutput> = changes
        .iter()
        .map(|c| decide_exists_push(c, &relationship_name, not_exists))
        .collect();

    serde_json::to_string(&actions).unwrap_or_else(|_| "[]".to_string())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_change(
        change_type: &str,
        child_rel: Option<&str>,
        child_type: Option<&str>,
        size: Option<i64>,
    ) -> ExistsChangeInput {
        ExistsChangeInput {
            change_type: change_type.to_string(),
            child_relationship: child_rel.map(|s| s.to_string()),
            child_change_type: child_type.map(|s| s.to_string()),
            fetched_size: size,
        }
    }

    #[test]
    fn test_add_pass_filter() {
        let c = make_change("add", None, None, None);
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_edit_pass_filter() {
        let c = make_change("edit", None, None, None);
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_remove_pass_filter() {
        let c = make_change("remove", None, None, None);
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_different_relationship() {
        let c = make_change("child", Some("other"), Some("add"), Some(1));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_edit_pass_filter() {
        let c = make_change("child", Some("items"), Some("edit"), Some(1));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_child_pass_filter() {
        let c = make_change("child", Some("items"), Some("child"), Some(1));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_add_size_1_exists() {
        let c = make_change("child", Some("items"), Some("add"), Some(1));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "convert_add");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_add_size_1_not_exists() {
        let c = make_change("child", Some("items"), Some("add"), Some(1));
        let r = decide_exists_push(&c, "items", true);
        assert_eq!(r.action, "convert_remove");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_add_size_gt_1() {
        let c = make_change("child", Some("items"), Some("add"), Some(3));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter_with_exists");
        assert_eq!(r.exists, Some(true));
    }

    #[test]
    fn test_child_add_size_0() {
        let c = make_change("child", Some("items"), Some("add"), Some(0));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter_with_exists");
        assert_eq!(r.exists, Some(false));
    }

    #[test]
    fn test_child_remove_size_0_exists() {
        let c = make_change("child", Some("items"), Some("remove"), Some(0));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "convert_remove");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_remove_size_0_not_exists() {
        let c = make_change("child", Some("items"), Some("remove"), Some(0));
        let r = decide_exists_push(&c, "items", true);
        assert_eq!(r.action, "convert_add");
        assert_eq!(r.exists, None);
    }

    #[test]
    fn test_child_remove_size_gt_0() {
        let c = make_change("child", Some("items"), Some("remove"), Some(2));
        let r = decide_exists_push(&c, "items", false);
        assert_eq!(r.action, "pass_filter_with_exists");
        assert_eq!(r.exists, Some(true));
    }

    #[test]
    fn test_napi_batch_multiple() {
        let input = serde_json::json!([
            {"type": "add", "childRelationship": null, "childChangeType": null, "fetchedSize": null},
            {"type": "child", "childRelationship": "items", "childChangeType": "add", "fetchedSize": 1},
            {"type": "child", "childRelationship": "items", "childChangeType": "remove", "fetchedSize": 0}
        ]);
        let result = rust_exists_push_batch(input.to_string(), "items".to_string(), false);
        let actions: Vec<ExistsActionOutput> = serde_json::from_str(&result).unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].action, "pass_filter");
        assert_eq!(actions[1].action, "convert_add");
        assert_eq!(actions[2].action, "convert_remove");
    }

    #[test]
    fn test_napi_batch_empty() {
        let result = rust_exists_push_batch("[]".to_string(), "items".to_string(), false);
        let actions: Vec<ExistsActionOutput> = serde_json::from_str(&result).unwrap();
        assert_eq!(actions.len(), 0);
    }
}
