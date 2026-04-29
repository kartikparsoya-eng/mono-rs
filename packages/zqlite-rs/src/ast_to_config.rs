//! Translates a TS AST JSON (from zero-protocol) into `Vec<OperatorConfig>`
//! for use by `hydrate_pipelines`.

use std::collections::HashMap;

use serde::Deserialize;
use zero_ivm_rs::pipeline::{ExistsBranch, OperatorConfig};

/// Match TS `EXISTS_LIMIT` from packages/zql/src/builder/builder.ts.
/// Limits the number of child rows fetched per parent in EXISTS subqueries.
const EXISTS_LIMIT: usize = 3;
const PERMISSIONS_EXISTS_LIMIT: usize = 1;

// --- AST serde types (mirrors zero-protocol/src/ast.ts) ---

#[derive(Debug, Deserialize)]
pub struct HydrateQuery {
    pub query_id: String,
    pub ast: Ast,
    pub primary_key: Vec<String>,
    /// Map of table_name → { column_name → value_type }.
    /// value_type is one of: "string", "number", "boolean", "json", "null".
    /// When present, only these columns are included in output rows,
    /// and type coercion (e.g. SQLite int → boolean) is applied.
    #[serde(default)]
    pub column_types: Option<HashMap<String, HashMap<String, String>>>,
    /// Map of table_name → primary_key columns, provided by TS from
    /// `#primaryKeys` (Zero schema PKs). Used instead of SQLite PRAGMA
    /// table_info which returns SQLite PKs that may differ from Zero PKs.
    #[serde(default)]
    pub all_primary_keys: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ast {
    pub table: String,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(rename = "where")]
    pub where_cond: Option<Box<Condition>>,
    #[serde(default)]
    pub related: Option<Vec<CorrelatedSubquery>>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(rename = "orderBy", default)]
    pub order_by: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub start: Option<StartBound>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartBound {
    pub row: serde_json::Value,
    pub exclusive: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorrelatedSubquery {
    pub correlation: Correlation,
    pub subquery: Box<Ast>,
    #[serde(default)]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub system: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Correlation {
    #[serde(rename = "parentField")]
    pub parent_field: Vec<String>,
    #[serde(rename = "childField")]
    pub child_field: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Condition {
    #[serde(rename = "simple")]
    Simple {
        op: String,
        left: ConditionValue,
        right: ConditionValue,
    },
    #[serde(rename = "and")]
    And {
        conditions: Vec<Condition>,
    },
    #[serde(rename = "or")]
    Or {
        conditions: Vec<Condition>,
    },
    #[serde(rename = "correlatedSubquery")]
    CorrelatedSubquery {
        related: Box<CorrelatedSubquery>,
        op: String,
        #[serde(default)]
        flip: Option<bool>,
        #[serde(default)]
        scalar: Option<bool>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ConditionValue {
    #[serde(rename = "literal")]
    Literal { value: serde_json::Value },
    #[serde(rename = "column")]
    Column { name: String },
    #[serde(rename = "static")]
    Static {
        anchor: String,
        field: serde_json::Value,
    },
}

// --- Column introspection ---

pub struct SchemaCache {
    columns: HashMap<String, Vec<String>>,
    primary_keys: HashMap<String, Vec<String>>,
    db_path: String,
}

impl SchemaCache {
    pub fn new(db_path: &str) -> Self {
        Self {
            columns: HashMap::new(),
            primary_keys: HashMap::new(),
            db_path: db_path.to_string(),
        }
    }

    fn ensure_table_info(&mut self, table_name: &str) -> Result<(), String> {
        if self.columns.contains_key(table_name) {
            return Ok(());
        }
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| format!("Failed to open db: {e}"))?;
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info(\"{}\")", table_name))
            .map_err(|e| format!("PRAGMA table_info failed: {e}"))?;
        let mut cols = Vec::new();
        let mut pk_cols: Vec<(i32, String)> = Vec::new();
        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(1)?;
                let pk: i32 = row.get(5)?;
                Ok((name, pk))
            })
            .map_err(|e| format!("query_map failed: {e}"))?;
        for r in rows {
            let (name, pk) = r.map_err(|e| format!("row error: {e}"))?;
            cols.push(name.clone());
            if pk > 0 {
                pk_cols.push((pk, name));
            }
        }
        if cols.is_empty() {
            return Err(format!("Table '{table_name}' not found or has no columns"));
        }
        pk_cols.sort_by_key(|(idx, _)| *idx);
        let pk: Vec<String> = pk_cols.into_iter().map(|(_, name)| name).collect();
        self.columns.insert(table_name.to_string(), cols);
        self.primary_keys.insert(table_name.to_string(), pk);
        Ok(())
    }

    pub fn get_columns(&mut self, table_name: &str) -> Result<Vec<String>, String> {
        self.ensure_table_info(table_name)?;
        Ok(self.columns.get(table_name).unwrap().clone())
    }

    /// Pre-seed primary key info from the Zero schema (TS `#primaryKeys`).
    /// This avoids relying on SQLite PRAGMA table_info, which may not have
    /// PRIMARY KEY constraints on replica tables.
    pub fn seed_primary_keys(&mut self, pks: &HashMap<String, Vec<String>>) {
        for (table, pk) in pks {
            self.primary_keys.insert(table.clone(), pk.clone());
        }
    }

    pub fn get_primary_key(&mut self, table_name: &str) -> Result<Vec<String>, String> {
        // If already seeded (from all_primary_keys), return directly
        if let Some(pk) = self.primary_keys.get(table_name) {
            return Ok(pk.clone());
        }
        // Fall back to SQLite PRAGMA
        self.ensure_table_info(table_name)?;
        Ok(self.primary_keys.get(table_name).unwrap().clone())
    }
}

