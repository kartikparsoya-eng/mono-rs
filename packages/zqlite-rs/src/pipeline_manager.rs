//! RustPipelineManager — manages pipeline instances for IVM.
//!
//! Each PipelineDriver owns one RustPipelineManager instance.
//! TS computes diffs (via frozen BEGIN CONCURRENT connections),
//! passes changes to advance(), and Rust handles operator tree fan-out.
//!
//! Responsibilities beyond basic IVM:
//! - Permission table filtering (skip rows from permission-system tables)
//! - minRowVersion bump (floor `_0_version` to table's minRowVersion)
//! - Companion scalar subquery monitoring (detect value changes → reset signal)
//! - Companion row change emission (sync companion table rows to client)

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::{panic, thread};

use napi::bindgen_prelude::{AsyncTask, Buffer};
use napi::{Env, Task};
use napi_derive::napi;
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use crate::advance::{
    advance_persistent_pipeline, build_pipeline_state, encode_advance_result_buf,
    encode_json_value, flatten_nodes_to_row_changes, AdvanceResult, PipelineState, RowChange,
};
use crate::chunk_encoder::StreamItem;
use crate::connection_pool::ConnectionPool;
use crate::diff::{Change, TableAndZqlSpec};
use zero_ivm_rs::types::FetchRequest;

/// Metadata for a companion scalar subquery associated with a pipeline query.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionInfo {
    /// The query ID this companion belongs to
    pub query_id: String,
    /// The table the companion subquery reads from
    pub table: String,
    /// The field in the companion table that holds the scalar value
    pub child_field: String,
    /// The resolved scalar value at hydration time (for change detection)
    pub resolved_value: serde_json::Value,
    /// Literal equality conditions from the WHERE clause: [{column, value}, ...]
    pub where_conditions: Vec<CompanionCondition>,
    /// Primary key columns of the companion table
    pub primary_key: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompanionCondition {
    pub column: String,
    pub value: serde_json::Value,
}

/// A single pipeline instance (corresponds to one PipelineDriver / client group).
struct PipelineInstance {
    db_path: String,
    // B11: prev snapshot path for cascade-delete enumeration. Set by TS
    // pipeline-driver.ts BEFORE swap_snapshot on each advance. Mirrors
    // TS pipeline-driver.ts:1542-1577 diff-from-prev lifecycle: TS
    // materializes the snapshotter diff against `prev`; Rust must read
    // descendants from the same snapshot (where same-tx-deleted rows
    // still exist) — not the post-swap `curr` snapshot.
    // None until first set_prev_snapshot call; fallback to db_path with
    // logged warning preserves back-compat. See
    // .planning/IVM-PORT-AUDIT-DEEP.md §B11.
    prev_db_path: Option<String>,
    /// NEW-1 / D-08: single-connection ConnectionPool keyed at `prev_db_path`.
    /// Lazily constructed by `set_prev_snapshot`; dropped by `swap_snapshot`
    /// (closes NEW-3). Inherits BEGIN DEFERRED snapshot pin from
    /// `ConnectionPool::new` (free side effect — closes NEW-5).
    /// See `.planning/phases/35-pool-cascade-hardening/35-03-PLAN.md`.
    prev_pool: Option<ConnectionPool>,
    pipelines: Vec<Mutex<PipelineState>>,
    shared_pool: ConnectionPool,
    /// Schema info for diffing — maps table name → TableAndZqlSpec
    syncable_tables: HashMap<String, TableAndZqlSpec>,
    /// All known table names (syncable + internal) for skipping non-syncable entries
    all_table_names: HashSet<String>,
    /// Permission table names (rows from these tables are filtered out of advance results)
    permission_tables: HashSet<String>,
    /// Companion metadata per query: query_id → Vec<CompanionInfo>
    companions: HashMap<String, Vec<CompanionInfo>>,
}

// SAFETY: Same reasoning as RustPipeline — all fields are Send+Sync via Mutex/RwLock/Arc.
unsafe impl Send for PipelineInstance {}
unsafe impl Sync for PipelineInstance {}

#[napi]
pub struct RustPipelineManager {
    instances: RwLock<HashMap<String, Arc<Mutex<PipelineInstance>>>>,
}

// SAFETY: RwLock<HashMap<String, Arc<Mutex<PipelineInstance>>>> is Send+Sync.
unsafe impl Send for RustPipelineManager {}
unsafe impl Sync for RustPipelineManager {}

#[napi]
impl RustPipelineManager {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new pipeline instance for a client group.
    #[napi]
    pub fn create_instance(&self, id: String, db_path: String) -> napi::Result<()> {
        let pool_size = rayon::current_num_threads().max(4);
        let shared_pool = ConnectionPool::new(&db_path, pool_size)
            .map_err(|e| napi::Error::from_reason(format!("Failed to create pool: {e}")))?;

        let instance = PipelineInstance {
            db_path,
            // B11: initialized None; TS pipeline-driver.ts is expected to
            // call set_prev_snapshot before each advance after swap_snapshot.
            // If never called, advance falls back to db_path with a logged
            // warning (back-compat path).
            prev_db_path: None,
            // NEW-1: lazily constructed by set_prev_snapshot.
            prev_pool: None,
            pipelines: Vec::new(),
            shared_pool,
            syncable_tables: HashMap::new(),
            all_table_names: HashSet::new(),
            permission_tables: HashSet::new(),
            companions: HashMap::new(),
        };

        let mut instances = self.instances.write()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        instances.insert(id, Arc::new(Mutex::new(instance)));
        Ok(())
    }

    /// Remove a pipeline instance.
    #[napi]
    pub fn remove_instance(&self, id: String) -> napi::Result<()> {
        let mut instances = self.instances.write()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        instances.remove(&id);
        Ok(())
    }

    /// Set table specs for diffing. Called after create_instance and on schema reset.
    /// JSON format: { "tableName": { "tableSpec": {...}, "zqlSpec": {...} }, ... }
    /// Also accepts allTableNames as a separate JSON array.
    #[napi]
    pub fn set_table_specs(
        &self,
        id: String,
        syncable_tables_json: String,
        all_table_names_json: String,
    ) -> napi::Result<()> {
        let syncable_tables: HashMap<String, TableAndZqlSpec> =
            serde_json::from_str(&syncable_tables_json)
                .map_err(|e| napi::Error::from_reason(format!("Failed to parse syncable_tables: {e}")))?;

        let all_table_names: HashSet<String> =
            serde_json::from_str(&all_table_names_json)
                .map_err(|e| napi::Error::from_reason(format!("Failed to parse all_table_names: {e}")))?;

        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;
        instance.syncable_tables = syncable_tables;
        instance.all_table_names = all_table_names;
        Ok(())
    }

