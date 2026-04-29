# TS IVM → Rust IVM Port: Codebase Understanding & Audit

Date: 2026-04-29
Scope: `packages/zql/src/ivm/`, `packages/zero-ivm-rs/`, `packages/zqlite-rs/`,
`packages/zero-cache/src/services/view-syncer/`.

---

## 1. The big picture

The repo is mid-port from a JS/TS-only sync engine (Zero) to one where the
hot IVM path runs in Rust via napi-rs. Server-side correctness-critical work —
SQLite I/O, IVM operator trees — has been moved to Rust. TS still owns
orchestration: snapshot management, change-log reading, CVR/PG persistence,
auth/permissions, WebSocket, and (mostly vestigial) parallel TS pipelines.

There are two Rust crates:

- **`zero-ivm-rs`** (~6.3k LOC) — pure operator library, no SQLite. Defines an
  `Operator` trait (`fetch`, `push`, `push_child`) and a concrete operator per
  algebraic node: `FilterOperator`, `JoinOperator`, `ExistsOperator`,
  `OrExistsOperator`, `TakeOperator`, `SkipOperator`, `CapOperator`. Also a
  `Predicate` enum for `where`-clause evaluation, and a `Change` enum that
  serializes to the same `[type, node, extra]` tuple TS uses on the wire.
- **`zqlite-rs`** (~9k LOC) — SQLite-aware runtime. Owns `Database`/`Statement`
  (rusqlite napi bindings), the `RustTableSource` (with overlay for in-tx
  pending changes), `ConnectionPool`, the AST→`OperatorConfig` translator
  (`ast_to_config.rs`), the operator-tree builder (`hydrate.rs::build_operator_chain`),
  and `RustPipelineManager` — the per-client-group orchestrator that holds
  `Vec<Mutex<PipelineState>>` (one per query) plus tablespecs, permission
  tables, and companion-subquery metadata.

The TS PipelineDriver in `pipeline-driver.ts` still constructs a TS pipeline
per query via `buildPipeline()`, but its output is set to a no-op
(`{push: () => []}`). The TS pipeline exists today for: `getRow()` lookups via
`TableSource`, companion scalar-subquery resolution at hydration time
(`#resolveScalarSubqueries`), and structural metadata.

The actual data path is Rust-only. There is an explicit assertion in
`pipeline-driver.ts:1245` — `'RustPipelineManager must be available — Rust is
the sole advance path'` — so if `zqlite-rs` fails to load, `advance` throws.

---

## 2. Lifecycle: hydrate vs advance

### Hydrate (per query addition)

```
TS PipelineDriver.addQuery(queryID, AST, timer)
  ├── #resolveScalarSubqueries(ast)      [TS, runs companion subqueries once]
  ├── buildPipeline(...)                 [TS, structural-only — push is no-op]
  └── #rustHydrateQuery(queryID, ast)
        └── manager.addQuery(JSON{query_id, ast, primary_key,
                                   column_types, all_primary_keys})
              └── ast_to_operator_configs   [Rust]
              └── build_operator_chain      [Rust, builds nested Box<dyn Op>
                                            + push_ptrs Vec<*mut dyn Op>]
              └── chain.fetch(default)      [warm-up, populates Take/Cap state]
        └── manager.hydrateQuery(queryID) → Buffer  [refetches from chain]
              └── decodeAdvanceResultBuf → RowChange[]
```

For batch hydration (`addQueries`/`addQueriesAsync`), all rust-eligible
queries (those without companion subqueries) are added in sequence and then
hydrated in a single `manager.hydrate()`/`hydrateAsync()` call — Rust uses
rayon `par_iter` over pipelines.

### Advance (per replicator transaction)

```
TS PipelineDriver.advance(timer)
  ├── snapshotter.advance(...)           [TS, two-snapshot WAL2 BEGIN CONCURRENT
  │                                       diff via _zero.changeLog2]
  ├── collect Change[] from Diff         [TS, getRows from prev for unique-key
  │                                       constraint expansion]
  ├── manager.swapSnapshot(newDbPath)    [Rust pool re-points to curr.db]
  ├── manager.setPermissionTables(...)
  ├── manager.advance(JSON.stringify(changes)) → Buffer
  │     ├── push each change through operator chain via push_ptrs
  │     ├── route child-table changes via push_child on Join/Exists
  │     ├── permission-table filter + minRowVersion bump
  │     ├── companion scalar-subquery check (returns reset_signal if changed)
  │     └── emit_descendant_removals for cascading deletes on root remove
  └── decodeAdvanceResultBuf → RowChange[]
```

