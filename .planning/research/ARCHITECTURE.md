# Architecture Research: Rust-Node FFI Patterns

## Component Boundaries

### Rust Layer (napi-rs module)

All SQLite I/O lives here:

```
Rust Module
├── Database (wraps rusqlite::Connection)
├── Statement (wraps rusqlite::CachedStatement)
├── TableSource (IVM Input implementation)
├── DatabaseStorage (IVM Storage implementation)
├── Snapshotter (snapshot diff iteration)
├── ChangeProcessor (CDC writer)
└── StatementRunner (parameterized queries)
```

### TypeScript Layer (unchanged)

```
TypeScript
├── WebSocket/HTTP (Fastify)
├── Auth (jose)
├── Protocol (zero-protocol)
├── Change Streamer
├── IVM Operators (Phase 2 target, stays TS initially)
└── CVR Store (Phase 3 target, stays TS initially)
```

## Data Flow Across FFI Boundary

### Direction: TS → Rust

- `new Database(path)` — constructor
- `db.prepare(sql)` — SQL string
- `stmt.run(...params)` — JS values → Rust SQLite params
- `tableSource.fetch(req)` — FetchRequest object → Rust

### Direction: Rust → TS

- `stmt.get()` → JS object (row)
- `stmt.all()` → JS array of objects
- `stmt.iterate()` → JS iterator
- `tableSource.fetch()` → rows as JS objects

### Key Design Decision: Row Representation

Current TS uses `Object.fromEntries()` per row (bottleneck).
Rust should return rows as JS objects directly via napi-rs, avoiding intermediate representation.

## Build Order (Dependency-Driven)

```
1. Database + Statement (foundation, 60+ consumers)
2. StatementRunner (unlocks zero-cache modules)
3. TableSource + DatabaseStorage (parallel, IVM data layer)
4. Schema modules (parallel, small)
5. Snapshotter + ChangeProcessor (parallel, high-impact)
6. InitialSync (bulk loader)
```

## Suggested Module Structure

```
packages/zero-cache-rs/       # New Rust crate
├── Cargo.toml
├── src/
│   ├── lib.rs               # napi entry point
│   ├── database.rs          # Database class
│   ├── statement.rs         # Statement class
│   ├── statement_cache.rs   # LRU cache
│   ├── table_source.rs      # IVM Input
│   ├── database_storage.rs  # IVM Storage
│   ├── snapshotter.rs       # Diff iteration
│   ├── change_processor.rs  # CDC writer
│   └── initial_sync.rs      # Bulk loader
├── build.rs
└── package.json             # npm package wrapper
```