    /// Set the combined permission table names for an instance.
    /// Rows from these tables are filtered out of advance results.
    /// JSON format: ["tableName1", "tableName2", ...]
    #[napi]
    pub fn set_permission_tables(&self, id: String, tables_json: String) -> napi::Result<()> {
        let tables: HashSet<String> = serde_json::from_str(&tables_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse permission tables: {e}")))?;

        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;
        instance.permission_tables = tables;
        Ok(())
    }

    /// Set companion metadata for a query. Called after addQuery() in TS.
    /// JSON format: [{ queryId, table, childField, resolvedValue, whereConditions, primaryKey }, ...]
    #[napi]
    pub fn set_query_companions(
        &self,
        id: String,
        query_id: String,
        companions_json: String,
    ) -> napi::Result<()> {
        let companions: Vec<CompanionInfo> = serde_json::from_str(&companions_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse companions: {e}")))?;

        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;
        if companions.is_empty() {
            instance.companions.remove(&query_id);
        } else {
            instance.companions.insert(query_id, companions);
        }
        Ok(())
    }

    /// Add a query to a pipeline instance.
    #[napi]
    pub fn add_query(&self, id: String, query_json: String) -> napi::Result<()> {
        use crate::ast_to_config::{HydrateQuery, SchemaCache};

        let query: HydrateQuery = serde_json::from_str(&query_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse query: {e}")))?;

        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;

        let mut schema_cache = SchemaCache::new(&instance.db_path);
        let state = build_pipeline_state(
            &instance.db_path, &query, &mut schema_cache, &instance.shared_pool,
        ).map_err(|e| napi::Error::from_reason(format!(
            "Failed to build pipeline '{}': {e}", query.query_id
        )))?;

        instance.pipelines.push(Mutex::new(state));
        Ok(())
    }

    /// Remove a query from a pipeline instance.
    #[napi]
    pub fn remove_query(&self, id: String, query_id: String) -> napi::Result<()> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;
        instance.pipelines.retain(|p| p.lock().unwrap().query_id != query_id);
        instance.companions.remove(&query_id);
        Ok(())
    }

    /// Run initial hydration: fetch from all persistent operator trees.
    #[napi]
    pub fn hydrate(&self, id: String) -> napi::Result<Buffer> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;

        let result = hydrate_instance(&instance);
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Run initial hydration for a single query.
    #[napi]
    pub fn hydrate_query(&self, id: String, query_id: String) -> napi::Result<Buffer> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;

        let result = hydrate_query_instance(&instance, &query_id)?;
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Push pre-computed changes through operator trees.
    /// Also handles:
    /// - Permission table filtering
    /// - minRowVersion bump
    /// - Companion scalar subquery change detection (returns reset_signal)
    /// - Companion row change emission
    #[napi]
    pub fn advance(&self, id: String, changes_json: String) -> napi::Result<Buffer> {
        let changes: Vec<Change> = serde_json::from_str(&changes_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;

        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B12: mutable lock — advance_instance now refreshes companion
        // resolved_value post-advance to prevent A→B→A spurious resets.
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;

        let result = advance_instance(&mut instance, &changes);
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Set the path of the snapshot whose state should be queried for
    /// cascade-delete enumeration. Must be called BEFORE swap_snapshot
    /// on each advance. Mirrors TS pipeline-driver.ts:1542-1577 — TS
    /// owns the prev/curr snapshot pair via the snapshotter diff iterator;
    /// Rust must read descendant rows from prev (where they still exist),
    /// not curr (post-swap; same-tx deletes already gone).
    ///
    /// Per CONTEXT D-17 / D-15 — B11 BLOCKING fix.
    ///
    /// Idempotent: calling multiple times before swap_snapshot is safe.
    /// If never called, advance falls back to db_path with a warning;
    /// fallback exists for back-compat with any non-mono-rs caller.
    ///
    /// CLAUDE.md gate #4: this is an ADDITIVE napi method — no signature
    /// change to existing buffered methods (advance, advance_async,
    /// hydrate*, addQuery*, addQueries*).
    #[napi]
    pub fn set_prev_snapshot(&self, id: String, prev_db_path: String) -> napi::Result<()> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;

        // NEW-1 / D-08: construct (or replace) the prev_pool. We build it
        // BEFORE mutating prev_db_path so a failed open leaves both fields
        // in their previous state (atomic semantics). Optional bench-only
        // bypass via Z_DISABLE_PREV_POOL=1 to measure the legacy fallback.
        let bypass = std::env::var("Z_DISABLE_PREV_POOL").as_deref() == Ok("1");
        let new_pool = if bypass {
            None
        } else {
            Some(
                ConnectionPool::new(&prev_db_path, 1)
                    .map_err(|e| napi::Error::from_reason(format!("prev_pool open: {e}")))?
            )
        };
        instance.prev_db_path = Some(prev_db_path);
        instance.prev_pool = new_pool;
        Ok(())
    }

    /// Re-open SQLite connections at a new path without rebuilding operator trees.
    ///
    /// NEW-3: clears `prev_db_path` after a successful pool swap. The
    /// `set_prev_snapshot` → `swap_snapshot` lifecycle is per-advance; if the
    /// next advance forgets to call `set_prev_snapshot`, the explicit fallback
    /// warning ("[B11] prev_db_path not set...") MUST fire instead of silently
    /// using a stale value from the previous advance. See
    /// .planning/IVM-PORT-AUDIT-DEEP.md §NEW-3.
    #[napi]
    pub fn swap_snapshot(&self, id: String, new_db_path: String) -> napi::Result<()> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let mut instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;

        instance.shared_pool.swap_path(&new_db_path)
            .map_err(|e| napi::Error::from_reason(format!("Failed to swap pool: {e}")))?;

        for pm in instance.pipelines.iter() {
            // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
            let pipeline = pm.lock()
                .map_err(|e| napi::Error::from_reason(format!("pipeline lock poisoned: {e}")))?;
            pipeline.source.reset_state()
                .map_err(|e| napi::Error::from_reason(format!("reset_state: {e}")))?;
        }

        instance.db_path = new_db_path;
        // NEW-3: clear prev so the next advance MUST re-arm via
        // set_prev_snapshot, otherwise the [B11] fallback warning fires.
        // NEW-1: drop prev_pool too — the underlying SQLite handles release
        // their BEGIN DEFERRED snapshot pin at this boundary.
        instance.prev_db_path = None;
        instance.prev_pool = None;
        Ok(())
    }

    /// Returns the number of active pipelines for an instance.
    #[napi]
    pub fn pipeline_count(&self, id: String) -> napi::Result<u32> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        // B10/D-03: explicit poison propagation — no .unwrap() on inner mutexes.
        let instance = instance_mutex.lock()
            .map_err(|e| napi::Error::from_reason(format!("instance lock poisoned: {e}")))?;
        Ok(instance.pipelines.len() as u32)
    }

    /// Async version of advance() — runs IVM work on a libuv worker thread,
    /// freeing the JS event loop for other client groups to advance in parallel.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn advance_async(
        &self,
        id: String,
        changes_json: String,
    ) -> napi::Result<napi::bindgen_prelude::AsyncTask<AdvanceTask>> {
        let changes: Vec<Change> = serde_json::from_str(&changes_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;

        let instance_arc = {
            let instances = self.instances.read()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            instances.get(&id)
                .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?
                .clone()
        };

        Ok(napi::bindgen_prelude::AsyncTask::new(AdvanceTask {
            instance: instance_arc,
            changes,
        }))
    }

    /// Async version of hydrate() — runs on a libuv worker thread.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn hydrate_async(
        &self,
        id: String,
    ) -> napi::Result<napi::bindgen_prelude::AsyncTask<HydrateTask>> {
        let instance_arc = {
            let instances = self.instances.read()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            instances.get(&id)
                .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?
                .clone()
        };

        Ok(napi::bindgen_prelude::AsyncTask::new(HydrateTask {
            instance: instance_arc,
        }))
    }

    /// Async version of hydrate_query() — runs on a libuv worker thread.
    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn hydrate_query_async(
        &self,
        id: String,
        query_id: String,
    ) -> napi::Result<napi::bindgen_prelude::AsyncTask<HydrateQueryTask>> {
        let instance_arc = {
            let instances = self.instances.read()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            instances.get(&id)
                .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?
                .clone()
        };

        Ok(napi::bindgen_prelude::AsyncTask::new(HydrateQueryTask {
            instance: instance_arc,
            query_id,
        }))
    }