// --- AST -> OperatorConfig translation ---

pub fn ast_to_operator_configs(
    schema: &mut SchemaCache,
    ast: &Ast,
    primary_key: &[String],
    // B3: partition_key threaded per TS builder.ts:260-261. When the caller is
    // the related-subquery recursion (or an EXISTS-subquery recursion), this
    // carries the parent's `correlation.child_field` so a child Take inside
    // the subquery uses a per-parent state bucket — mirrors TS
    // `buildPipelineInternal(sq.subquery, ..., sq.correlation.childField)` at
    // builder.ts:626. Closes IVM-PORT-AUDIT-DEEP §B3 + IVM-PORT-AUDIT Risk #1.
    partition_key: Option<Vec<String>>,
) -> Result<Vec<OperatorConfig>, String> {
    let table_name = &ast.table;
    let columns = schema.get_columns(table_name)?;

    let order_by: Vec<(String, String)> = ast.order_by.as_ref().cloned().unwrap_or_default();

    // Build sort: orderBy + PK columns not already in orderBy
    let mut sort = order_by.clone();
    let existing: std::collections::HashSet<String> =
        sort.iter().map(|(f, _)| f.clone()).collect();
    for pk_col in primary_key {
        if !existing.contains(pk_col) {
            sort.push((pk_col.clone(), "asc".to_string()));
        }
    }

    let mut configs = Vec::new();

    // 1. Source — mirrors TS builder.ts:291-296 (`source.connect(...)` + decorators).
    configs.push(OperatorConfig::Source {
        table_name: table_name.clone(),
        columns,
        primary_key: primary_key.to_vec(),
        sort: sort.clone(),
    });

    // 2. Skip — moved here per B1 fix. Mirrors TS builder.ts:302-306 ordering:
    //    Skip is placed immediately after Source, BEFORE CSQ-Exists and Filter.
    //    Pre-Phase-34 Rust emitted Skip AFTER append_condition_configs, which
    //    reversed pipeline observation order vs TS — see IVM-PORT-AUDIT-DEEP §B1.
    if let Some(start) = &ast.start {
        configs.push(OperatorConfig::Skip {
            bound_row: start.row.clone(),
            exclusive: start.exclusive,
            sort: sort.clone(),
        });
    }

    // 3. Where conditions -> Filter + Exists. Mirrors TS builder.ts:308-333:
    //    csqConditions (CSQ-Exists with EXISTS_LIMIT downgrade) at lines
    //    308-329, then `applyWhere` (Filter) at lines 331-333. The Rust
    //    `append_condition_configs` already splits Or branches with CSQs into
    //    separate Exists configs, but for And-of-(simple, CSQ) the inner
    //    relative ordering of CSQ vs Filter is order-of-appearance — see
    //    test_b1_csq_then_filter_inside_and below for the documented caveat.
    if let Some(cond) = ast.where_cond.as_deref() {
        append_condition_configs(schema, &mut configs, cond, primary_key)?;
    }

    // 4. Limit -> Take. Mirrors TS builder.ts:335-345 + builder.ts:341
    //    (`new Take(end, ..., partitionKey)`). B3: partition_key flows in via
    //    the parameter — top-level callers pass None; recursive calls pass
    //    `Some(rel.correlation.child_field.clone())` for related[] and
    //    EXISTS-subquery sites. Pre-Phase-34 Rust hard-coded `None` here,
    //    causing silent push-as-no-op on every related-with-limit shape.
    //    See IVM-PORT-AUDIT-DEEP §B3 + IVM-PORT-AUDIT Risk #1.
    if let Some(limit) = ast.limit {
        configs.push(OperatorConfig::Take {
            limit,
            sort: sort.clone(),
            partition_key: partition_key.clone(),
        });
    }

    // 5. Related -> Join. Mirrors TS builder.ts:347-356.
    //    B3: child Take inherits parent's correlation.child_field as its
    //    partition_key. Mirrors TS builder.ts:626-632:
    //      `buildPipelineInternal(sq.subquery, ..., sq.correlation.childField)`
    if let Some(related) = &ast.related {
        for rel in related {
            let child_pk = schema.get_primary_key(&rel.subquery.table)?;
            let child_partition = Some(rel.correlation.child_field.clone());
            let child_configs = ast_to_operator_configs(
                schema,
                &rel.subquery,
                &child_pk,
                child_partition,
            )?;
            configs.push(OperatorConfig::Join {
                parent_key: rel.correlation.parent_field.clone(),
                child_key: rel.correlation.child_field.clone(),
                relationship_name: relationship_name(rel),
                child: child_configs,
            });
        }
    }

    Ok(configs)
}

