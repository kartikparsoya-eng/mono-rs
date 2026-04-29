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
        let mut instance = instance_mutex.lock().unwrap();
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
        let mut instance = instance_mutex.lock().unwrap();
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
        let mut instance = instance_mutex.lock().unwrap();
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
        let mut instance = instance_mutex.lock().unwrap();

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
        let mut instance = instance_mutex.lock().unwrap();
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
        let instance = instance_mutex.lock().unwrap();

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
        let instance = instance_mutex.lock().unwrap();

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
        let instance = instance_mutex.lock().unwrap();

        let result = advance_instance(&instance, &changes);
        Ok(Buffer::from(encode_advance_result_buf(&result)))
    }

    /// Re-open SQLite connections at a new path without rebuilding operator trees.
    #[napi]
    pub fn swap_snapshot(&self, id: String, new_db_path: String) -> napi::Result<()> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        let mut instance = instance_mutex.lock().unwrap();

        instance.shared_pool.swap_path(&new_db_path)
            .map_err(|e| napi::Error::from_reason(format!("Failed to swap pool: {e}")))?;

        for pm in instance.pipelines.iter() {
            let pipeline = pm.lock().unwrap();
            pipeline.source.reset_state();
        }

        instance.db_path = new_db_path;
        Ok(())
    }

    /// Returns the number of active pipelines for an instance.
    #[napi]
    pub fn pipeline_count(&self, id: String) -> napi::Result<u32> {
        let instances = self.instances.read()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let instance_mutex = instances.get(&id)
            .ok_or_else(|| napi::Error::from_reason(format!("No instance: {id}")))?;
        let instance = instance_mutex.lock().unwrap();
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
}

// ─── Extracted instance-level operations (shared by sync + async paths) ─────

/// Run advance logic on a locked PipelineInstance. Used by both sync and async paths.
fn advance_instance(instance: &PipelineInstance, changes: &[Change]) -> AdvanceResult {
    if changes.is_empty() {
        return AdvanceResult { changes: vec![], error: None, error_type: None, reset_signal: None };
    }

    // 1. Push changes through IVM operator trees
    let mut all_row_changes: Vec<RowChange> = instance.pipelines.iter().flat_map(|pm| {
        let mut pipeline = pm.lock().unwrap();
        advance_persistent_pipeline(&mut pipeline, changes, &instance.db_path)
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

        match check_companions_and_emit(
            &instance.companions,
            changes,
            &changed_tables,
            &instance.db_path,
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
        let instance = self.instance.lock()
            .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
        let result = advance_instance(&instance, &self.changes);
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
fn check_companions_and_emit(
    companions: &HashMap<String, Vec<CompanionInfo>>,
    changes: &[Change],
    changed_tables: &HashSet<&str>,
    db_path: &str,
) -> CompanionResult {
    let mut companion_changes = Vec::new();

    for (query_id, query_companions) in companions {
        for companion in query_companions {
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
}
