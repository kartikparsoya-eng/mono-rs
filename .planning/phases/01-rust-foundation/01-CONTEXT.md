# Phase 1: Rust Foundation - Context

**Gathered:** 2026-04-20
**Status:** Ready for planning

<domain>
## Phase Boundary

Create a napi-rs Rust native module (`packages/zqlite-rs/`) that exactly replaces `db.ts` Database and Statement classes with zero behavioral changes. The StatementCache stays in TypeScript wrapping the Rust Database. Existing `db.test.ts` must pass unchanged with the Rust module swapped in.

</domain>

<decisions>
## Implementation Decisions

### Crate Location & Packaging
- **D-01:** New sibling package `packages/zqlite-rs/` with its own `Cargo.toml` + `package.json`. Clean separation from existing `zqlite`. TS consumers change imports from `zqlite` db.ts to `zqlite-rs`.

### FFI Boundary Design
- **D-02:** 1:1 method mapping — each method (prepare, exec, run, get, all, iterate, pragma, transaction) is a separate napi export matching the existing TS API exactly. No batching or coarse-grained calls.

### Statement Lifetime & GC
- **D-03:** Database-owned + Disposable pattern. Database owns all statements in Rust. Statement JS objects hold a reference to the Database. When Database is disposed (`[Symbol.dispose]()`), all statements are invalidated. Matches existing Disposable pattern in `db.ts`.

### SQLite Build Config
- **D-04:** Bundled SQLite via `rusqlite` `bundled` feature, compiled from source. Enable FTS5 and JSON1 extensions. Patch SQLite source in `build.rs` for WAL2 support. Full control, reproducible builds.

### OpenTelemetry Tracing
- **D-05:** No OTel dependency in Rust. Rust emits timing metadata (duration) as return values. TS wrapper layer converts timing data to OpenTelemetry spans using existing tracing patterns.

### Logging Strategy
- **D-06:** Skip logging in Rust for Phase 1. No LogContext bridge, no `tracing` crate. Add logging bridge in a later phase if needed. Keep foundation simple.

### Error Handling
- **D-07:** Rust returns Result-like objects (no exceptions thrown from Rust). TS wrapper checks return values and throws appropriate error classes (DatabaseInitError, SqliteError) for compatibility with existing error handling code.

### Statement Cache Location
- **D-08:** StatementCache stays in TypeScript wrapping Rust `Database.prepare()`. Cache logic is simple (Map + array), overhead is minimal. Easier to debug and modify. No Rust cache implementation needed in Phase 1.

### Claude's Discretion
- Exact Cargo.toml dependency versions for napi-rs, rusqlite
- Build configuration details (napi-rs build targets, optimization flags)
- Internal Rust module structure within the crate
- Test scaffolding approach for `#[cfg(test)]` tests

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Source Files (rewrite targets)
- `packages/zqlite/src/db.ts` — Database + Statement classes to replace (329 lines)
- `packages/zqlite/src/internal/statement-cache.ts` — StatementCache staying in TS but must work with Rust Database (131 lines)

### Test Files (correctness gates)
- `packages/zqlite/src/db.test.ts` — Must pass unchanged with Rust module (183 lines)

### Dependencies (understand API contract)
- `packages/zqlite/package.json` — Current dependencies including `@rocicorp/zero-sqlite3`
- `@rocicorp/zero-sqlite3` — The better-sqlite3 fork being replaced; type coercion behavior must match exactly

### Codebase Maps
- `.planning/codebase/CONVENTIONS.md` — Naming, patterns, Disposable usage
- `.planning/codebase/STACK.md` — Tech stack details
- `.planning/codebase/TESTING.md` — Test patterns and vitest config

### Research
- `.planning/research/PITFALLS.md` — Known risks (NULL handling, type coercion, statement lifetimes)
- `.planning/research/STACK.md` — napi-rs v3 + rusqlite research

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `db.ts` Database class (329 lines) — defines the exact API surface to replicate
- `statement-cache.ts` StatementCache — stays TS, will wrap Rust Database
- `@rocicorp/zero-sqlite3` — better-sqlite3 fork; defines type coercion behavior to match

### Established Patterns
- **Disposable:** `[Symbol.dispose]()` for resource cleanup — Rust must support this
- **LogContext:** Threaded through constructors — Rust skips this for Phase 1, TS wrapper handles
- **OpenTelemetry:** Manual spans via `manualSpan()` — Rust returns timing, TS creates spans
- **Private fields:** `#field` pattern — Rust uses napi class encapsulation instead

### Integration Points
- 60+ files import from `db.ts` — all need import path change to `zqlite-rs`
- `statement-cache.ts` imports `Database` and `Statement` types from `db.ts` — must work with Rust exports
- `statements.ts` (Phase 2) depends on Database/Statement — Phase 1 must be compatible

</code_context>

<specifics>
## Specific Ideas

No specific requirements — open to standard approaches for napi-rs crate scaffolding and Rust module structure.

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope.

</deferred>

---

*Phase: 01-rust-foundation*
*Context gathered: 2026-04-20*
