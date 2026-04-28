# Stack Research: Rust napi-rs + SQLite for zero-cache

## Recommended Stack

### Core

- **napi-rs** (v3.x) — Rust-to-Node.js FFI framework
  - `#[napi]` macro for automatic TypeScript type generation
  - `@napi-rs/cli` for build tooling and cross-platform distribution
  - Supports classes, async functions, iterators, typed arrays
  - Platform packages: `{name}-darwin-arm64`, `{name}-linux-x64-gnu`, etc.

### SQLite

- **rusqlite** (0.39.x) — Ergonomic Rust wrapper for SQLite
  - `Connection::open()`, `Connection::open_in_memory()`
  - `conn.prepare()` → `Statement`
  - `stmt.query_map()` for row iteration
  - `params![]` and `named_params!{}` macros
  - Features: `bundled` (embeds SQLite), `backup`, `blob`, `hooks`, `trace`, `vtab`
  - WAL mode and WAL2 supported via raw SQLite API
  - `CachedStatement` for prepared statement caching
  - `Transaction` and `Savepoint` for transaction management

### Build

- **Cargo** workspace (can coexist with npm workspaces)
- **@napi-rs/cli** for building `.node` files
- Build script: `napi build --release`

### What NOT to Use

- **wasm-bindgen** — no direct filesystem access, wasm overhead, no multi-threading
- **libsqlite3-sys** directly — too low-level, rusqlite wraps it properly
- **sqlx** — async ORM, overkill for embedded SQLite (rusqlite is synchronous and faster)
- **better-sqlite3 from Rust** — defeats the purpose (still JS↔C++ FFI)

## Confidence Levels

- napi-rs: **HIGH** — industry standard, used by SWC, Parcel, Prisma
- rusqlite: **HIGH** — most popular Rust SQLite crate, well-maintained
- bundled SQLite: **HIGH** — eliminates system dependency, ensures version consistency
