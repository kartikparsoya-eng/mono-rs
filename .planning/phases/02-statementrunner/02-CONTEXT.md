# Phase 2: StatementRunner - Context

**Gathered:** 2026-04-20
**Status:** Skipped (no Rust work needed)

<domain>
## Phase Boundary

StatementRunner (`statements.ts`, 70 lines) is a pure TS delegation layer over StatementCache + Database. Since Phase 1 already put Database in Rust, StatementRunner already delegates to Rust underneath. No rewrite needed.

</domain>

<decisions>
## Implementation Decisions

### Keep StatementRunner in TypeScript
- **D-01:** StatementRunner stays as-is in TypeScript. It's 70 lines of pure delegation (`statementCache.use(sql, s => s.run(...args))`). No performance-critical logic.
- **D-02:** Phase 1 decision D-08 (StatementCache stays in TS) is NOT reversed. The entire TS chain `StatementRunner → StatementCache → Database (Rust)` is the final architecture for this layer.
- **D-03:** 25+ consumer files keep their existing `import {StatementRunner} from '../../db/statements.ts'` unchanged. No import changes needed.
- **D-04:** If a future phase needs to move more logic into Rust, StatementRunner can be re-exported from a Rust shim without changing consumers.

### Rationale
- Per-call FFI overhead dominates at this layer (benchmark showed 0.5-0.65x vs better-sqlite3)
- Real Rust wins come from Phases 3-6 where entire hot loops stay in Rust
- Moving StatementRunner to Rust would add FFI round-trips (Rust→TS StatementCache→Rust Database) — net negative

</decisions>

<canonical_refs>
## Canonical References

No external specs — phase skipped by user decision.

### Source Files (unchanged)
- `packages/zero-cache/src/db/statements.ts` — Stays as-is (70 lines)
- `packages/zero-cache/src/db/statements.test.ts` — Already passing (49 lines)

</canonical_refs>

<code_context>
## Existing Code Insights

### Consumer Count
- 25+ files across zero-cache import StatementRunner
- Major consumers: snapshotter.ts, change-processor.ts, replication-state.ts, change-log.ts, load-permissions.ts, write-authorizer.ts

### Current Call Chain (already optimal)
```
StatementRunner.run(sql, ...args)    [TS, 70 lines]
  → StatementCache.use(sql, fn)      [TS, 131 lines]
    → Database.prepare(sql)           [Rust napi, Phase 1]
    → Statement.run(params)           [Rust napi, Phase 1]
```

</code_context>

<specifics>
## Specific Ideas

None — phase skipped.

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope.

</deferred>

---

*Phase: 02-statementrunner*
*Context gathered: 2026-04-20*
*Decision: Skip — no Rust work needed, proceed to Phase 3*