### Wire format

Inputs (changes, queries) go in as JSON. Outputs come back in a binary
allBuf-style buffer encoded by `advance.rs::encode_advance_result_buf` and
decoded in `decode-advance-buf.ts`:

```
[u32 count][u8 flags][changes…][optional reset_signal]
```

Per row: tagged values (0=null, 1=i64 LE, 2=f64, 3=text, 4=blob, 5=bool,
6=json fallback). The same protocol `Database.queryAll` uses for table reads.

---

## 3. Parallelism

Two layers:

1. `RustPipelineManager` is `RwLock<HashMap<String, Arc<Mutex<PipelineInstance>>>>`.
   Different client groups (instances) advance concurrently on libuv worker
   threads via `advanceAsync`/`hydrateAsync` (which return `napi::AsyncTask`).
   Multiple ViewSyncers can run IVM in parallel without blocking the JS event
   loop.
2. Inside one instance, `hydrate_instance` uses rayon `par_iter` across
   pipelines. `advance_instance` is sequential per-instance because it holds
   the instance Mutex, but the connection pool is rayon-thread-sized for the
   child-table SQL probes.

Snapshots: TS snapshotter uses WAL2's `BEGIN CONCURRENT` with two leapfrogging
connections (`prev` and `curr`). Rust never opens its own snapshots — it
shares whatever path TS has frozen via `swap_snapshot`.

---

## 4. Operator-by-operator notes

I read each TS operator side-by-side against its Rust counterpart. Summary:

| Operator | Rust file                       | TS file                                                                | Parity                                                                                                                                                                        |
| -------- | ------------------------------- | ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Filter   | `filter_op.rs`                  | `filter.ts` + `filter-push.ts` + `maybe-split-and-push-edit-change.ts` | ✓ Edit-split semantics match (4 cases)                                                                                                                                        |
| Join     | `join_op.rs`                    | `join.ts` + `join-utils.ts`                                            | ✓ `pushParent` and `pushChild` 1:1; compound-key constraint correctly post-filtered via `is_join_match`. Overlay logic skipped intentionally because Rust push is synchronous |
| Exists   | `exists_op.rs`                  | `exists.ts`                                                            | ✓ for membership transitions; ⚠ Edit only checks new node (same as TS, but Rust adds `or_predicate` which makes this riskier)                                                 |
| OrExists | `or_exists_op.rs`               | (no direct TS equivalent — TS uses applyWhere chaining)                | ✓ structurally                                                                                                                                                                |
| Take     | `take_op.rs`                    | `take.ts`                                                              | ✓ all 6 `pushEditChange` cases preserved                                                                                                                                      |
| Skip     | `skip_op.rs`                    | `skip.ts`                                                              | ✓ `getStart` 9-case logic preserved                                                                                                                                           |
| Cap      | `cap_op.rs`                     | `cap.ts`                                                               | ⚠ fetch semantics differ on repeated calls — but **Cap is dead code** (`ast_to_config.rs` never emits `OperatorConfig::Cap`)                                                  |
| Source   | `table_source.rs` + `source.rs` | `table-source.ts` (zqlite)                                             | structurally different but functionally correct                                                                                                                               |

`compare_values` in Rust matches TS `compareValues`: UTF-8 byte order for
strings (Rust `String::cmp` ≡ TS `compareUTF8`), null-first ordering, bool
false<true. The one divergence is cross-type comparison: TS throws, Rust
returns `Ordering::Equal`. The framework should never produce cross-type
comparisons, so this is more of a debugging-aid difference than a bug.

---

## 5. Issues found

### 🔴 Bug #1 — `LIKE` is silently case-insensitive in Rust

**Location:** `packages/zqlite-rs/src/hydrate.rs:769`

```rust
if let Some(val) = obj.get("like") {
    let pattern = val.as_str().ok_or("'like' value must be a string")?.to_string();
    return Ok(Predicate::Like(field, pattern, true));   // ← case_insensitive = true
}
```

**Impact:** TS `LIKE 'Foo%'` (case-sensitive) becomes case-insensitive in Rust.
Rows matching `'foo%'`, `'FOO%'` would incorrectly pass the filter. `ILIKE`,
`NOT LIKE`, `NOT ILIKE` all flow through `condition_to_predicate_json` and
hit this path; `ILIKE` happens to be correct because it should be
case-insensitive, but `LIKE` and `NOT LIKE` are wrong.