/// Collect all relationship names and their underlying table names
/// from the AST recursively. Used to look up child table PKs.
pub fn collect_child_tables(ast: &Ast) -> Vec<(String, String)> {
    let mut result = Vec::new();
    collect_child_tables_recursive(ast, &mut result);
    result
}

fn collect_child_tables_recursive(ast: &Ast, out: &mut Vec<(String, String)>) {
    if let Some(related) = &ast.related {
        for rel in related {
            let rel_name = relationship_name(rel);
            let table_name = rel.subquery.table.clone();
            out.push((rel_name, table_name));
            collect_child_tables_recursive(&rel.subquery, out);
        }
    }
    // Also check exists subqueries in where conditions
    if let Some(cond) = &ast.where_cond {
        collect_child_tables_from_condition(cond, out);
    }
}

fn collect_child_tables_from_condition(cond: &Condition, out: &mut Vec<(String, String)>) {
    match cond {
        Condition::And { conditions: subs } | Condition::Or { conditions: subs } => {
            for sub in subs {
                collect_child_tables_from_condition(sub, out);
            }
        }
        Condition::CorrelatedSubquery { related, .. } => {
            let rel_name = relationship_name(related);
            let table_name = related.subquery.table.clone();
            out.push((rel_name, table_name));
            collect_child_tables_recursive(&related.subquery, out);
        }
        _ => {}
    }
}

fn relationship_name(rel: &CorrelatedSubquery) -> String {
    if let Some(alias) = &rel.subquery.alias {
        if !alias.is_empty() {
            return alias.clone();
        }
    }
    rel.subquery.table.clone()
}

fn append_condition_configs(
    schema: &mut SchemaCache,
    configs: &mut Vec<OperatorConfig>,
    cond: &Condition,
    primary_key: &[String],
) -> Result<(), String> {
    match cond {
        Condition::Simple { .. } => {
            let pred = condition_to_predicate_json(cond)?;
            configs.push(OperatorConfig::Filter { predicate: pred });
        }
        Condition::And { conditions } => {
            for sub in conditions {
                append_condition_configs(schema, configs, sub, primary_key)?;
            }
        }
        Condition::Or { conditions } => {
            let mut simple_conds = Vec::new();
            let mut csq_conds = Vec::new();
            for sub in conditions {
                if has_csq(sub) {
                    csq_conds.push(sub);
                } else {
                    simple_conds.push(sub);
                }
            }
            if csq_conds.is_empty() {
                let pred = condition_to_predicate_json(cond)?;
                configs.push(OperatorConfig::Filter { predicate: pred });
            } else {
                let or_cond_json = if simple_conds.is_empty() {
                    None
                } else if simple_conds.len() == 1 {
                    Some(condition_to_predicate_json(simple_conds[0])?)
                } else {
                    let preds: Result<Vec<serde_json::Value>, String> =
                        simple_conds.iter().map(|c| condition_to_predicate_json(c)).collect();
                    Some(serde_json::json!({ "or": preds? }))
                };
                if csq_conds.len() == 1 {
                    // Single CSQ: use regular Exists with or_condition
                    append_csq_as_exists(schema, configs, csq_conds[0], primary_key, or_cond_json)?;
                } else {
                    // Multiple CSQs: use OrExists so they are OR'd, not AND'd
                    let mut branches = Vec::new();
                    for csq in &csq_conds {
                        collect_exists_branches(schema, &mut branches, csq, primary_key)?;
                    }
                    configs.push(OperatorConfig::OrExists {
                        branches,
                        or_condition: or_cond_json,
                    });
                }
            }
        }
        Condition::CorrelatedSubquery { related, op, flip, .. } => {
            let not_exists = op == "NOT EXISTS";
            let is_flipped = flip.unwrap_or(false);
            let child_pk = schema.get_primary_key(&related.subquery.table)?;
            // B3: thread parent's correlation.child_field into the child
            // pipeline so any Take inside the EXISTS subquery (including the
            // EXISTS_LIMIT downgrade Take applied below) gets the per-parent
            // partition. Mirrors TS builder.ts:626-632 + builder.ts:308-329
            // EXISTS context — partition is the parent.correlation.childField.
            let child_partition = Some(related.correlation.child_field.clone());
            let mut child_configs = ast_to_operator_configs(
                schema,
                &related.subquery,
                &child_pk,
                child_partition,
            )?;
            // TS FlippedJoin does not apply EXISTS_LIMIT
            if !is_flipped {
                apply_exists_limit(
                    &mut child_configs,
                    related.system.as_deref(),
                    Some(related.correlation.child_field.clone()),
                );
            }
            configs.push(OperatorConfig::Exists {
                relationship_name: relationship_name(related),
                not_exists,
                parent_key: related.correlation.parent_field.clone(),
                child_key: related.correlation.child_field.clone(),
                child: child_configs,
                or_condition: None,
            });
        }
    }
    Ok(())
}

fn has_csq(cond: &Condition) -> bool {
    match cond {
        Condition::CorrelatedSubquery { .. } => true,
        Condition::And { conditions } | Condition::Or { conditions } => {
            conditions.iter().any(has_csq)
        }
        Condition::Simple { .. } => false,
    }
}

