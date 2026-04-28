# Phase 11: Rust Join Operator — Context

## Goal

Port the Join operator's hot-path computation to Rust for maximum perf gain (3.56x slower than Filter baseline at 44.8ms).

## Architecture: Hybrid Rust/TS

### What Rust handles (hot path)

- `isJoinMatch(parent, parentKey, child, childKey)` — per-row key comparison
- `buildJoinConstraint(sourceRow, sourceKey, targetKey)` — constraint object building
- `rowEqualsForCompoundKey(a, b, key)` — compound key equality
- `compareValues(a, b)` — already in Rust from Phase 10

### What stays in TS

- Generator orchestration (`fetch()`, `pushParent()`, `pushChild()`)
- Overlay generators (`generateWithOverlay`, `generateWithOverlayUnordered`) — deeply coupled to JS generator protocol
- Relationship closures — opaque pass-through
- `parent.fetch()` and `child.fetch()` calls — stay in TS

## Decisions

- **D-49:** Hybrid Rust pushChild batch — Rust handles matching/comparing/building, TS keeps generator orchestration
- **D-50:** Parent/child fetch() stays in TS — Rust receives materialized arrays
- **D-51:** Relationships pass-through opaque — consistent with Phase 10
- **D-52:** Overlay generators stay in TS — deeply coupled to JS generator protocol
- **D-53:** Rust handles matching: isJoinMatch, buildJoinConstraint, rowEqualsForCompoundKey, compareValues

## Source Files (reference only — DO NOT MODIFY)

- `packages/zql/src/ivm/join.ts` — Join operator (~303 lines)
- `packages/zql/src/ivm/join-utils.ts` — Hot-path utility functions (~252 lines)
- `packages/zql/src/ivm/data.ts` — compareValues, Node type

## Test Files (DO NOT MODIFY — correctness oracle)

- `packages/zql/src/ivm/join.test.ts`
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts`

## Integration

Delegate-based injection via `pipeline-driver.ts` (same pattern as Phase 10 Filter/Take).
BuilderDelegate gets `createJoin?` optional method.