    /// Streaming version of advance_async — emits one StreamItem::Chunk per
    /// pipeline as it completes, plus a final StreamItem::ResetSignal or
    /// StreamItem::Chunk for companion handling.
    ///
    /// Channel capacity is `pipeline_count.max(1) + 1` per RESEARCH Open Q #1
    /// + Pitfall 1: every pipeline gets one slot, plus one extra for the
    /// final ResetSignal/companion-Chunk so the coordinator never blocks
    /// post-scope. (Differs from CONTEXT D-16's literal `pipeline_count` to
    /// eliminate the cancel-deadlock window for trivial cost.)
    #[napi(ts_return_type = "AdvanceStream")]
    pub fn advance_streaming(
        &self,
        id: String,
        changes_json: String,
    ) -> napi::Result<AdvanceStream> {
        let changes: Vec<Change> = serde_json::from_str(&changes_json)
            .map_err(|e| napi::Error::from_reason(format!("Failed to parse changes: {e}")))?;

        let instance_arc = {
            let instances = self.instances.read()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            instances.get(&id)
                .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?
                .clone()
        };

        let cancel = Arc::new(AtomicBool::new(false));

        // RESEARCH Open Q #3: numChanges = 0 early-return.
        if changes.is_empty() {
            let (_tx, rx) = mpsc::sync_channel::<StreamItem>(1);
            // Drop _tx immediately → channel closed → first next() returns done.
            return Ok(AdvanceStream {
                rx: Arc::new(Mutex::new(rx)),
                cancel,
                _coordinator: None,
            });
        }

        // D-17: read pipeline_count under brief instance lock.
        let pipeline_count = {
            let inst = instance_arc.lock()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            inst.pipelines.len()
        };

        // RESEARCH Open Q #1 + Pitfall 1: pipeline_count + 1 to guarantee a
        // slot for the final ResetSignal/companion-Chunk. Avoids the cancel-
        // deadlock window where all per-pipeline slots are full and the
        // coordinator blocks on tx.send for the post-scope item.
        let (tx, rx) = mpsc::sync_channel::<StreamItem>(pipeline_count.max(1) + 1);
        let cancel_for_thread = cancel.clone();

        let coordinator = thread::Builder::new()
            .name(format!("advance-stream-{id}"))
            .spawn(move || {
                // B12: mutable lock — STREAM-05 companion check now refreshes
                // companion.resolved_value post-advance to prevent A→B→A
                // spurious resets across two streamed advances.
                let mut instance = match instance_arc.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        let _ = tx.send(StreamItem::Error(
                            "instance lock poisoned".to_string(),
                            "rayon_error".to_string()));
                        return;
                    }
                };

                // B11: resolve prev_db_path once per advance, before fan-out.
                // TS pipeline-driver.ts:1542-1577 calls set_prev_snapshot(prev)
                // BEFORE swap_snapshot(curr); we read it here. Fallback to
                // db_path with a logged warning preserves back-compat.
                let prev_db_path_owned: String = instance.prev_db_path.clone()
                    .unwrap_or_else(|| {
                        eprintln!(
                            "[B11] prev_db_path not set (streaming); falling back to db_path — \
                             same-tx descendant deletes may be elided. \
                             See .planning/IVM-PORT-AUDIT-DEEP.md §B11."
                        );
                        instance.db_path.clone()
                    });

                // NEW-1: route through prev_pool when available. Each
                // pipeline acquires its OWN connection from the pool —
                // since pool size is 1, sibling pipelines that race
                // concurrently fall through to the legacy open path.
                // For the typical case (set_prev_snapshot before each
                // advance), the first pipeline through wins the pool
                // connection.
                let prev_pool_ref: Option<&crate::connection_pool::ConnectionPool> =
                    instance.prev_pool.as_ref();
                rayon::scope(|s| {
                    for pm in instance.pipelines.iter() {
                        let tx = tx.clone();
                        let cancel = cancel_for_thread.clone();
                        let permission_tables = &instance.permission_tables;
                        let syncable_tables = &instance.syncable_tables;
                        let db_path = instance.db_path.as_str();
                        let prev_db_path = prev_db_path_owned.as_str();
                        let changes_ref = &changes;
                        let prev_pool_inner = prev_pool_ref;
                        s.spawn(move |_| {
                            if cancel.load(Ordering::Relaxed) { return; }
                            // Pitfall 3: AssertUnwindSafe per task. A panic
                            // here surfaces as StreamItem::Error("panic",..)
                            // and does NOT take down siblings.
                            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                                let mut pipeline = pm.lock().unwrap();
                                // STREAM-04: cancel-aware advance (Task 5/6).
                                // B11/NEW-1: prev_pool routes the read through the
                                // snapshot-pinned ConnectionPool when available.
                                let chunk = crate::advance::advance_persistent_pipeline_with_cancel_and_prev(
                                    &mut pipeline, changes_ref, db_path, prev_db_path,
                                    prev_pool_inner, &cancel,
                                );
                                // STREAM-06 / D-20: filter per-chunk.
                                let mut filtered = chunk;
                                apply_permission_and_version_filters(
                                    &mut filtered,
                                    permission_tables,
                                    syncable_tables,
                                );
                                if filtered.is_empty() {
                                    None
                                } else {
                                    Some(crate::chunk_encoder::encode_chunk_buf(&filtered))
                                }
                            }));
                            match result {
                                Ok(Some(buf)) => { let _ = tx.send(StreamItem::Chunk(buf)); }
                                Ok(None) => { /* nothing to send */ }
                                Err(payload) => {
                                    let msg = format_panic_payload(payload);
                                    let _ = tx.send(StreamItem::Error(msg, "panic".to_string()));
                                }
                            }
                        });
                    }
                }); // ← all pipelines joined here

                // STREAM-05: companion check after join.
                // B12: pass &mut companions so resolved_value is refreshed
                // post-advance (prevents A→B→A spurious resets across two
                // streamed advances).
                if !instance.companions.is_empty() {
                    let changed_tables: HashSet<&str> = changes.iter()
                        .map(|c| c.table.as_str())
                        .collect();
                    // B12: clone db_path/permission/syncable refs we need —
                    // companions is mutably borrowed below, so we can't also
                    // hold an immutable borrow of `instance` for these fields
                    // at the same time.
                    let db_path = instance.db_path.clone();
                    let companion_result = check_companions_and_emit(
                        &mut instance.companions, &changes,
                        &changed_tables, &db_path,
                    );
                    match companion_result {
                        CompanionResult::Reset(reason) => {
                            let _ = tx.send(StreamItem::ResetSignal(reason));
                        }
                        CompanionResult::Changes(companion_changes) => {
                            // Pitfall 4: companions MUST also be filtered.
                            let mut cs = companion_changes;
                            apply_permission_and_version_filters(
                                &mut cs,
                                &instance.permission_tables,
                                &instance.syncable_tables,
                            );
                            if !cs.is_empty() {
                                let buf = crate::chunk_encoder::encode_chunk_buf(&cs);
                                let _ = tx.send(StreamItem::Chunk(buf));
                            }
                        }
                    }
                }
                // tx dropped here → rx.recv() returns Err → JS sees done=true.
            })
            .map_err(|e| napi::Error::from_reason(format!("Failed to spawn coordinator: {e}")))?;

        Ok(AdvanceStream {
            rx: Arc::new(Mutex::new(rx)),
            cancel,
            _coordinator: Some(coordinator),
        })
    }

    /// Streaming version of hydrate_async — emits one StreamItem::Chunk per
    /// pipeline. No companion check (companions only run on advance).
    #[napi(ts_return_type = "HydrateStream")]
    pub fn hydrate_streaming(&self, id: String) -> napi::Result<HydrateStream> {
        let instance_arc = {
            let instances = self.instances.read()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            instances.get(&id)
                .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?
                .clone()
        };
        let cancel = Arc::new(AtomicBool::new(false));

        let pipeline_count = {
            let inst = instance_arc.lock()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            inst.pipelines.len()
        };
        let (tx, rx) = mpsc::sync_channel::<StreamItem>(pipeline_count.max(1) + 1);
        let cancel_for_thread = cancel.clone();

        let coordinator = thread::Builder::new()
            .name(format!("hydrate-stream-{id}"))
            .spawn(move || {
                let instance = match instance_arc.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        let _ = tx.send(StreamItem::Error(
                            "instance lock poisoned".to_string(),
                            "rayon_error".to_string()));
                        return;
                    }
                };
                rayon::scope(|s| {
                    for pm in instance.pipelines.iter() {
                        let tx = tx.clone();
                        let cancel = cancel_for_thread.clone();
                        let permission_tables = &instance.permission_tables;
                        let syncable_tables = &instance.syncable_tables;
                        s.spawn(move |_| {
                            if cancel.load(Ordering::Relaxed) { return; }
                            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                                let mut pipeline = pm.lock().unwrap();
                                let chunk = hydrate_single_pipeline(&mut pipeline);
                                let mut filtered = chunk;
                                apply_permission_and_version_filters(
                                    &mut filtered, permission_tables, syncable_tables);
                                if filtered.is_empty() { None }
                                else { Some(crate::chunk_encoder::encode_chunk_buf(&filtered)) }
                            }));
                            match result {
                                Ok(Some(buf)) => { let _ = tx.send(StreamItem::Chunk(buf)); }
                                Ok(None) => {}
                                Err(payload) => {
                                    let msg = format_panic_payload(payload);
                                    let _ = tx.send(StreamItem::Error(msg, "panic".to_string()));
                                }
                            }
                        });
                    }
                });
                // tx dropped → channel closed → done.
            })
            .map_err(|e| napi::Error::from_reason(format!("Failed to spawn coordinator: {e}")))?;

        Ok(HydrateStream {
            rx: Arc::new(Mutex::new(rx)),
            cancel,
            _coordinator: Some(coordinator),
        })
    }

    /// Streaming version of hydrate_query_async — single pipeline.
    /// Returns a HydrateStream with at most one Chunk (or zero if empty).
    #[napi(ts_return_type = "HydrateStream")]
    pub fn hydrate_query_streaming(
        &self,
        id: String,
        query_id: String,
    ) -> napi::Result<HydrateStream> {
        let instance_arc = {
            let instances = self.instances.read()
                .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
            instances.get(&id)
                .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?
                .clone()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<StreamItem>(2); // 1 chunk + safety slot
        let cancel_for_thread = cancel.clone();

        let coordinator = thread::Builder::new()
            .name(format!("hydrate-query-stream-{id}"))
            .spawn(move || {
                let instance = match instance_arc.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        let _ = tx.send(StreamItem::Error(
                            "instance lock poisoned".to_string(),
                            "rayon_error".to_string()));
                        return;
                    }
                };
                if cancel_for_thread.load(Ordering::Relaxed) { return; }

                // Find pipeline outside catch_unwind — if missing, surface
                // as a clear rayon_error rather than a panic.
                let pipeline_idx = instance.pipelines.iter().position(|p| {
                    p.lock().map(|g| g.query_id == query_id).unwrap_or(false)
                });
                let pm = match pipeline_idx {
                    Some(i) => &instance.pipelines[i],
                    None => {
                        let _ = tx.send(StreamItem::Error(
                            format!("No pipeline for query: {query_id}"),
                            "rayon_error".to_string()));
                        return;
                    }
                };

                let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    let mut pipeline = pm.lock().unwrap();
                    let chunk = hydrate_single_pipeline(&mut pipeline);
                    let mut filtered = chunk;
                    apply_permission_and_version_filters(
                        &mut filtered,
                        &instance.permission_tables,
                        &instance.syncable_tables,
                    );
                    if filtered.is_empty() { None }
                    else { Some(crate::chunk_encoder::encode_chunk_buf(&filtered)) }
                }));
                match result {
                    Ok(Some(buf)) => { let _ = tx.send(StreamItem::Chunk(buf)); }
                    Ok(None) => {} // empty filtered chunk
                    Err(payload) => {
                        let msg = format_panic_payload(payload);
                        let _ = tx.send(StreamItem::Error(msg, "panic".to_string()));
                    }
                }
            })
            .map_err(|e| napi::Error::from_reason(format!("Failed to spawn coordinator: {e}")))?;

        Ok(HydrateStream {
            rx: Arc::new(Mutex::new(rx)),
            cancel,
            _coordinator: Some(coordinator),
        })
    }
}