**Cross-check:** The other parser, `zero_ivm_rs::pipeline::parse_predicate`
(pipeline.rs:128-135), is correct (`Like(field, pattern, false)` for
`like`; `true` for `ilike`). The bug is local to `parse_predicate_json` —
looks like a copy-paste when the second arm was added. `like_match_impl`
(filter.rs:291-294) does honor the flag (`to_ascii_lowercase`), so this
manifests at runtime.

**Fix:** Change line 769 third arg to `false`.

---

### 🔴 Bug #2 — `EXISTS` parent_field missing from `split_edit_keys`

**Location:** `packages/zqlite-rs/src/advance.rs::collect_split_edit_keys`
(around line 852).

```rust
fn collect_split_edit_keys(ast: &Ast) -> Vec<String> {
    let mut keys = HashSet::new();
    if let Some(related) = &ast.related {
        for rel in related {
            for field in &rel.correlation.parent_field { keys.insert(field.clone()); }
        }
    }
    fn collect_from_cond(cond: &Condition, keys: &mut HashSet<String>) {
        match cond {
            Condition::And { conditions } | Condition::Or { conditions } => {
                for c in conditions { collect_from_cond(c, keys); }
            }
            _ => {}    // ← misses Condition::CorrelatedSubquery
        }
    }
    if let Some(cond) = &ast.where_cond { collect_from_cond(cond, &mut keys); }
    keys.into_iter().collect()
}
```

**Impact:** TS `buildPipelineInternal` (builder.ts:273-290) gathers
splitEditKeys from BOTH `ast.related[*].correlation.parentField` AND
`gatherCorrelatedSubqueryQueryConditions(ast.where)[*].related.correlation.parentField`
— i.e., the parent_field of every EXISTS / NOT EXISTS in the where clause.
Rust's `collect_from_cond` only walks AND/OR nodes; the
`Condition::CorrelatedSubquery` arm is missing.

When a column that is an EXISTS parent_field is updated:

- TS source emits `Remove + Add` → the Exists membership transition is clean.
- Rust source emits `Edit` → `ExistsOperator::push` Edit branch
  (exists_op.rs:243-259) only consults the new node:

```rust
Change::Edit { .. } => {
    let row = change.node().row.clone();
    if self.or_condition_matches(&row) { return vec![change]; }
    let pk = self.parent_key_str(&row);
    let count = self.parent_sizes.get(&pk).copied().unwrap_or_else(|| { ... });
    if self.passes_filter(count) { vec![change] } else { vec![] }
}
```

**Concrete failure:** parent row was in output because EXISTS matched at the
old parent_key, but the new parent_key has no children. TS emits Remove via
the source split; Rust emits nothing — stale row stays in the output. The
inverse case (becomes a member on edit) emits an `Edit` instead of `Add`,
which downstream may not handle.

**Fix:** Add the missing arm to `collect_from_cond`:

```rust
Condition::CorrelatedSubquery { related, .. } => {
    for f in &related.correlation.parent_field {
        keys.insert(f.clone());
    }
    if let Some(inner) = &related.subquery.where_cond {
        collect_from_cond(inner, keys);
    }
}
```

---

### 🟡 Risk #1 — `partition_key` not propagated to child Takes

**Location:** `ast_to_config.rs::ast_to_operator_configs` recurses without
passing partition_key (line 254). Every `Take` is built with
`partition_key: None`. Compare to TS `applyCorrelatedSubQuery`
(builder.ts:631) which threads `sq.correlation.childField` as the
partitionKey down into the child's `buildPipelineInternal`.

**Why it works today:** `take_op.rs::fetch` at lines 317-329 has a
deliberate fallback — when `partition_key` is None but a `constraint` is
present, it derives a per-constraint state key. Since `JoinOperator::fetch_children`
always passes a constraint of `child_field → parent_value`, each parent ends
up with isolated take state, matching TS's per-partition behavior.

**Why it's still a risk:**

- `Take::push_edit` in TS asserts `this.#partitionKeyComparator(old, new) === 0`
  to catch partition-key changes early (take.ts:432-440). Rust has no
  equivalent because there's no `partition_key`.
- TS adds `partitionKey` columns to splitEditKeys (builder.ts:274-276).
  Rust has no analogue. Combined with Bug #2, an edit that flips the
  partition column won't be split, won't be asserted, and will mis-update
  Take state.
- The constraint-fallback works only because Join/Exists _always_ fetch
  with a constraint. A future caller that fetches a child Take without a
  constraint would collapse partitions silently.

