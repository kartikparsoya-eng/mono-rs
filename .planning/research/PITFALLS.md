# Pitfalls Research: Common Rust-Node Interop Mistakes

## Critical Pitfalls

### 1. Lifetime Management Across FFI
**Risk:** Rust borrows that outlive the FFI call boundary.
**Warning signs:** Compiler errors about lifetimes in `#[napi]` methods.
**Prevention:** Own all data returned to JS. Never return references. Clone if needed.
**Phase:** Phase 1 (Database/Statement)

### 2. Thread Safety with SQLite
**Risk:** rusqlite `Connection` is `Send` but not `Sync`. Sharing across JS callbacks is unsafe.
**Warning signs:** "cannot be shared between threads safely" compiler errors.
**Prevention:** One Connection per Database instance. No sharing. Use `Mutex` if absolutely needed.
**Phase:** Phase 1 (Database)

### 3. Statement Lifetime Tied to Connection
**Risk:** `Statement` borrows `Connection`. If Connection drops, Statement is dangling.
**Warning signs:** Use-after-free, segfaults.
**Prevention:** Use `CachedStatement` pattern in rusqlite. Or Arc<Connection> shared between Database and Statement instances.
**Phase:** Phase 1 (Statement)

### 4. Iterator Invalidation
**Risk:** JS can hold iterator while Rust state changes.
**Warning signs:** Stale data, panics during iteration.
**Prevention:** Materialize results for small sets. For large sets, use explicit cursor with bounds checking.
**Phase:** Phase 1 (Statement.iterate), Phase 5 (Snapshotter Diff)

### 5. Error Translation
**Risk:** Rust panics crash the Node.js process.
**Warning signs:** Process abort, no JS error caught.
**Prevention:** Always use `Result<T, napi::Error>`. Never `unwrap()` in napi-exported functions. Catch panics at boundary.
**Phase:** All phases

## Performance Pitfalls

### 6. Excessive FFI Crossings
**Risk:** Calling Rust function per-row negates batch benefits.
**Warning signs:** No improvement over better-sqlite3.
**Prevention:** Batch operations. Return Vec<Row> instead of one-at-a-time. Keep iteration loops in Rust.
**Phase:** Phase 1 (all()), Phase 3 (TableSource), Phase 5 (Snapshotter)

### 7. Unnecessary Serialization
**Risk:** Converting Rust structs to JS objects via JSON.
**Warning signs:** `serde_json` in hot paths.
**Prevention:** Use napi-rs direct object construction (`Object::new`, `env.create_object()`). Return napi types directly.
**Phase:** All phases

### 8. Memory Leaks from Reference Cycles
**Risk:** Rust holding JS references that prevent GC.
**Warning signs:** Growing memory usage over time.
**Prevention:** Minimize stored JS references. Use weak references where possible. Implement `Drop` properly.
**Phase:** Phase 2 (IVM operators with setOutput callbacks)

## API Compatibility Pitfalls

### 9. Behavioral Divergence
**Risk:** Rust module behaves slightly differently from TS (NULL handling, type coercion).
**Warning signs:** Existing vitest tests fail.
**Prevention:** Run existing test suite as primary gate. Match JS type coercion exactly.
**Phase:** Phase 1 (critical — 60+ dependents)

### 10. Missing Edge Cases
**Risk:** SQLite edge cases (empty strings vs NULL, INTEGER vs REAL coercion).
**Warning signs:** Subtle data corruption.
**Prevention:** Map SQLite types carefully: NULL→null, INTEGER→number/bigint, REAL→number, TEXT→string, BLOB→Buffer.
**Phase:** Phase 1