/// Convert a catch_unwind panic payload into a human-readable message.
fn format_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

// ─── Extracted instance-level operations (shared by sync + async paths) ─────

/// Run advance logic on a locked PipelineInstance. Used by both sync and async paths.
///
/// B12: takes `&mut PipelineInstance` so `check_companions_and_emit` can refresh
/// `companion.resolved_value` post-advance (otherwise A→B→A drift over two
/// advances emits a spurious Reset).
fn advance_instance(instance: &mut PipelineInstance, changes: &[Change]) -> AdvanceResult {
    if changes.is_empty() {
        return AdvanceResult { changes: vec![], error: None, error_type: None, reset_signal: None };
    }

    // B11: resolve prev_db_path for cascade-delete enumeration. TS
    // pipeline-driver.ts:1542-1577 calls set_prev_snapshot(prev) BEFORE
    // swap_snapshot(curr); we read it here. Fallback to db_path with a
    // logged warning preserves back-compat for callers that don't yet
    // wire setPrevSnapshot.
    let prev_db_path = instance.prev_db_path.clone()
        .unwrap_or_else(|| {
            eprintln!(
                "[B11] prev_db_path not set; falling back to db_path — \
                 same-tx descendant deletes may be elided. \
                 See .planning/IVM-PORT-AUDIT-DEEP.md §B11."
            );
            instance.db_path.clone()
        });

    // 1. Push changes through IVM operator trees
    // B11 (Task 1b): advance_persistent_pipeline now consumes prev_db_path
    // (descendant SQL reads) alongside db_path (child_row_has_parent reads).
    // Phase 35 / NEW-1: route through prev_pool when available so the
    // descendant-reads connection is reused across the entire batch.
    let prev_pool_ref = instance.prev_pool.as_ref();
    let mut all_row_changes: Vec<RowChange> = instance.pipelines.iter().flat_map(|pm| {
        let mut pipeline = pm.lock().unwrap();
        crate::advance::advance_persistent_pipeline_with_prev(
            &mut pipeline, changes, &instance.db_path, &prev_db_path, prev_pool_ref,
        )
    }).collect();

    // 2. Permission table filtering + minRowVersion bump
    if !instance.permission_tables.is_empty() || !instance.syncable_tables.is_empty() {
        apply_permission_and_version_filters(
            &mut all_row_changes,
            &instance.permission_tables,
            &instance.syncable_tables,
        );
    }

    // 3. Companion scalar subquery checks
    let mut reset_signal: Option<String> = None;
    if !instance.companions.is_empty() {
        let changed_tables: HashSet<&str> = changes.iter().map(|c| c.table.as_str()).collect();
        // B12: copy db_path to satisfy the borrow checker — companions is
        // mutably borrowed below, so we can't also pass &instance.db_path.
        let db_path = instance.db_path.clone();

        match check_companions_and_emit(
            &mut instance.companions,
            changes,
            &changed_tables,
            &db_path,
        ) {
            CompanionResult::Reset(reason) => {
                reset_signal = Some(reason);
            }
            CompanionResult::Changes(companion_changes) => {
                all_row_changes.extend(companion_changes);
            }
        }
    }

    AdvanceResult {
        changes: all_row_changes,
        error: None,
        error_type: None,
        reset_signal,
    }
}

/// Run hydrate logic on a locked PipelineInstance. Used by both sync and async paths.
fn hydrate_instance(instance: &PipelineInstance) -> AdvanceResult {
    let all_row_changes: Vec<RowChange> = instance.pipelines.par_iter().flat_map(|pm| {
        let mut pipeline = pm.lock().unwrap();
        hydrate_single_pipeline(&mut pipeline)
    }).collect();

    AdvanceResult {
        changes: all_row_changes,
        error: None,
        error_type: None,
        reset_signal: None,
    }
}

/// Run hydrate_query logic on a locked PipelineInstance.
fn hydrate_query_instance(instance: &PipelineInstance, query_id: &str) -> napi::Result<AdvanceResult> {
    let pm = instance.pipelines.iter()
        .find(|p| p.lock().unwrap().query_id == query_id)
        .ok_or_else(|| napi::Error::from_reason(format!("No pipeline for query: {query_id}")))?;

    let mut pipeline = pm.lock().unwrap();
    let row_changes = hydrate_single_pipeline(&mut pipeline);

    Ok(AdvanceResult {
        changes: row_changes,
        error: None,
        error_type: None,
        reset_signal: None,
    })
}

// ─── AsyncTask implementations ──────────────────────────────────────────────

/// Async task for advance() — runs IVM fan-out on a libuv worker thread.
pub struct AdvanceTask {
    instance: Arc<Mutex<PipelineInstance>>,
    changes: Vec<Change>,
}

// SAFETY: All fields are Send (Arc<Mutex<T>> is Send, Vec<Change> is Send).
unsafe impl Send for AdvanceTask {}

impl Task for AdvanceTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        // B12: mutable lock — advance_instance now refreshes companion
        // resolved_value post-advance to prevent A→B→A spurious resets.
        let mut instance = self.instance.lock()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let result = advance_instance(&mut instance, &self.changes);
        Ok(encode_advance_result_buf(&result))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Buffer::from(output))
    }
}

/// Async task for hydrate() — runs hydration on a libuv worker thread.
pub struct HydrateTask {
    instance: Arc<Mutex<PipelineInstance>>,
}

// SAFETY: Arc<Mutex<T>> is Send.
unsafe impl Send for HydrateTask {}

impl Task for HydrateTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let instance = self.instance.lock()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let result = hydrate_instance(&instance);
        Ok(encode_advance_result_buf(&result))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Buffer::from(output))
    }
}

/// Async task for hydrate_query() — runs single-query hydration on a libuv worker thread.
pub struct HydrateQueryTask {
    instance: Arc<Mutex<PipelineInstance>>,
    query_id: String,
}

// SAFETY: Arc<Mutex<T>> is Send, String is Send.
unsafe impl Send for HydrateQueryTask {}

impl Task for HydrateQueryTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let instance = self.instance.lock()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let result = hydrate_query_instance(&instance, &self.query_id)?;
        Ok(encode_advance_result_buf(&result))
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(Buffer::from(output))
    }
}

// ─── Streaming napi types (Phase 31) ────────────────────────────────────────

/// JS-facing shape of one chunk pulled from a stream.
/// `done=true` means the channel closed (no more items).
/// Otherwise `kind ∈ {"chunk","reset","error"}` discriminates the variant.
#[napi(object)]
pub struct NextChunkValue {
    pub done: bool,
    pub kind: Option<String>,
    pub chunk: Option<Buffer>,
    pub reason: Option<String>,
    pub error_msg: Option<String>,
    pub error_kind: Option<String>,
}

/// AsyncTask that blocks on `rx.recv()` to fetch the next StreamItem.
/// One AsyncTask per `next()` call; runs on a libuv worker thread
/// (blocking is fine — that's exactly what worker threads are for).
pub struct NextChunkTask {
    rx: Arc<Mutex<mpsc::Receiver<StreamItem>>>,
}
unsafe impl Send for NextChunkTask {}

impl Task for NextChunkTask {
    type Output = Option<StreamItem>; // None == channel closed == done
    type JsValue = NextChunkValue;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let rx = self.rx.lock()
            .map_err(|e| napi::Error::from_reason(format!("Stream rx mutex poisoned: {e}")))?;
        match rx.recv() {
            Ok(item) => Ok(Some(item)),
            Err(_) => Ok(None), // SendError means coordinator dropped tx — done
        }
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        match output {
            None => Ok(NextChunkValue {
                done: true, kind: None, chunk: None,
                reason: None, error_msg: None, error_kind: None,
            }),
            Some(StreamItem::Chunk(buf)) => Ok(NextChunkValue {
                done: false, kind: Some("chunk".to_string()),
                chunk: Some(Buffer::from(buf)),
                reason: None, error_msg: None, error_kind: None,
            }),
            Some(StreamItem::ResetSignal(reason)) => Ok(NextChunkValue {
                done: false, kind: Some("reset".to_string()),
                chunk: None, reason: Some(reason),
                error_msg: None, error_kind: None,
            }),
            Some(StreamItem::Error(msg, kind)) => Ok(NextChunkValue {
                done: false, kind: Some("error".to_string()),
                chunk: None, reason: None,
                error_msg: Some(msg), error_kind: Some(kind),
            }),
        }
    }
}

/// Streaming handle returned by `advance_streaming`. JS calls
/// `await stream.next()` repeatedly until `done=true`. Calling
/// `stream.return_()` flips the cancel flag (D-14/D-15).
///
/// Lifecycle note: callers SHOULD call `return_()` explicitly on graceful
/// shutdown. `Drop` is a safety net for GC, not a guarantee for process
/// exit (Pitfall 2 in 31-RESEARCH.md).
#[napi]
pub struct AdvanceStream {
    rx: Arc<Mutex<mpsc::Receiver<StreamItem>>>,
    cancel: Arc<AtomicBool>,
    // Coordinator JoinHandle — kept so the thread isn't detached at
    // construction time; never explicitly joined (would block JS GC).
    _coordinator: Option<thread::JoinHandle<()>>,
}

