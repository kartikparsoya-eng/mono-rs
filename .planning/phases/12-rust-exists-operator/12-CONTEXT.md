# Phase 12: Rust Exists Operator — Context

## Goal

Port the Exists operator to Rust. Exists is a FilterOperator that checks relationship size and caches results.

## Architecture Decisions

### D-59: Batch push + decision logic in Rust

Single napi call per push cycle. fetchSize stays in TS.

### D-60: Cache stays in TS Map

Rust receives pre-resolved exists values. Cache (parentJoinKey → boolean) managed by TS wrapper.

### D-61: New `createExists?` delegate method on BuilderDelegate

Follows same optional delegate pattern as `createStorage?` for Take.

## Scope

### Rust handles

- Batch push + decision logic (ADD/EDIT/REMOVE → filter, CHILD changes → size check → convert)
- Single napi call per push cycle

### TS handles

- `fetchSize()` (iterates relationship generators)
- Cache (`Map<string, boolean>` of parentJoinKey → exists boolean)
- `beginFilter`/`endFilter` lifecycle

## Source Reference

- `packages/zql/src/ivm/exists.ts` — ~250 lines, port target
- `packages/zql/src/ivm/filter-operators.ts` — FilterInput, FilterOutput interfaces

## Test Coverage

- `exists.test.ts`: 1 unit test (beginFilter/endFilter forwarding)
- `pipeline-driver.test.ts`: 30 tests (integration, 1 pre-existing failure)

## Integration Pattern

- Same as Join (Phase 11): wrapper in `view-syncer/`, delegate method on BuilderDelegate
- Reference: `packages/zero-cache/src/services/view-syncer/rust-join.ts`