**Fix:** Either thread partition_key through `ast_to_operator_configs` so it
lands on the child `Take`, or — at minimum — extend `collect_split_edit_keys`
to include the parent's `child_field` for each related subquery.

---

### 🟡 Risk #2 — `debug_assert!` instead of `assert!` for framework invariants

The Rust port replaces several TS runtime `assert(...)` calls with
`debug_assert!`, which is stripped in release builds. Affected:

- `join_op.rs:219-221` — `Parent edit must not change relationship.`
- `join_op.rs:263-266` — `Child edit must not change relationship.`
- `exists_op.rs:170` — `Unexpected re-entrancy` (`in_push`)
- `take_op.rs:200, 246` — `Invalid state. Row has duplicate primary key`
- `cap_op.rs:166-169` — `Cap: partition key must not change on edit`

**Why it matters:** TS asserts loudly when a framework invariant is
violated (e.g., when split_edit_keys logic upstream fails). Rust
silently produces wrong output instead. Combined with Bug #2, this
makes regressions extremely hard to trace — the symptom (stale row,
missing Add) is far from the cause (split-edit-keys gap).

**Fix:** Promote these to `assert!`. They're cheap and the safety net
matters.

---

### 🟡 Risk #3 — Exists/OrExists Edit only checks the new node

`exists_op.rs:243-259` and the OrExists equivalent only consult the new
row when deciding whether to emit. TS `Exists` has the same property —
the comment in TS claims this is safe because a parent edit cannot
change the relationship's size, which is true for the count itself.

**But Rust has an `or_predicate`** on `OperatorConfig::Exists.or_condition`
(used to translate `OR(simple, EXISTS)` patterns into a single Exists with
short-circuit). If a parent edit flips the or-condition truth value, Rust
silently produces wrong output:

- old: or_predicate matched (row was in output regardless of size)
- new: or_predicate doesn't match, size is 0 → Rust emits nothing, stale
  row remains.

**Fix:** When `or_predicate` is set, evaluate it on both `old_node.row` and
`node.row` and split the Edit accordingly.

---

### 🟡 Risk #4 — `Cap` operator has divergent fetch semantics (but is unused)

`cap_op.rs::fetch` re-iterates the input every call and stops once
`state.size == limit`. TS `cap.ts::fetch` uses cached PKs to do point
lookups on subsequent fetches. These are not equivalent under repeated
fetches.

`ast_to_config.rs` never emits `OperatorConfig::Cap` (greppable: zero hits
in the AST translator). EXISTS subqueries always go through `Take` via
`apply_exists_limit`. Cap is dead code today.

**Fix:** Either remove `CapOperator` from the codebase, or rewrite its
fetch path to match TS's PK-lookup semantics before enabling it.

---

## 6. Things that are correct (verified)

- **Filter Edit-split:** Rust `filter_op.rs::push` matches TS
  `maybeSplitAndPushEditChange` exactly — 4-case (true,true)→Edit,
  (true,false)→Remove(old), (false,true)→Add(new), (false,false)→drop.
- **Join push/pushChild:** parent and child push paths are 1:1 with TS for
  ADD/REMOVE/EDIT/CHILD; compound-key constraints are post-filtered via
  `is_join_match`. The TS overlay used during fetch-during-push interleaving
  is structurally unnecessary in Rust because push is synchronous.
- **Skip `getStart`:** 9-case branching for forward/reverse × {undefined start,
  equal, greater, less} matches TS exactly.
- **Take `pushEditChange`:** all 6 cases (`oldCmp ∈ {<0,=0,>0}` × `newCmp`
  directions) translated faithfully. The `limit==1` special case and the
  `pushWithRowHiddenFromFetch` overlay are preserved.
- **`compareValues`:** UTF-8 byte order matches between TS `compareUTF8`
  and Rust `String::cmp` (Rust strings are UTF-8). Number/bool/null orderings
  match. Cross-type comparison is the one minor divergence (TS throws, Rust
  returns Equal).
- **`Change` enum tuple format:** `types.rs::Serialize/Deserialize` produces
  the TS `[type, node, extra]` tuple wire format.

---

## 7. Recommendation

Fix Bug #1 immediately. It's a one-character change, has user-visible
behavior (case-sensitive matches in TS, case-insensitive in Rust), and
isn't covered by parity tests in `parse_predicate_json`.

Fix Bug #2 before relying on the Rust path for production traffic. It's
silent, only fires under specific edits, and is the worst kind of
regression to debug after the fact.

Promote the `debug_assert!`s to `assert!` to guard against future
regressions and to make Bug #2 (and similar) loud rather than silent.
