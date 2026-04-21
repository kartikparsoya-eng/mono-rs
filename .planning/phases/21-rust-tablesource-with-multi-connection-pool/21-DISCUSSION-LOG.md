# Phase 21: Rust TableSource with Multi-Connection Pool - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 21-rust-tablesource-with-multi-connection-pool
**Areas discussed:** Connection Pool Architecture, Overlay System Design, Source-Operator Integration, SQL Query Building
**Mode:** auto (all areas auto-selected with recommended defaults)

---

## Connection Pool Architecture

| Option                  | Description                                            | Selected |
| ----------------------- | ------------------------------------------------------ | -------- |
| Custom lightweight pool | Vec<Connection> behind Arc<Mutex>, take/return pattern | ✓        |
| r2d2 pool               | Full-featured connection pool crate                    |          |
| crossbeam channel pool  | Channel-based checkout                                 |          |

**User's choice:** [auto] Custom lightweight pool (recommended default)
**Notes:** r2d2 is overkill for fixed-snapshot read-only connections. WAL snapshot pinning via BEGIN DEFERRED on each connection at pool creation.

---

## Overlay System Design

| Option                | Description                                    | Selected |
| --------------------- | ---------------------------------------------- | -------- |
| HashMap-based overlay | Match TS semantics exactly with epoch tracking | ✓        |
| BTreeMap overlay      | Sorted overlay for ordered merge               |          |

**User's choice:** [auto] HashMap-based overlay (recommended default)
**Notes:** Matches TS Overlay type. Epoch tracking for lastPushedEpoch per connection.

---

## Source-Operator Integration

| Option                | Description                               | Selected |
| --------------------- | ----------------------------------------- | -------- |
| Separate Source trait | Mirror TS Source/Input/Output distinction | ✓        |
| Extend Operator trait | Source as special Operator variant        |          |

**User's choice:** [auto] Separate Source trait (recommended default)
**Notes:** TS has distinct interfaces. Source.connect() returns SourceInput with fetch(). Pipeline builder wires as leaf.

---

## SQL Query Building

| Option          | Description                            | Selected |
| --------------- | -------------------------------------- | -------- |
| Port to Rust    | Rewrite buildSelectQuery() in Rust     | ✓        |
| Call TS for SQL | Have TS build SQL string, pass to Rust |          |

**User's choice:** [auto] Port to Rust (recommended default)
**Notes:** Must be in Rust for connection pool to work on Rayon threads. ~200 lines, straightforward port.

---

## Claude's Discretion

- Internal pool data structures
- SQL parameter binding strategy
- Statement caching approach
- Error handling strategy

## Deferred Ideas

None
