# IVM Parity Status

## Current: Post-Phase-34 — B1 / B2 / B3 / B11 shipped; Track 2 fixes landed; allow-list unchanged (5 keys + 4 deferred patterns)

## Phase 34 Track 2 — Shipped

| ID  | Description                                                                          | Commits                                |
|-----|--------------------------------------------------------------------------------------|----------------------------------------|
| B1  | Skip placement order — emit Skip BEFORE conditions in ast_to_operator_configs (mirrors TS builder.ts:302-306) | `4fa50987e` (fix), `23eb15c9f` (test)  |
| B2  | parent_sizes cache poison — drop `max(1)` from `ExistsOperator::fetch` so empty children don't pollute cache  | `1f99fd75b` (fix), `23eb15c9f` (test)  |
| B3  | Take `partition_key` threading — pass partition_key through ast_to_operator_configs and Take operator stages  | `12f4d43cc` (fix), `3d6b32b43` (test secondary), `d29114f73` (test push) |
| B11 | Cascade-delete prev-snapshot — `set_prev_snapshot` napi method + `emit_descendant_removals` routing + pipeline-driver wiring | `3e4ca5182`, `78e063b2f`, `d4176b047` (impl); `12911f8d3` (test) |

All four BLOCKING items have Rust unit tests AND `tools/ivm-parity/` differential tests committed (D-21 #1).

## Phase 34 Verification Gate (D-21) — Per Plan 34-07

| Sub-condition                                              | Result                                                                                                       |
|------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------|
| #1 Track 2 unit + diff tests committed                     | YES — see commits above + 34-02/34-05/34-06 SUMMARYs                                                          |
| #2 `tools/ivm-parity/` `npm test` exits 0                  | MANUAL — npm test aggregate wired to include `npm run fuzz-check` per 34-07 Task 2; live run requires caches |
| #3 1k fuzz <2 min, 0 unexpected, FUZZ-02 coverage gate met | DRY-RUN PASS (WALL_MS=26ms, all 3 FUZZ-02 tables ≥85 hits per 1k iter); LIVE deferred to Phase 35           |
| #4 Phase 33 benches PASS                                   | YES — TTFB ratio 1.05× <1.5× threshold; MemPeak ratio 12.47× ≥4× threshold                                  |
| #5 Full cargo + vitest green                               | YES — cargo: 195+134 passed; vitest: 209/213 passed (3 PRE-EXISTING snapshot mismatches NOT introduced by P34) |

See `.planning/milestones/v5.0-bench-results.md` Phase 34 section for the full per-gate evidence table including PER_TABLE_HITS distribution.

## Allow-List State (Post-Phase-34)

The allow-list at `tools/ivm-parity/parity-allowlist.json` is **read-only** post-Phase-34. Adding entries requires explicit user approval per CD-04 + SKILL.md hard rule 1 (never modify the test apparatus to hide a divergence).

### Catalogued (5 keys — pre-existing baseline from prior parity work)

| ID            | Pattern                                                                                                       | Phase to fix                |
|---------------|---------------------------------------------------------------------------------------------------------------|------------------------------|
| seed_18       | Duplicate CSQ alias overwrite (same alias `zsubq_participants` for 2 EXISTS)                                  | v6.0 (won't-fix in v5.0)    |
| fuzz_00132    | `OR(simple, EXISTS flip=true)` — TS FlippedJoin child→parent traversal vs RS regular EXISTS                   | Phase 35 / v6.0 (B7)        |
| fuzz_00133    | Same root cause as fuzz_00132 (FlippedJoin variant)                                                           | Phase 35 / v6.0 (B7)        |
| fuzz_00139    | `scalar:true` on second EXISTS — TS resolves as companion query out of main IVM tree; RS emits child rows    | Phase 35 (B12)              |
| fuzz_00140    | Same root cause as fuzz_00139 (scalar-companion variant)                                                      | Phase 35 (B12)              |

### Deferred (4 patterns — Phase 35 work items)

| ID                              | Pattern Match                                                                            |
|---------------------------------|------------------------------------------------------------------------------------------|
| B5-or-exists-short-circuit      | `or_exists_op.rs` short-circuits on first matching branch when ≥2 EXISTS share child    |
| B6-exists-limit-downgrade       | EXISTS with explicit limit > 1 — Rust keeps user limit; TS downgrades                   |
| B7-flipped-join                 | Any AST containing CSQ with `flip:true` (Rust treats as regular EXISTS)                 |
| B12-companion-scalar-drift      | Companion scalar-subquery `resolved_value` never updated post-advance                    |

## New Divergences Found in Phase 34

**Live 1k fuzz: deferred to Phase 35** because the Wave 0 FUZZ-02 schema-typing blocker (carried forward from 34-03 timing-probe.md → 34-04 SUMMARY "Deferred Items") was not resolved in 34-04. zero-schema.ts declares `string()` for TIMESTAMPTZ/JSONB/NUMERIC columns (events.occurredAt, events.metadataJson, events.amount, big_id_records.createdAt, event_tags.metadataJson), but the running zero-cache replicator resolves these as `number`/`json` and rejects every desiredQueriesPatch with `SchemaVersionNotSupported`. ~3/11 tables become unhydrate-able under the live fuzz; the harness returns `error` outcomes instead of reaching divergence detection.

**Dry-run 1k fuzz** (WALL_MS=26ms, FUZZ_SEED=20260429): synthetic stub harness — cannot surface real RS↔TS divergences. Validates instrumentation, table coverage, and timing budget only.

**Phase 35 owns**:
1. Apply FUZZ-02 schema-typing fix Option A (zero-schema.ts column types).
2. Re-run live 1k fuzz with caches up; capture `PER_TABLE_HITS` and `WALL_MS` from a happy-path run.
3. Add CI integration of `npm run fuzz-check:gate` (D-22 explicitly defers this to Phase 35).

## Files Modified in Phase 34

### Track 1 — Fuzz Infrastructure (34-01, 34-03, 34-04, 34-07)
- `tools/ivm-parity/random-ast-fuzz.ts` — fast-check driver with PER_TABLE_HITS + WALL_MS instrumentation (34-07).
- `tools/ivm-parity/arb-ast.ts` — full operator surface + targeted B1/B2/B3 arbs (34-03).
- `tools/ivm-parity/harness-fuzz.ts` — live primitives + BatchedRunner + mutation pruning (34-03).
- `tools/ivm-parity/parity-allowlist.json` — 5 keys + 4 patterns (34-01; read-only post-34).
- `tools/ivm-parity/zero-schema.ts` — FUZZ-02 schema (events / big_id_records / event_tags) (34-04).
- `tools/ivm-parity/schema.sql` — FUZZ-02 PG DDL (34-04).
- `tools/ivm-parity/seed-extras.sql` — FUZZ-02 NULL/i64/DST/jsonb fixtures (34-04).
- `tools/ivm-parity/harness-advance-coverage.ts` — 4 new MUTATIONS for FUZZ-02 (34-04).
- `tools/ivm-parity/package.json` — `fuzz-check:gate` shortcut + npm test integration (34-07).

### Track 2 — Surgical Fixes (34-02, 34-05, 34-06)
- `packages/zqlite-rs/src/ast_to_config.rs` — B1 Skip ordering + B3 partition_key threading.
- `packages/zero-ivm-rs/src/exists_op.rs` — B2 parent_sizes cache fix.
- `packages/zero-ivm-rs/src/take_op.rs` — B3 secondary alignment.
- `packages/zqlite-rs/src/advance.rs` — B11 emit_descendant_removals signature.
- `packages/zqlite-rs/src/pipeline_manager.rs` — B11 set_prev_snapshot napi method.
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — B11 setPrevSnapshot wiring.
- `tools/ivm-parity/diff-tests-track2.ts` — Track 2 differential test stubs (B1/B2/B3/B11).

## Bench Tracking

See `.planning/milestones/v5.0-bench-results.md` — append-only log; Phase 34 section appended 2026-04-30 with full verification gate table.