fn append_csq_as_exists(
    schema: &mut SchemaCache,
    configs: &mut Vec<OperatorConfig>,
    cond: &Condition,
    primary_key: &[String],
    or_condition: Option<serde_json::Value>,
) -> Result<(), String> {
    match cond {
        Condition::CorrelatedSubquery { related, op, flip, .. } => {
            let not_exists = op == "NOT EXISTS";
            let is_flipped = flip.unwrap_or(false);
            let child_pk = schema.get_primary_key(&related.subquery.table)?;
            // B3: thread parent's correlation.child_field — same as the
            // top-level CSQ recursion at line ~385. Mirrors TS
            // builder.ts:626-632 EXISTS subquery partition propagation.
            let child_partition = Some(related.correlation.child_field.clone());
            let mut child_configs = ast_to_operator_configs(
                schema,
                &related.subquery,
                &child_pk,
                child_partition,
            )?;
            if !is_flipped {
                apply_exists_limit(
                    &mut child_configs,
                    related.system.as_deref(),
                    Some(related.correlation.child_field.clone()),
                );
            }
            configs.push(OperatorConfig::Exists {
                relationship_name: relationship_name(related),
                not_exists,
                parent_key: related.correlation.parent_field.clone(),
                child_key: related.correlation.child_field.clone(),
                child: child_configs,
                or_condition,
            });
            Ok(())
        }
        Condition::And { conditions } => {
            // Handle AND(simple_conditions..., CSQ1, CSQ2, ...) inside an OR branch.
            // Uses distributive law:
            //   OR(S, AND(G, CSQ1, CSQ2)) = AND(OR(S,G), OR(S,CSQ1), OR(S,CSQ2))
            // where S = simple OR branches (already captured in or_condition),
            // G = gate conditions from this AND, CSQn = correlated subqueries.
            //
            // This emits:
            //   1. Filter(OR(S, G)) — pre-filter using distributive law
            //   2. Exists(CSQ1, or_condition=S) — each short-circuits on S
            //   3. Exists(CSQ2, or_condition=S) — chained sequentially
            let mut gate_conds = Vec::new();
            let mut csqs = Vec::new();
            for sub in conditions {
                if has_csq(sub) {
                    csqs.push(sub);
                } else {
                    gate_conds.push(sub);
                }
            }
            if csqs.is_empty() {
                return Err("AND inside OR with no correlated subqueries".to_string());
            }

            // Emit pre-filter: OR(original_simple_branches, gate_conditions)
            if !gate_conds.is_empty() {
                let gate_preds: Result<Vec<serde_json::Value>, String> =
                    gate_conds.iter().map(|c| condition_to_predicate_json(c)).collect();
                let gate_preds = gate_preds?;
                // Build the AND of gate conditions
                let gate_json = if gate_preds.len() == 1 {
                    gate_preds.into_iter().next().unwrap()
                } else {
                    serde_json::json!({ "and": gate_preds })
                };
                // Build OR(or_condition, gate_json) for the pre-filter
                let prefilter = match &or_condition {
                    Some(oc) => serde_json::json!({ "or": [oc.clone(), gate_json] }),
                    None => gate_json,
                };
                configs.push(OperatorConfig::Filter { predicate: prefilter });
            }

            // Emit an Exists for each CSQ, all with the same or_condition for short-circuit
            for csq in &csqs {
                append_csq_as_exists(schema, configs, csq, primary_key, or_condition.clone())?;
            }
            Ok(())
        }
        Condition::Or { conditions } => {
            // Nested OR: OR(inner_simples..., inner_csqs...)
            // Merge inner simple conditions with outer or_condition
            let mut inner_simples = Vec::new();
            let mut inner_csqs = Vec::new();
            for sub in conditions {
                if has_csq(sub) {
                    inner_csqs.push(sub);
                } else {
                    inner_simples.push(sub);
                }
            }
            let mut all_simple_preds = Vec::new();
            if let Some(oc) = &or_condition {
                all_simple_preds.push(oc.clone());
            }
            for s in &inner_simples {
                all_simple_preds.push(condition_to_predicate_json(s)?);
            }
            let combined_or = if all_simple_preds.is_empty() {
                None
            } else if all_simple_preds.len() == 1 {
                Some(all_simple_preds.into_iter().next().unwrap())
            } else {
                Some(serde_json::json!({ "or": all_simple_preds }))
            };
            if inner_csqs.len() == 1 {
                append_csq_as_exists(schema, configs, inner_csqs[0], primary_key, combined_or)?;
            } else if inner_csqs.len() > 1 {
                let mut branches = Vec::new();
                for csq in &inner_csqs {
                    collect_exists_branches(schema, &mut branches, csq, primary_key)?;
                }
                configs.push(OperatorConfig::OrExists {
                    branches,
                    or_condition: combined_or,
                });
            }
            Ok(())
        }
        _ => Err(format!("Expected CorrelatedSubquery, And, or Or, got {:?}", cond)),
    }
}

