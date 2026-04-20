# Features Research: napi-rs Capabilities

## Table Stakes (Must Have)

### Class Export
napi-rs supports `#[napi]` on `struct` + `impl` blocks to export Rust classes to JS:
```rust
#[napi]
pub struct Database { conn: rusqlite::Connection }

#[napi]
impl Database {
  #[napi(constructor)]
  pub fn new(path: String) -> Result<Self> { ... }
  #[napi]
  pub fn prepare(&self, sql: String) -> Result<Statement> { ... }
}
```
Generates matching TypeScript types automatically.

### Synchronous Methods
Critical for SQLite (synchronous API). napi-rs supports sync methods natively.

### Iterator/Generator Support
napi-rs supports returning iterators to JS via `Generator` type or `Iterator` trait impl.

### Error Handling
Rust `Result<T, napi::Error>` maps to JS exceptions.

### TypeScript Type Generation
`@napi-rs/cli` auto-generates `.d.ts` files from `#[napi]` annotations.

## Differentiators

### ThreadsafeFunction
napi-rs `ThreadsafeFunction` allows calling JS callbacks from Rust threads.
Useful for async operations that need to report back to JS.

### AsyncTask
`AsyncTask` trait for offloading work to libuv thread pool.
Could be used for batch SQLite operations.

### Buffer/TypedArray
Zero-copy buffer sharing between Rust and JS.
Useful for bulk data transfer (initial sync, large result sets).

## Anti-Features (Do Not Build)
- Don't expose raw SQLite handle to JS — keep it encapsulated in Rust
- Don't use async Rust for SQLite (it's synchronous) — use sync napi methods
- Don't try to share rusqlite Connection across threads (it's !Send when borrowed)

## Complexity Notes
- Class + method export: LOW complexity
- Iterator support: MEDIUM (need to manage lifetime carefully)
- Transaction callbacks: MEDIUM (closure capture across FFI boundary)
- Statement cache: LOW (Rust HashMap, no FFI needed)