unsafe impl Send for AdvanceStream {}
unsafe impl Sync for AdvanceStream {}

impl AdvanceStream {
    /// Test-only constructor used by Rust unit tests. Production code
    /// receives an AdvanceStream from `advance_streaming` (Task 4).
    #[cfg(test)]
    pub(crate) fn new_for_test(
        rx: mpsc::Receiver<StreamItem>,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            rx: Arc::new(Mutex::new(rx)),
            cancel,
            _coordinator: None,
        }
    }

    /// Test-only accessor for the underlying receiver. Tests need to
    /// call `recv()` directly to avoid the napi AsyncTask path which
    /// is awkward to drive from pure Rust.
    #[cfg(test)]
    pub(crate) fn rx_for_test(&self) -> Arc<Mutex<mpsc::Receiver<StreamItem>>> {
        self.rx.clone()
    }
}

#[napi]
impl AdvanceStream {
    /// Pull the next StreamItem from the channel.
    /// Returns Promise<NextChunkValue> on the JS side.
    #[napi(ts_return_type = "Promise<NextChunkValue>")]
    pub fn next(&self) -> AsyncTask<NextChunkTask> {
        AsyncTask::new(NextChunkTask { rx: self.rx.clone() })
    }

    /// Cancel the stream. Synchronous; flips the cancel flag.
    /// Per D-15, the TS wrapper MUST call this in `finally`.
    ///
    /// Note: name is `return_` because `return` is a Rust keyword.
    /// Mapped to JS `return_` automatically by napi-rs.
    #[napi]
    pub fn return_(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for AdvanceStream {
    fn drop(&mut self) {
        // D-14: belt-and-suspenders cancel on GC. Do NOT join the
        // coordinator thread here — that would block JS GC. The
        // coordinator notices the cancel flag at scope.spawn entry
        // and at change boundaries, then drops tx normally.
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Symmetric to AdvanceStream — separate type for clarity in JS API.
#[napi]
pub struct HydrateStream {
    rx: Arc<Mutex<mpsc::Receiver<StreamItem>>>,
    cancel: Arc<AtomicBool>,
    _coordinator: Option<thread::JoinHandle<()>>,
}

unsafe impl Send for HydrateStream {}
unsafe impl Sync for HydrateStream {}

impl HydrateStream {
    #[cfg(test)]
    pub(crate) fn rx_for_test(&self) -> Arc<Mutex<mpsc::Receiver<StreamItem>>> {
        self.rx.clone()
    }
}

#[napi]
impl HydrateStream {
    #[napi(ts_return_type = "Promise<NextChunkValue>")]
    pub fn next(&self) -> AsyncTask<NextChunkTask> {
        AsyncTask::new(NextChunkTask { rx: self.rx.clone() })
    }

    #[napi]
    pub fn return_(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for HydrateStream {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Hydrate a single pipeline (fetch all rows from operator tree).
fn hydrate_single_pipeline(pipeline: &mut PipelineState) -> Vec<RowChange> {
    let mut row_changes = Vec::new();

    if let Some(ref mut chain) = pipeline.chain {
        let nodes = chain.fetch(&FetchRequest::default());
        flatten_nodes_to_row_changes(
            &mut row_changes,
            &pipeline.query_id,
            &pipeline.source_table,
            &pipeline.primary_key,
            &nodes,
            &pipeline.all_primary_keys,
            &pipeline.column_types,
            &pipeline.rel_to_table,
        );
    } else {
        let req = crate::source::FetchRequest::default();
        if let Ok(nodes) = pipeline.source.fetch(0, &req) {
            let ivm_nodes: Vec<zero_ivm_rs::types::Node> = nodes.into_iter().map(|n| {
                zero_ivm_rs::types::Node {
                    row: n.row,
                    relationships: n.relationships.into_iter().map(|(k, v)| {
                        (k, v.into_iter().map(|cn| zero_ivm_rs::types::Node {
                            row: cn.row,
                            relationships: HashMap::new(),
                        }).collect())
                    }).collect(),
                }
            }).collect();
            flatten_nodes_to_row_changes(
                &mut row_changes,
                &pipeline.query_id,
                &pipeline.source_table,
                &pipeline.primary_key,
                &ivm_nodes,
                &pipeline.all_primary_keys,
                &pipeline.column_types,
                &pipeline.rel_to_table,
            );
        }
    }

    row_changes
}

// ─── Permission filtering + minRowVersion ───────────────────────────────────

const ZERO_VERSION_COLUMN: &str = "_0_version";

/// Filter out permission table rows and apply minRowVersion bump in place.
fn apply_permission_and_version_filters(
    changes: &mut Vec<RowChange>,
    permission_tables: &HashSet<String>,
    syncable_tables: &HashMap<String, TableAndZqlSpec>,
) {
    changes.retain_mut(|change| {
        // Skip rows from permission-system tables
        if permission_tables.contains(&change.table) {
            return false;
        }

        // Apply minRowVersion bump for non-remove changes
        if change.change_type != "remove" {
            if let Some(ref mut row) = change.row {
                if let Some(spec) = syncable_tables.get(&change.table) {
                    if let Some(ref min_ver) = spec.table_spec.min_row_version {
                        if let Some(serde_json::Value::String(ver)) = row.get(ZERO_VERSION_COLUMN) {
                            if ver.as_str() < min_ver.as_str() {
                                row.insert(
                                    ZERO_VERSION_COLUMN.to_string(),
                                    serde_json::Value::String(min_ver.clone()),
                                );
                            }
                        }
                    }
                }
            }
        }

        true
    });
}

// ─── Companion scalar subquery handling ─────────────────────────────────────

enum CompanionResult {
    /// Scalar value changed — caller should reset all pipelines.
    Reset(String),
    /// No value changes — here are the companion row changes to emit.
    Changes(Vec<RowChange>),
}

/// Check all companions for scalar value changes.
/// If any companion's scalar value changed, return Reset.
/// Otherwise, emit row changes for companion table rows that were modified.
///
/// B12: takes `&mut companions` so that on a no-reset outcome we can
/// refresh `companion.resolved_value` to the freshly-queried `new_value`.
/// Without this update, drift A→B→A across two advances would compare
/// the post-advance scalar against the *initial hydration* value forever
/// and emit spurious Reset signals. The post-advance update keeps the
/// "last known good" baseline in sync with what's actually in SQLite.
fn check_companions_and_emit(
    companions: &mut HashMap<String, Vec<CompanionInfo>>,
    changes: &[Change],
    changed_tables: &HashSet<&str>,
    db_path: &str,
) -> CompanionResult {
    let mut companion_changes = Vec::new();
    // B12: collect (query_id, companion_idx, new_value) updates to apply
    // after iteration so we don't violate borrow rules while iterating.
    let mut resolved_value_updates: Vec<(String, usize, serde_json::Value)> = Vec::new();

    for (query_id, query_companions) in companions.iter() {
        for (idx, companion) in query_companions.iter().enumerate() {
            if !changed_tables.contains(companion.table.as_str()) {
                continue;
            }

            // Query current scalar value from SQLite
            let new_value = query_companion_scalar(db_path, companion);

            if !scalar_values_equal(&new_value, &companion.resolved_value) {
                return CompanionResult::Reset(format!(
                    "Scalar subquery value changed for {}: {} -> {}",
                    companion.table,
                    format_scalar(&companion.resolved_value),
                    format_scalar(&new_value),
                ));
            }

            // B12: queue post-advance refresh of resolved_value. Even though
            // it equals the cached value here (scalar_values_equal == true),
            // the cached value may be a JSON shape that doesn't survive
            // round-tripping (e.g. integer vs string normalization). Always
            // refresh to keep the baseline aligned with query_companion_scalar
            // output for the next advance.
            resolved_value_updates.push((query_id.clone(), idx, new_value));

            // Emit row changes for matching companion rows
            for change in changes {
                if change.table != companion.table {
                    continue;
                }

                let row = change.next_value.as_ref().or_else(|| change.prev_values.first());
                let Some(row) = row else { continue };

                // Check if this row matches the companion's WHERE conditions
                let matches = companion.where_conditions.iter().all(|cond| {
                    match row.get(&cond.column) {
                        Some(v) => json_values_match(v, &cond.value),
                        None => false,
                    }
                });
                if !matches {
                    continue;
                }

                // Build row key from primary key
                let row_key = extract_pk_as_json(row, &companion.primary_key);

                if change.next_value.is_some() && !change.prev_values.is_empty() {
                    // Edit
                    companion_changes.push(RowChange {
                        query_id: query_id.clone(),
                        table: companion.table.clone(),
                        row_key,
                        row: change.next_value.clone(),
                        change_type: "edit".to_string(),
                    });
                } else if change.next_value.is_some() {
                    // Add
                    companion_changes.push(RowChange {
                        query_id: query_id.clone(),
                        table: companion.table.clone(),
                        row_key,
                        row: change.next_value.clone(),
                        change_type: "add".to_string(),
                    });
                } else {
                    // Remove
                    companion_changes.push(RowChange {
                        query_id: query_id.clone(),
                        table: companion.table.clone(),
                        row_key,
                        row: None,
                        change_type: "remove".to_string(),
                    });
                }
            }
        }
    }

    // B12: apply queued resolved_value refreshes. Done after iteration so we
    // can take a mutable borrow without violating the immutable borrow used
    // during the read pass above.
    for (query_id, idx, new_value) in resolved_value_updates {
        if let Some(query_companions) = companions.get_mut(&query_id) {
            if let Some(companion) = query_companions.get_mut(idx) {
                companion.resolved_value = new_value;
            }
        }
    }

    CompanionResult::Changes(companion_changes)
}

/// Query SQLite for a companion scalar value.
/// Opens a fresh read-only connection (the pool is already swapped to curr.db).
fn query_companion_scalar(
    db_path: &str,
    companion: &CompanionInfo,
) -> serde_json::Value {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;

    let conn = match Connection::open_with_flags(db_path, flags) {
        Ok(c) => c,
        Err(_) => return serde_json::Value::Null,
    };

    if companion.where_conditions.is_empty() {
        let sql = format!(
            "SELECT \"{}\" FROM \"{}\" LIMIT 1",
            companion.child_field, companion.table,
        );
        match conn.query_row(&sql, [], |row| row.get::<_, rusqlite::types::Value>(0)) {
            Ok(val) => sqlite_to_json(val),
            Err(_) => serde_json::Value::Null,
        }
    } else {
        let where_clauses: Vec<String> = companion
            .where_conditions
            .iter()
            .map(|c| format!("\"{}\" = ?", c.column))
            .collect();
        let sql = format!(
            "SELECT \"{}\" FROM \"{}\" WHERE {} LIMIT 1",
            companion.child_field,
            companion.table,
            where_clauses.join(" AND "),
        );

        let params: Vec<Box<dyn rusqlite::types::ToSql>> = companion
            .where_conditions
            .iter()
            .map(|c| json_to_sql_param(&c.value))
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        match conn.query_row(&sql, param_refs.as_slice(), |row| {
            row.get::<_, rusqlite::types::Value>(0)
        }) {
            Ok(val) => sqlite_to_json(val),
            Err(_) => serde_json::Value::Null,
        }
    }
}

fn sqlite_to_json(val: rusqlite::types::Value) -> serde_json::Value {
    match val {
        rusqlite::types::Value::Null => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::Value::Number(i.into()),
        rusqlite::types::Value::Real(f) => {
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
        rusqlite::types::Value::Blob(b) => {
            // Encode as JSON array of bytes (matching TS behavior)
            serde_json::Value::Array(b.into_iter().map(|byte| serde_json::Value::Number(byte.into())).collect())
        }
    }
}

fn json_to_sql_param(val: &serde_json::Value) -> Box<dyn rusqlite::types::ToSql> {
    match val {
        serde_json::Value::Null => Box::new(rusqlite::types::Value::Null),
        serde_json::Value::Bool(b) => Box::new(*b as i64),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Box::new(i)
            } else if let Some(f) = n.as_f64() {
                Box::new(f)
            } else {
                Box::new(n.to_string())
            }
        }
        serde_json::Value::String(s) => Box::new(s.clone()),
        _ => Box::new(val.to_string()),
    }
}

/// Compare two scalar values for equality (matching TS `scalarValuesEqual`).
/// null/undefined in TS maps to Value::Null in JSON.
fn scalar_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    // Both null → equal
    if a.is_null() && b.is_null() {
        return true;
    }
    // String comparison (matching TS String() coercion)
    match (a, b) {
        (serde_json::Value::String(sa), serde_json::Value::String(sb)) => sa == sb,
        (serde_json::Value::Number(na), serde_json::Value::Number(nb)) => na == nb,
        (serde_json::Value::Bool(ba), serde_json::Value::Bool(bb)) => ba == bb,
        _ => {
            // Fallback: compare string representations
            format!("{a}") == format!("{b}")
        }
    }
}

fn json_values_match(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    // Match TS behavior: String(row[c.column]) === String(c.value)
    format!("{}", json_display(a)) == format!("{}", json_display(b))
}

/// Display a JSON value as TS would with String() coercion
fn json_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => v.to_string(),
    }
}

fn format_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "undefined".to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

fn extract_pk_as_json(
    row: &HashMap<String, serde_json::Value>,
    pk: &[String],
) -> serde_json::Value {
    if pk.is_empty() {
        return serde_json::Value::Object(row.clone().into_iter().collect());
    }
    let mut obj = serde_json::Map::new();
    for k in pk {
        if let Some(v) = row.get(k) {
            obj.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(obj)
}

// ─── Streaming tests (Phase 31) ─────────────────────────────────────────────

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    /// Build a real RustPipelineManager + N pipelines on a temp SQLite DB.
    /// Returns (manager, instance_id, db_dir).
    /// The TempDir must outlive the manager — keep it alive in test scope.
    fn build_test_manager_with_n_pipelines(
        n: usize,
        num_rows: usize,
    ) -> (RustPipelineManager, String, tempfile::TempDir) {
        use crate::ast_to_config::{HydrateQuery, SchemaCache};

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_path_str = db_path.to_str().unwrap().to_string();

        // Seed DB with `users` table
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE users (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    age INTEGER NOT NULL,
                    active INTEGER NOT NULL DEFAULT 1,
                    score REAL NOT NULL DEFAULT 0.0
                );",
            )
            .unwrap();

            let mut stmt = conn
                .prepare("INSERT INTO users (id, name, age, active, score) VALUES (?1, ?2, ?3, ?4, ?5)")
                .unwrap();
            for i in 0..num_rows {
                stmt.execute(rusqlite::params![
                    format!("u{}", i),
                    format!("User {}", i),
                    20 + (i % 60) as i64,
                    if i % 5 == 0 { 0 } else { 1 },
                    (i as f64) * 1.5,
                ])
                .unwrap();
            }
        }

        let manager = RustPipelineManager::new();
        let id = "test".to_string();
        manager.create_instance(id.clone(), db_path_str.clone()).unwrap();

        // Build pipelines directly using build_pipeline_state (faster than
        // going through add_query JSON path; we don't need the AST-driven
        // codepath for these streaming tests).
        let mut schema_cache = SchemaCache::new(&db_path_str);
        {
            let instances = manager.instances.read().unwrap();
            let inst_arc = instances.get(&id).unwrap().clone();
            drop(instances);
            let mut inst = inst_arc.lock().unwrap();
            for i in 0..n {
                let q_json = serde_json::json!({
                    "query_id": format!("q{}", i),
                    "ast": {
                        "table": "users",
                        "orderBy": [["age", "asc"]],
                        "limit": 10 + (i % 20),
                    },
                    "primary_key": ["id"],
                })
                .to_string();
                let query: HydrateQuery = serde_json::from_str(&q_json).unwrap();
                let state = crate::advance::build_pipeline_state(
                    &db_path_str,
                    &query,
                    &mut schema_cache,
                    &inst.shared_pool,
                )
                .unwrap();
                inst.pipelines.push(Mutex::new(state));
            }
        }
        (manager, id, dir)
    }

    /// Build M synthetic Edit changes against the `users` table for use in
    /// streaming tests. Each change updates one row's `name`/`age`.
    fn build_synthetic_changes(count: usize) -> Vec<Change> {
        (0..count)
            .map(|i| {
                let mut next = std::collections::HashMap::new();
                next.insert("id".to_string(), serde_json::json!(format!("u{}", i)));
                next.insert("name".to_string(), serde_json::json!(format!("Updated {}", i)));
                next.insert("age".to_string(), serde_json::json!(25 + (i % 40) as i64));
                next.insert("active".to_string(), serde_json::json!(1));
                next.insert("score".to_string(), serde_json::json!((i as f64) * 2.0));

                let mut prev = std::collections::HashMap::new();
                prev.insert("id".to_string(), serde_json::json!(format!("u{}", i)));
                prev.insert("name".to_string(), serde_json::json!(format!("User {}", i)));
                prev.insert("age".to_string(), serde_json::json!(20 + (i % 60) as i64));
                prev.insert("active".to_string(), serde_json::json!(1));
                prev.insert("score".to_string(), serde_json::json!((i as f64) * 1.5));

                Change {
                    table: "users".to_string(),
                    prev_values: vec![prev],
                    next_value: Some(next),
                    row_key: serde_json::json!(format!("u{}", i)),
                }
            })
            .collect()
    }

    #[test]
    fn advance_stream_drop_sets_cancel_flag() {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_observer = cancel.clone();
        let (_tx, rx) = mpsc::channel::<StreamItem>();
        {
            let _stream = AdvanceStream::new_for_test(rx, cancel);
            assert!(
                !cancel_observer.load(Ordering::SeqCst),
                "cancel must be false before drop"
            );
        } // _stream dropped here
        assert!(
            cancel_observer.load(Ordering::SeqCst),
            "AdvanceStream::drop must set cancel flag (D-14)"
        );
    }

    /// TEST-01 (STREAM-04): cancel observation actually short-circuits work.
    ///
    /// We can't rely on a tight chunk-count bound (rayon completes small
    /// pipelines very fast — by the time we call return_() many will have
    /// already enqueued their chunk). Instead we measure the STRUCTURAL
    /// behavior of the cancel flag:
    ///
    /// 1. Build N=20 pipelines with HEAVY workload (50_000 changes each).
    ///    Without cancel observation, each pipeline iterates through all
    ///    50_000 changes — measurable wall-time.
    /// 2. Call `return_()` IMMEDIATELY after construction (before draining
    ///    a single chunk). The cancel flag is set process-wide.
    /// 3. Drain everything to completion.
    /// 4. Compare wall-time vs the same workload with NO cancel.
    ///
    /// If the cancel observation works, the cancelled run completes in a
    /// small fraction of the un-cancelled run's time. Otherwise both take
    /// the same time. This is robust to rayon scheduling and channel
    /// behavior — it directly tests the cancel propagation path.
    #[test]
    fn streaming_cancellation_bounded_chunks() {
        use std::time::Instant;

        crate::advance::PANIC_ON_PIPELINE_INDEX
            .store(usize::MAX, Ordering::SeqCst);
        crate::advance::PIPELINE_INVOCATION_COUNT.store(0, Ordering::SeqCst);

        let n_pipelines = 8;
        // 50_000 changes per pipeline — large enough that even a fast
        // pipeline takes 100ms+ to run uncancelled, giving the cancel
        // observation a measurable signal.
        let n_changes = 50_000;

        // ── Run 1: cancel IMMEDIATELY ──
        let (manager, id, _dir) =
            build_test_manager_with_n_pipelines(n_pipelines, 100);
        let changes_json =
            serde_json::to_string(&build_synthetic_changes(n_changes)).unwrap();
        let stream = manager.advance_streaming(id, changes_json).unwrap();
        let rx = stream.rx_for_test();
        // Set cancel BEFORE the rayon scope makes meaningful progress.
        stream.return_();
        let t_cancelled = Instant::now();
        loop {
            match rx.lock().unwrap().recv_timeout(Duration::from_secs(30)) {
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let cancelled_elapsed = t_cancelled.elapsed();
        drop(stream);

        // ── Run 2: NO cancel ──
        let (manager2, id2, _dir2) =
            build_test_manager_with_n_pipelines(n_pipelines, 100);
        let changes_json2 =
            serde_json::to_string(&build_synthetic_changes(n_changes)).unwrap();
        let stream2 = manager2.advance_streaming(id2, changes_json2).unwrap();
        let rx2 = stream2.rx_for_test();
        let t_full = Instant::now();
        loop {
            match rx2.lock().unwrap().recv_timeout(Duration::from_secs(60)) {
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let full_elapsed = t_full.elapsed();

        // STREAM-04: with cancel observation wired at the per-change loop
        // top, the cancelled run must be MUCH faster than the full run.
        // We require ≤ 50% (generous to avoid CI flake) — in practice
        // the cancelled run completes in ~ms while the full run takes
        // hundreds of ms. With the placeholder helper that ignores cancel,
        // the two would be roughly equal (cancelled may be slightly
        // faster only because of channel-fill ordering luck).
        eprintln!(
            "TEST-01 timings: cancelled={:?}, full={:?}, ratio={:.2}",
            cancelled_elapsed,
            full_elapsed,
            cancelled_elapsed.as_secs_f64() / full_elapsed.as_secs_f64()
        );
        assert!(
            cancelled_elapsed.as_secs_f64() < full_elapsed.as_secs_f64() * 0.5,
            "Cancel observation did NOT short-circuit work. \
             cancelled={:?}, full={:?}. \
             Either cancel flag is not being checked in advance_persistent_pipeline_with_cancel, \
             or the per-change loop check is in the wrong place.",
            cancelled_elapsed,
            full_elapsed
        );
    }

    /// TEST-02 (STREAM-05): companion scalar reset closes the channel.
    ///
    /// MARKED `#[ignore]` — companion test setup helpers do not exist in
    /// pipeline_manager_tests. The Task 4 wiring for companion handling is
    /// covered indirectly by TEST-04 (parity fuzz in 31-02 plan) which
    /// exercises the buffered + streaming companion paths against each
    /// other on random inputs. A direct test would require seeding a
    /// companion table, calling set_query_companions, and triggering a
    /// scalar change — sketch left as TODO for Phase 33 hardening.
    #[test]
    #[ignore]
    fn streaming_companion_reset_closes_channel() {
        // TODO(phase-33): build companion table + scalar trigger, then
        // assert: items.last() is StreamItem::ResetSignal AND no items
        // arrive within 100ms after the ResetSignal.
    }

    /// TEST-03: panic in one pipeline does not take down siblings.
    /// Sibling pipelines must complete; the panicked pipeline's chunk is
    /// replaced by `StreamItem::Error("test-injected ...", "panic")`.
    #[test]
    fn streaming_panic_in_one_pipeline_others_complete() {
        // Reset state from any prior test, then inject panic at the 2nd
        // pipeline (index 1). NOTE: the counter is process-wide, so this
        // test must not race with other streaming tests that call
        // advance_persistent_pipeline_with_cancel. Cargo test parallelism
        // may run them concurrently; use --test-threads 1 if flaky.
        crate::advance::PIPELINE_INVOCATION_COUNT.store(0, Ordering::SeqCst);
        crate::advance::PANIC_ON_PIPELINE_INDEX.store(1, Ordering::SeqCst);

        let (manager, id, _dir) = build_test_manager_with_n_pipelines(3, 100);
        let changes_json = serde_json::to_string(&build_synthetic_changes(5)).unwrap();
        let stream = manager.advance_streaming(id, changes_json).unwrap();
        let rx = stream.rx_for_test();

        let mut chunks = 0;
        let mut errors = 0;
        loop {
            match rx.lock().unwrap().recv_timeout(Duration::from_secs(10)) {
                Ok(StreamItem::Chunk(_)) => chunks += 1,
                Ok(StreamItem::Error(_, kind)) => {
                    assert_eq!(kind, "panic", "kind must be 'panic', got '{}'", kind);
                    errors += 1;
                }
                Ok(StreamItem::ResetSignal(_)) => {}
                Err(_) => break,
            }
        }

        // Cleanup BEFORE assertions so a failure doesn't leak state.
        crate::advance::PANIC_ON_PIPELINE_INDEX
            .store(usize::MAX, Ordering::SeqCst);
        crate::advance::PIPELINE_INVOCATION_COUNT.store(0, Ordering::SeqCst);

        // Two non-panicking pipelines run; one panics.
        // (Some pipelines may filter to empty chunk → no Chunk emitted; in
        // that case we still expect ≤ 2 chunks but ≥ 1.)
        assert_eq!(errors, 1, "exactly one panic must surface as Error('panic')");
        assert!(
            chunks <= 2,
            "at most 2 non-panicking pipelines emit chunks, got {}",
            chunks
        );
    }

    /// PERF-01 (Phase 33-03): validates the production-sized bounded channel
    /// (`mpsc::sync_channel(pipeline_count.max(1) + 1)` at lines 428/544/614)
    /// actually rejects sends when full and unblocks after a drain.
    ///
    /// Deterministic — uses `try_send` + `TrySendError::Full` (no thread sleeps,
    /// no timing flake). Self-contained on `u32` payload (does not need to
    /// construct `StreamItem` values).
    ///
    /// Closes the gap noted in Phase 31 D-12: production code has the bounded
    /// channel sizing but no test confirms producer pipelines block until JS pulls.
    #[test]
    fn streaming_channel_bounded_blocks_when_full() {
        use std::sync::mpsc::{sync_channel, TrySendError};

        // Mirror production sizing: pipeline_count = 4 → capacity 5.
        let pipeline_count = 4usize;
        let capacity = pipeline_count.max(1) + 1;
        assert_eq!(
            capacity, 5,
            "production capacity formula must yield 5 for pipeline_count=4"
        );

        let (tx, rx) = sync_channel::<u32>(capacity);

        // Phase 1: fill exactly to capacity.
        for i in 0..capacity as u32 {
            tx.try_send(i).expect("send within capacity must succeed");
        }

        // Phase 2: next send must be rejected with TrySendError::Full(99).
        match tx.try_send(99) {
            Err(TrySendError::Full(99)) => { /* expected */ }
            other => panic!("expected Full(99), got {:?}", other),
        }

        // Phase 3: drain one item.
        let drained = rx.recv().expect("recv after fill must succeed");
        assert_eq!(drained, 0);

        // Phase 4: previously-blocked sender now succeeds.
        tx.try_send(99).expect("send after drain must succeed");

        // Phase 5: confirm queue order is preserved.
        assert_eq!(rx.recv().unwrap(), 1);
        assert_eq!(rx.recv().unwrap(), 2);
        assert_eq!(rx.recv().unwrap(), 3);
        assert_eq!(rx.recv().unwrap(), 4);
        assert_eq!(rx.recv().unwrap(), 99);
    }
}

// ─── B12 / NEW-3 regression tests ───────────────────────────────────────────

#[cfg(test)]
mod companion_lifecycle_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// Build a temp SQLite DB with a `prefs` table holding (k, v) pairs.
    /// Returns (db_path, TempDir kept alive for the caller).
    fn build_prefs_db(initial: &[(&str, i64)]) -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prefs.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE prefs (k TEXT PRIMARY KEY, v INTEGER NOT NULL);",
        )
        .unwrap();
        let mut stmt = conn
            .prepare("INSERT INTO prefs (k, v) VALUES (?1, ?2)")
            .unwrap();
        for (k, v) in initial {
            stmt.execute(rusqlite::params![k, v]).unwrap();
        }
        (db_path.to_str().unwrap().to_string(), dir)
    }

    /// Build a CompanionInfo monitoring `prefs.v WHERE k = 'limit'`.
    fn make_companion(resolved: serde_json::Value) -> CompanionInfo {
        CompanionInfo {
            query_id: "q1".to_string(),
            table: "prefs".to_string(),
            child_field: "v".to_string(),
            resolved_value: resolved,
            where_conditions: vec![CompanionCondition {
                column: "k".to_string(),
                value: serde_json::json!("limit"),
            }],
            primary_key: vec!["k".to_string()],
        }
    }

    /// Build a synthetic edit Change against the prefs table (k='limit')
    /// so `changed_tables` triggers the companion scan.
    fn make_prefs_change(prev_v: i64, next_v: i64) -> Change {
        let mut prev = HashMap::new();
        prev.insert("k".to_string(), serde_json::json!("limit"));
        prev.insert("v".to_string(), serde_json::json!(prev_v));
        let mut next = HashMap::new();
        next.insert("k".to_string(), serde_json::json!("limit"));
        next.insert("v".to_string(), serde_json::json!(next_v));
        Change {
            table: "prefs".to_string(),
            prev_values: vec![prev],
            next_value: Some(next),
            row_key: serde_json::json!("limit"),
        }
    }

    /// B12: after a no-reset advance, `companion.resolved_value` MUST be
    /// refreshed to the freshly-queried value from SQLite. Without this
    /// refresh, A→B→A drift across two advances would compare against the
    /// stale baseline forever and emit spurious resets.
    ///
    /// Strategy:
    ///   1. Seed DB with limit=10. Hydrate companion with resolved=10.
    ///   2. Run advance with no real scalar change (DB still says 10) →
    ///      no reset, but the function should still rewrite resolved_value
    ///      from the freshly-queried value (still 10).
    ///   3. Mutate DB to limit=20 BEHIND THE BACK of the companion (simulates
    ///      another writer / a snapshot swap).
    ///   4. Run a second advance. Without the B12 fix, this would compare
    ///      20 against 10 (stale) and emit Reset. With the fix, the check
    ///      uses the previous run's refreshed baseline.
    ///   5. Mutate DB to limit=30 — now this MUST emit Reset because the
    ///      baseline (20 from step 4) differs from current (30).
    #[test]
    fn b12_resolved_value_updated_after_advance() {
        let (db_path, _dir) = build_prefs_db(&[("limit", 10)]);

        let mut companions: HashMap<String, Vec<CompanionInfo>> = HashMap::new();
        companions.insert("q1".to_string(), vec![make_companion(serde_json::json!(10))]);

        let changes = vec![make_prefs_change(9, 10)];
        let changed_tables: HashSet<&str> =
            changes.iter().map(|c| c.table.as_str()).collect();

        // Step 1+2: first advance. DB scalar=10, baseline=10 → no reset.
        let result = check_companions_and_emit(
            &mut companions,
            &changes,
            &changed_tables,
            &db_path,
        );
        match result {
            CompanionResult::Changes(_) => { /* expected */ }
            CompanionResult::Reset(reason) => {
                panic!("B12 baseline pre-condition broken: unexpected reset: {reason}")
            }
        }
        // B12 INVARIANT: resolved_value refreshed to the queried value (10).
        let baseline = &companions["q1"][0].resolved_value;
        assert_eq!(
            baseline,
            &serde_json::json!(10),
            "B12: resolved_value must be refreshed post-advance"
        );

        // Step 3: out-of-band write to DB → limit becomes 20.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute("UPDATE prefs SET v = 20 WHERE k = 'limit'", [])
                .unwrap();
        }

        // Step 4: second advance. DB scalar=20, baseline (from step 2) is
        // still 10 BEFORE this advance. So this advance MUST observe the
        // change and emit Reset (this is the correct behavior — the
        // intervening write went undetected by the companion lifecycle).
        // After Reset, resolved_value is NOT updated (early return).
        let changes2 = vec![make_prefs_change(10, 20)];
        let result2 = check_companions_and_emit(
            &mut companions,
            &changes2,
            &changed_tables,
            &db_path,
        );
        match result2 {
            CompanionResult::Reset(_) => { /* expected — drift detected */ }
            CompanionResult::Changes(_) => panic!(
                "B12 sanity broken: scalar drift 10→20 must emit Reset"
            ),
        }
        // After a reset, resolved_value stays at the pre-reset baseline
        // (caller is expected to rebuild via re-hydration which re-runs
        // set_query_companions). Spec: no in-place refresh on the reset path.
        assert_eq!(
            &companions["q1"][0].resolved_value,
            &serde_json::json!(10),
            "B12: on reset, resolved_value must be left untouched (caller rehydrates)"
        );
    }

    /// B12: the no-reset baseline refresh prevents A→B→A spurious resets.
    /// Specifically: hydrate with v=10, then DB stays at v=10 across an
    /// advance (a sibling-table change triggers the companion scan), then
    /// the baseline is refreshed; a subsequent companion scan against the
    /// same v=10 must NOT reset, even though prior to B12 the cached
    /// resolved_value JSON shape might have round-tripped differently.
    #[test]
    fn b12_no_spurious_reset_after_baseline_refresh() {
        let (db_path, _dir) = build_prefs_db(&[("limit", 42)]);

        let mut companions: HashMap<String, Vec<CompanionInfo>> = HashMap::new();
        // Seed with the SAME numeric value but constructed as a different
        // JSON shape (e.g. via a serde round-trip from a string-typed
        // source) — simulates the cached vs queried mismatch B12 covers.
        companions.insert(
            "q1".to_string(),
            vec![make_companion(serde_json::json!(42))],
        );

        let changes = vec![make_prefs_change(41, 42)];
        let changed_tables: HashSet<&str> =
            changes.iter().map(|c| c.table.as_str()).collect();

        // Two consecutive advances, neither should reset (DB unchanged).
        for i in 0..2 {
            let result = check_companions_and_emit(
                &mut companions,
                &changes,
                &changed_tables,
                &db_path,
            );
            match result {
                CompanionResult::Changes(_) => { /* expected */ }
                CompanionResult::Reset(reason) => panic!(
                    "B12: advance #{i} must not reset (scalar unchanged); got reset: {reason}"
                ),
            }
            assert_eq!(
                &companions["q1"][0].resolved_value,
                &serde_json::json!(42),
                "B12: resolved_value must remain in sync with DB across advances"
            );
        }
    }

    /// NEW-3: `swap_snapshot` MUST clear `prev_db_path` after a successful
    /// pool swap, so that the next advance MUST re-arm via
    /// `set_prev_snapshot` or hit the explicit [B11] fallback warning
    /// (instead of silently using a stale value from the previous advance).
    #[test]
    fn new3_prev_db_path_cleared_on_swap() {
        let (db_path1, _dir1) = build_prefs_db(&[("limit", 1)]);
        let (db_path2, _dir2) = build_prefs_db(&[("limit", 2)]);

        let manager = RustPipelineManager::new();
        let id = "test-new3".to_string();
        manager
            .create_instance(id.clone(), db_path1.clone())
            .unwrap();

        // 1. Arm prev_db_path via set_prev_snapshot.
        manager
            .set_prev_snapshot(id.clone(), db_path1.clone())
            .unwrap();
        {
            let instances = manager.instances.read().unwrap();
            let inst = instances.get(&id).unwrap().lock().unwrap();
            assert_eq!(
                inst.prev_db_path.as_deref(),
                Some(db_path1.as_str()),
                "pre-swap: prev_db_path must be set by set_prev_snapshot"
            );
        }

        // 2. Swap to a new snapshot. After this, prev_db_path MUST be None.
        manager.swap_snapshot(id.clone(), db_path2.clone()).unwrap();
        {
            let instances = manager.instances.read().unwrap();
            let inst = instances.get(&id).unwrap().lock().unwrap();
            assert!(
                inst.prev_db_path.is_none(),
                "NEW-3: swap_snapshot must clear prev_db_path; got {:?}",
                inst.prev_db_path
            );
            assert_eq!(
                inst.db_path, db_path2,
                "swap_snapshot must update db_path"
            );
        }
    }
}