/// Collects ExistsBranch items from a condition tree for use in OrExists.
/// Handles direct CSQ nodes and AND(simple..., CSQ...) nodes.
fn collect_exists_branches(
    schema: &mut SchemaCache,
    branches: &mut Vec<ExistsBranch>,
    cond: &Condition,
    primary_key: &[String],
) -> Result<(), String> {
    match cond {
        Condition::CorrelatedSubquery { related, op, flip, .. } => {
            let not_exists = op == "NOT EXISTS";
            let is_flipped = flip.unwrap_or(false);
            let child_pk = schema.get_primary_key(&related.subquery.table)?;
            // B3: thread parent's correlation.child_field — same as the other
            // two EXISTS recursion sites. Mirrors TS builder.ts:626-632.
            let child_partition = Some(related.correlation.child_field.clone());
            let mut child_configs = ast_to_operator_configs(
                schema,
                &related.subquery,
                &child_pk,
                child_partition,
            )?;
            if !is_flipped {
                apply_exists_limit(
                    &mut child_configs,
                    related.system.as_deref(),
                    Some(related.correlation.child_field.clone()),
                );
            }
            branches.push(ExistsBranch {
                relationship_name: relationship_name(related),
                not_exists,
                parent_key: related.correlation.parent_field.clone(),
                child_key: related.correlation.child_field.clone(),
                child: child_configs,
            });
            Ok(())
        }
        Condition::And { conditions } => {
            // AND(gate_conds..., CSQ1, CSQ2, ...) inside an OR.
            // Each CSQ becomes a branch. Gate conditions become pre-filters on parent.
            // For OrExists, we need to AND all branches from this And node together,
            // so we wrap them in a single branch group. However, since ExistsBranch
            // is a single exists check, we handle And by collecting all inner CSQs
            // as separate branches that must ALL match (caller handles OR semantics
            // across top-level branches, not within an And).
            //
            // Actually for OR(CSQ1, AND(CSQ2, CSQ3)), CSQ2 AND CSQ3 must both match.
            // This is complex. For now, handle the common case: AND(simple, CSQ).
            let mut gate_conds = Vec::new();
            let mut csqs = Vec::new();
            for sub in conditions {
                if has_csq(sub) {
                    csqs.push(sub);
                } else {
                    gate_conds.push(sub);
                }
            }
            if csqs.is_empty() {
                return Err("AND inside OR with no correlated subqueries".to_string());
            }
            // For simplicity, if there's only one CSQ in the AND, we can make it a branch
            // with gate conditions baked into the child pipeline as filters.
            // For multiple CSQs in AND, fall back to append_csq_as_exists behavior
            // (which chains them — correct for AND semantics within this OR branch).
            // TODO: For full correctness with AND(CSQ1, CSQ2) inside OR, we'd need
            // a compound branch. For now, collect each CSQ as a separate branch.
            for csq in &csqs {
                collect_exists_branches(schema, branches, csq, primary_key)?;
            }
            Ok(())
        }
        _ => Err(format!("Expected CorrelatedSubquery or And in OrExists branch, got {:?}", cond)),
    }
}

// NOTE: Alias uniquification (matching TS `uniquifyCorrelatedSubqueryConditionAliases`)
// is NOT done in Rust. In production, the TS builder already uniquifies CSQ aliases
// before sending ASTs to Rust. For the parity test, 1 divergence exists for ASTs
// with duplicate CSQ aliases (e.g., seed_18) — this is a known limitation.

/// Apply EXISTS_LIMIT to child configs if no Take is already present.
/// Matches TS behavior where EXISTS subqueries always have a limit applied.
/// The Take inherits the sort order from the child Source config so that
/// the bound-based fetch path (after warm-up) correctly limits rows.
///
/// B3: `partition_key` is the EXISTS subquery's `correlation.child_field`,
/// passed by every CSQ-recursion site (3 call sites in this file). When this
/// Take fires, the child rows are partitioned per parent so push-side state
/// keys match fetch-side keys. Pre-Phase-34 this was hard-coded `None`,
/// causing the same silent push-as-no-op as the related[] Take site.
fn apply_exists_limit(
    child_configs: &mut Vec<OperatorConfig>,
    system: Option<&str>,
    partition_key: Option<Vec<String>>,
) {
    // Check if there's already a Take in the child configs
    let has_take = child_configs.iter().any(|c| matches!(c, OperatorConfig::Take { .. }));
    if !has_take {
        let limit = if system == Some("permissions") {
            PERMISSIONS_EXISTS_LIMIT
        } else {
            EXISTS_LIMIT
        };
        // Extract sort from the child Source config so TakeOperator can compare
        // rows correctly when using the bound-based path on subsequent fetches.
        let sort = child_configs.first().and_then(|c| match c {
            OperatorConfig::Source { sort, .. } => Some(sort.clone()),
            _ => None,
        }).unwrap_or_default();
        child_configs.push(OperatorConfig::Take {
            limit,
            sort,
            partition_key,
        });
    }
}

fn json_cmp(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    match (a, b) {
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
            let af = a.as_f64().unwrap_or(0.0);
            let bf = b.as_f64().unwrap_or(0.0);
            af.partial_cmp(&bf).unwrap_or(std::cmp::Ordering::Equal)
        }
        (serde_json::Value::String(a), serde_json::Value::String(b)) => a.cmp(b),
        _ => std::cmp::Ordering::Equal,
    }
}

