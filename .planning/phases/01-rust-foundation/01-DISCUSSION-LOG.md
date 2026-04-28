# Phase 1: Rust Foundation - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-20
**Phase:** 01-rust-foundation
**Areas discussed:** Crate location, FFI boundary design, Statement lifetime, SQLite build config, OTel tracing, Logging strategy, Error handling, Statement cache location

---

## Crate Location & Packaging

| Option                      | Description                                                  | Selected |
| --------------------------- | ------------------------------------------------------------ | -------- |
| Sibling package (zqlite-rs) | New `packages/zqlite-rs/` with own Cargo.toml + package.json | ✓        |
| Nested in zqlite            | Rust crate inside `packages/zqlite/native/`                  |          |
| Cargo workspace at root     | Top-level `crates/` with Cargo workspace                     |          |

**User's choice:** Sibling package (zqlite-rs)
**Notes:** Clean separation, existing zqlite stays untouched.

---

## FFI Boundary Design

| Option                    | Description                                           | Selected |
| ------------------------- | ----------------------------------------------------- | -------- |
| 1:1 method mapping        | Each method is a separate napi export matching TS API | ✓        |
| Coarse-grained batched    | Grouped operations, fewer FFI crossings               |          |
| 1:1 first, optimize later | Start simple, optimize hot paths later                |          |

**User's choice:** 1:1 method mapping
**Notes:** Keeps API identical, simplest to swap in.

---

## Statement Lifetime & GC

| Option                        | Description                                      | Selected |
| ----------------------------- | ------------------------------------------------ | -------- |
| Database-owned + Disposable   | Database owns all statements, Disposable pattern | ✓        |
| Independent ref-counted       | Each Statement independently ref-counted         |          |
| napi-rs default (Drop via GC) | Let napi-rs GC invoke Rust Drop                  |          |

**User's choice:** Database-owned + Disposable
**Notes:** Matches existing pattern in db.ts.

---

## SQLite Build Config

| Option                       | Description                                    | Selected |
| ---------------------------- | ---------------------------------------------- | -------- |
| Bundled + patched for WAL2   | Compile from source, FTS5, JSON1, WAL2 patched | ✓        |
| Bundled standard, WAL2 later | Standard extensions, skip WAL2 for now         |          |
| Link existing zero-sqlite3   | Link against same SQLite as existing module    |          |

**User's choice:** Bundled + patched for WAL2
**Notes:** Full control, reproducible builds.

---

## OTel Tracing in Rust

| Option                     | Description                                       | Selected |
| -------------------------- | ------------------------------------------------- | -------- |
| Rust-native OTel           | opentelemetry-rust crate, spans from Rust         |          |
| TS-side tracing only       | No tracing in Rust, TS wrapper creates spans      |          |
| Timing metadata + TS spans | Rust emits timing data, TS converts to OTel spans | ✓        |

**User's choice:** Timing metadata + TS spans
**Notes:** No OTel dependency in Rust, still get Rust-level timing.

---

## Logging Strategy

| Option             | Description                             | Selected |
| ------------------ | --------------------------------------- | -------- |
| JS callback bridge | Rust calls back to JS LogContext        |          |
| Rust-internal only | tracing crate, no output crosses FFI    |          |
| Skip for Phase 1   | No logging in Rust, add later if needed | ✓        |

**User's choice:** Skip for Phase 1
**Notes:** Keep foundation simple.

---

## Error Handling Shape

| Option                      | Description                                            | Selected |
| --------------------------- | ------------------------------------------------------ | -------- |
| napi::Error + TS wrapper    | Rust throws napi::Error, TS wraps in custom classes    |          |
| Rust-exported error classes | Rust exports matching error hierarchy                  |          |
| Result objects (no throw)   | Rust returns Result-like objects, TS checks and throws | ✓        |

**User's choice:** Result objects (no throw)
**Notes:** No exceptions from Rust side.

---

## Statement Cache Location

| Option                  | Description                                       | Selected |
| ----------------------- | ------------------------------------------------- | -------- |
| Fully in Rust           | get/return/use/drop pattern in Rust               |          |
| Keep in TS              | StatementCache stays in TS wrapping Rust Database | ✓        |
| Rust cache, same TS API | Cache in Rust, expose same API to TS              |          |

**User's choice:** Keep in TS
**Notes:** Cache logic is simple, easier to debug and modify.

---

## Claude's Discretion

- Cargo.toml dependency versions
- Build configuration details
- Internal Rust module structure
- Test scaffolding approach

## Deferred Ideas

None — discussion stayed within phase scope.