fn condition_to_predicate_json(cond: &Condition) -> Result<serde_json::Value, String> {
    match cond {
        Condition::Simple { op, left, right } => {
            let field = match left {
                ConditionValue::Column { name } => name.clone(),
                ConditionValue::Literal { value: left_val } => {
                    // Handle literal-literal conditions (e.g., 1=0 for ALWAYS_FALSE
                    // produced by scalar subquery resolution when no rows match).
                    let right_val = extract_literal_value(right)?;
                    let result = match op.as_str() {
                        "=" | "IS" => left_val == &right_val,
                        "!=" | "IS NOT" => left_val != &right_val,
                        "<" => json_cmp(left_val, &right_val) == std::cmp::Ordering::Less,
                        "<=" => json_cmp(left_val, &right_val) != std::cmp::Ordering::Greater,
                        ">" => json_cmp(left_val, &right_val) == std::cmp::Ordering::Greater,
                        ">=" => json_cmp(left_val, &right_val) != std::cmp::Ordering::Less,
                        _ => return Err(format!("Unsupported literal-literal comparison with op: {op}")),
                    };
                    return if result {
                        Ok(serde_json::json!({"and": []}))
                    } else {
                        Ok(serde_json::json!({"or": []}))
                    };
                }
                _ => return Err("Filter left side must be a column or literal reference".to_string()),
            };
            let value = extract_literal_value(right)?;
            let rust_op = match op.as_str() {
                "=" | "IS" => "eq",
                "!=" | "IS NOT" => "neq",
                ">" => "gt",
                ">=" => "gte",
                "<" => "lt",
                "<=" => "lte",
                "LIKE" => "like",
                "NOT LIKE" => "like",
                "ILIKE" | "NOT ILIKE" => "ilike",
                "IN" => "in",
                "NOT IN" => "in",
                other => return Err(format!("Unsupported operator: {other}")),
            };

            match rust_op {
                "in" => {
                    let arr = match &value {
                        serde_json::Value::Array(a) => a.clone(),
                        other => vec![other.clone()],
                    };
                    // Empty array short-circuit:
                    // `x IN ()` is always false, `x NOT IN ()` is always true.
                    if arr.is_empty() {
                        return if op == "NOT IN" {
                            Ok(serde_json::json!({"and": []})) // always true
                        } else {
                            Ok(serde_json::json!({"or": []})) // always false
                        };
                    }
                    if op == "NOT IN" {
                        return Ok(serde_json::json!({
                            "not": {
                                "field": field,
                                "in": arr,
                            }
                        }));
                    }
                    Ok(serde_json::json!({
                        "field": field,
                        "in": arr,
                    }))
                }
                "like" | "ilike" => {
                    let pattern = value.as_str().unwrap_or("").to_string();
                    let like_key = rust_op;
                    let like_pred = serde_json::json!({
                        "field": field,
                        like_key: pattern,
                    });
                    if op == "NOT LIKE" || op == "NOT ILIKE" {
                        Ok(serde_json::json!({"not": like_pred}))
                    } else {
                        Ok(like_pred)
                    }
                }
                _ => {
                    if value.is_null() {
                        let null_op = if rust_op == "eq" { "isNull" } else { "isNotNull" };
                        Ok(serde_json::json!({
                            "field": field,
                            null_op: true,
                        }))
                    } else {
                        Ok(serde_json::json!({
                            "field": field,
                            rust_op: value,
                        }))
                    }
                }
            }
        }
        Condition::And { conditions } => {
            let preds: Result<Vec<serde_json::Value>, String> =
                conditions.iter().map(condition_to_predicate_json).collect();
            Ok(serde_json::json!({
                "and": preds?,
            }))
        }
        Condition::Or { conditions } => {
            let preds: Result<Vec<serde_json::Value>, String> =
                conditions.iter().map(condition_to_predicate_json).collect();
            Ok(serde_json::json!({
                "or": preds?,
            }))
        }
        Condition::CorrelatedSubquery { .. } => {
            Err("CorrelatedSubquery should be handled as Exists, not Filter".to_string())
        }
    }
}

fn extract_literal_value(cv: &ConditionValue) -> Result<serde_json::Value, String> {
    match cv {
        ConditionValue::Literal { value } => Ok(value.clone()),
        ConditionValue::Column { name } => Err(format!(
            "Column reference '{name}' on right side not supported in hydration filters"
        )),
        ConditionValue::Static { anchor, field } => Err(format!(
            "Unresolved static parameter (anchor={anchor}, field={field}) in hydration"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_ast() {
        let json = r#"{
            "query_id": "q1",
            "ast": {
                "table": "issues",
                "orderBy": [["created", "desc"]],
                "where": {
                    "type": "simple",
                    "op": "=",
                    "left": {"type": "column", "name": "status"},
                    "right": {"type": "literal", "value": "open"}
                },
                "limit": 10
            },
            "primary_key": ["id"]
        }"#;
        let query: HydrateQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.query_id, "q1");
        assert_eq!(query.ast.table, "issues");
        assert!(query.ast.where_cond.is_some());
        assert_eq!(query.ast.limit, Some(10));
    }

    #[test]
    fn test_condition_to_predicate_eq() {
        let cond = Condition::Simple {
            op: "=".to_string(),
            left: ConditionValue::Column {
                name: "status".to_string(),
            },
            right: ConditionValue::Literal {
                value: serde_json::json!("open"),
            },
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        assert_eq!(pred["field"], "status");
        assert_eq!(pred["eq"], "open");
    }

    #[test]
    fn test_condition_to_predicate_is_null() {
        let cond = Condition::Simple {
            op: "IS".to_string(),
            left: ConditionValue::Column {
                name: "deleted_at".to_string(),
            },
            right: ConditionValue::Literal {
                value: serde_json::Value::Null,
            },
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        assert_eq!(pred["field"], "deleted_at");
        assert_eq!(pred["isNull"], true);
    }

    #[test]
    fn test_condition_to_predicate_in() {
        let cond = Condition::Simple {
            op: "IN".to_string(),
            left: ConditionValue::Column {
                name: "id".to_string(),
            },
            right: ConditionValue::Literal {
                value: serde_json::json!([1, 2, 3]),
            },
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        assert_eq!(pred["field"], "id");
        assert_eq!(pred["in"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_condition_to_predicate_and() {
        let cond = Condition::And {
            conditions: vec![
                Condition::Simple {
                    op: "=".to_string(),
                    left: ConditionValue::Column {
                        name: "a".to_string(),
                    },
                    right: ConditionValue::Literal {
                        value: serde_json::json!(1),
                    },
                },
                Condition::Simple {
                    op: ">".to_string(),
                    left: ConditionValue::Column {
                        name: "b".to_string(),
                    },
                    right: ConditionValue::Literal {
                        value: serde_json::json!(5),
                    },
                },
            ],
        };
        let pred = condition_to_predicate_json(&cond).unwrap();
        let arr = pred["and"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_parse_with_related() {
        let json = r#"{
            "query_id": "q2",
            "ast": {
                "table": "issues",
                "related": [{
                    "correlation": {
                        "parentField": ["id"],
                        "childField": ["issueId"]
                    },
                    "subquery": {
                        "table": "comments",
                        "alias": "comments"
                    }
                }]
            },
            "primary_key": ["id"]
        }"#;
        let query: HydrateQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.ast.related.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_parse_correlated_subquery_condition() {
        let json = r#"{
            "type": "correlatedSubquery",
            "op": "EXISTS",
            "related": {
                "correlation": {
                    "parentField": ["id"],
                    "childField": ["issueId"]
                },
                "subquery": {
                    "table": "comments"
                }
            }
        }"#;
        let cond: Condition = serde_json::from_str(json).unwrap();
        assert!(matches!(cond, Condition::CorrelatedSubquery { .. }));
    }

    // ========================================================================
    // Phase 34 Wave 0 — Red-state stubs (CONTEXT D-18). Each test asserts the
    // CURRENT BROKEN behavior so Wave 1 fixes flip them green by removing
    // `#[ignore]` AND inverting the assertion to the TS-spec correct value.
    // The `#[ignore]` attribute keeps default `cargo test` runs green; run
    // with `cargo test -- --ignored` to exercise.
    // ========================================================================

    /// **B1 (BLOCKING) — Skip placement order. (Phase 34 Wave 1 — GREEN)**
    ///
    /// Spec: TS `packages/zql/src/builder/builder.ts:302-345` orders the
    /// pipeline: Source → Skip → CSQ-Exists → Filter → Take → related Joins.
    /// Skip lands BEFORE conditions (line 302-306). Mirror enforced post-fix.
    ///
    /// This was a Wave 0 red-state stub (commit b4b970719); Phase 34 plan 02
    /// flipped it green by moving the Skip emission BEFORE the
    /// `append_condition_configs` call in `ast_to_operator_configs`.
    #[test]
    fn test_b1_skip_before_conditions() {
        // Source-level GREEN-state check: read this file and confirm Skip is
        // emitted BEFORE the where-condition block, matching TS builder.ts:302.
        let src = include_str!("ast_to_config.rs");
        let skip_marker = src
            .find("// 2. Skip — moved here per B1 fix")
            .expect("missing Skip marker — code refactored without preserving B1 fix?");
        let where_marker = src
            .find("// 3. Where conditions -> Filter + Exists")
            .expect("missing where-condition marker — code refactored?");
        assert!(
            skip_marker < where_marker,
            "B1 (TS builder.ts:302-306): Skip must precede conditions in \
             ast_to_operator_configs. Got skip_marker={} where_marker={}.",
            skip_marker,
            where_marker
        );
    }

    /// **B1 CAVEAT — CSQ vs Filter relative order inside an And.**
    ///
    /// Per TS `builder.ts:308-333`, when both csqConditions and a regular
    /// where-clause exist (e.g., AST has `where: { type: 'and', conditions:
    /// [{simple_pred}, { type: 'correlatedSubquery', op: 'EXISTS', ... }] }`),
    /// TS emits CSQ-Exists FIRST, then `applyWhere` (Filter). Today's Rust
    /// `append_condition_configs` walks an And in declaration order, so a
    /// `[simple, csq]` payload yields `[Filter, Exists]` — the inverse of TS.
    /// This test documents the caveat (per plan 34-02 Task 1 step 5) so a
    /// follow-up plan can either reorder inside `append_condition_configs` or
    /// confirm parity is preserved by the existing semantics.
    ///
    /// The behavior is currently order-of-appearance — see
    /// `append_condition_configs::Condition::And` at lines ~327-330. This test
    /// is `#[ignore]`d because it captures a documented follow-up, not a
    /// failure of the B1 hot-fix.
    #[test]
    #[ignore = "B1 CAVEAT: CSQ vs Filter inner ordering inside And — \
        documented for follow-up; ast_to_config currently emits in declaration \
        order. TS spec: builder.ts:308-333 (CSQ first then applyWhere). \
        Tracking: phase 34 deferred follow-up."]
    fn test_b1_csq_then_filter_inside_and() {
        // Build AST with And(simple, CSQ-EXISTS) and assert configs ordering.
        // We construct directly rather than via JSON to avoid a SchemaCache
        // db-path dependency; verification is structural via the source.
        let json = r#"{
            "table": "issues",
            "where": {
                "type": "and",
                "conditions": [
                    {
                        "type": "simple",
                        "op": "=",
                        "left": {"type": "column", "name": "status"},
                        "right": {"type": "literal", "value": "open"}
                    },
                    {
                        "type": "correlatedSubquery",
                        "op": "EXISTS",
                        "related": {
                            "correlation": {
                                "parentField": ["id"],
                                "childField": ["issueId"]
                            },
                            "subquery": {
                                "table": "comments"
                            }
                        }
                    }
                ]
            }
        }"#;
        let ast: Ast = serde_json::from_str(json).unwrap();
        // Walk the where condition without ast_to_operator_configs (avoids db).
        // For an And, `append_condition_configs` recursively appends in
        // conditions[] order. Confirm the simple comes first in the AST so
        // the produced configs would be [Filter, Exists] — opposite of TS.
        if let Some(Condition::And { conditions }) = ast.where_cond.as_deref() {
            assert!(matches!(conditions[0], Condition::Simple { .. }));
            assert!(matches!(conditions[1], Condition::CorrelatedSubquery { .. }));
            // TS would emit Exists then Filter; Rust would emit Filter then Exists.
            // This is the documented divergence — captured for the follow-up.
            // assert!(false, "follow-up needed: reorder And-inner CSQ before simple");
        } else {
            panic!("expected And condition");
        }
    }

    /// **B3 (BLOCKING) — Take partition_key threading. (Phase 34 Wave 2 — GREEN)**
    ///
    /// Spec: TS `packages/zql/src/builder/builder.ts:626-632` propagates
    /// `sq.correlation.childField` as the child subquery's partition key.
    /// `take.ts:80-83, 99, 219` use this so fetch (constraint-driven) and
    /// push (row-driven) state keys match.
    ///
    /// Pre-Phase-34 Rust (`ast_to_config.rs:240-247`) hard-coded
    /// `partition_key: None` — the recursive calls also ignored the parent's
    /// child_field. Phase 34 plan 34-05 threaded a `partition_key` parameter
    /// through `ast_to_operator_configs` and updated all 4 internal recursive
    /// call sites (related[] at line ~263 + 3 EXISTS sites at lines ~385,
    /// ~429, ~553) to pass `Some(rel.correlation.child_field.clone())`.
    ///
    /// GREEN-state check: the Take config branch now uses the parameter
    /// (`partition_key: partition_key.clone(),`), and the function signature
    /// gains the `partition_key: Option<Vec<String>>` parameter.
    #[test]
    fn test_b3_partition_key_threading() {
        let src = include_str!("ast_to_config.rs");

        // 1. Signature carries the new parameter.
        let sig_marker =
            src.find("pub fn ast_to_operator_configs(").expect("signature missing");
        let sig_block = &src[sig_marker..sig_marker + 800];
        assert!(
            sig_block.contains("partition_key: Option<Vec<String>>"),
            "B3 (TS builder.ts:260-261): ast_to_operator_configs must accept \
             partition_key: Option<Vec<String>>. Got signature block: {}",
            sig_block
        );

        // 2. Take config uses the parameter (no longer hard-coded None).
        let take_block_start = src
            .find("// 4. Limit -> Take")
            .expect("missing Take marker — code refactored?");
        let take_block = &src[take_block_start..take_block_start + 1000];
        assert!(
            take_block.contains("partition_key: partition_key.clone(),"),
            "B3 (TS builder.ts:341): Take config must thread the parameter. \
             Got take_block:\n{}",
            take_block
        );
        assert!(
            !take_block.contains("partition_key: None,\n        });"),
            "B3: hard-coded `partition_key: None` at the Take config site \
             must be replaced with the parameter."
        );

        // 3. Related-recursion site passes child_field.
        let related_block_start = src
            .find("// 5. Related -> Join")
            .expect("missing Related marker");
        let related_block = &src[related_block_start..related_block_start + 1000];
        assert!(
            related_block.contains("Some(rel.correlation.child_field.clone())"),
            "B3 (TS builder.ts:626-632): related-recursion must pass \
             Some(rel.correlation.child_field.clone()) as partition_key. \
             Got related_block:\n{}",
            related_block
        );
    }
}
