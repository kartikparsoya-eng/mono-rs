---
phase: 34
plan: 34-01
slug: wave-0-scaffolding
subsystem: tools/ivm-parity + Rust IVM Red-state stubs
tags: [scaffolding, fuzz, fast-check, fuzz-02-schema, b1, b2, b3, b11, red-state]
dependency_graph:
  requires:
    - 34-CONTEXT.md (D-04 BFS preserved, D-15 in-scope fixes, D-17 TS-as-spec, D-18 Red stubs)
    - 34-RESEARCH.md (Track 2 fix specs, fast-check generator design, allow-list hybrid format)
    - 34-VALIDATION.md (Wave 0 Requirements list)
    - PARITY_STATUS.md (5 catalogued divergences for allow-list keys[])
    - IVM-PORT-AUDIT-DEEP.md (B5/B6/B7/B12 deferred items for allow-list patterns[])
  provides:
    - tools/ivm-parity/parity-allowlist.json — fuzz allow-list (5 keys + 4 patterns)
    - tools/ivm-parity/arb-ast.ts — fast-check arbitrary builder (Wave 0 scaffold)
    - tools/ivm-parity/harness-fuzz.ts — TS↔RS shim stub (dryRun mode)
    - tools/ivm-parity/random-ast-fuzz.ts — fast-check driver script
    - tools/ivm-parity/queries/wave0-stubs/{b1,b2,b3,b11}.json — diff test stubs
    - 4 Rust Red-state #[ignore] tests (one per BLOCKING fix)
    - FUZZ-02 schema (events / big_id_records / event_tags) + seed fixtures
  affects:
    - tools/ivm-parity/zero-schema.ts (additive — 3 new tables, boolean import)
    - tools/ivm-parity/schema.sql (additive — 3 new CREATE TABLE blocks)
    - tools/ivm-parity/seed-extras.sql (additive — 3 new INSERT blocks)
    - tools/ivm-parity/package.json (additive — 3 new scripts, test pipeline integration)
    - packages/zqlite-rs/src/ast_to_config.rs (additive — 2 #[ignore] tests in mod tests)
    - packages/zqlite-rs/src/advance.rs (additive — 1 #[ignore] test in mod tests)
    - packages/zero-ivm-rs/src/exists_op.rs (additive — 1 #[ignore] test in mod tests)
    - packages/zero-ivm-rs/src/take_op.rs (additive — 1 #[ignore] test in mod tests)
tech_stack:
  added: []
  patterns:
    - fast-check `fc.letrec` recursive arbitraries (CD-01)
    - hybrid allow-list (canonical-key keys[] + shape-pattern patterns[]) (CD-04 RESOLVED)
    - source-level Red-state assertion via `include_str!` (Wave 0 D-18 pattern)
    - schema-as-data extension (D-14 — fuzzer re-reads zero-schema.ts at runtime)
key_files:
  created:
    - .planning/phases/34-differential-fuzz-schema-extension/34-01-PLAN.md
    - tools/ivm-parity/arb-ast.ts (333 lines)
    - tools/ivm-parity/harness-fuzz.ts (177 lines)
    - tools/ivm-parity/random-ast-fuzz.ts (228 lines)
    - tools/ivm-parity/parity-allowlist.json (88 lines)
    - tools/ivm-parity/queries/wave0-stubs/b1-skip-after-exists.json
    - tools/ivm-parity/queries/wave0-stubs/b2-or-with-empty-children.json
    - tools/ivm-parity/queries/wave0-stubs/b3-related-with-limit.json
    - tools/ivm-parity/queries/wave0-stubs/b11-cascade-delete.json
  modified:
    - tools/ivm-parity/zero-schema.ts (added events / big_id_records / event_tags)
    - tools/ivm-parity/schema.sql (added 3 CREATE TABLE + indexes for FUZZ-02)
    - tools/ivm-parity/seed-extras.sql (NULL semantics + i64 > 2^53 + DST + jsonb fixtures)
    - tools/ivm-parity/package.json (fuzz-check / fuzz-shrink / fuzz-record scripts)
    - packages/zqlite-rs/src/ast_to_config.rs (B1 + B3 Red-state stubs)
    - packages/zqlite-rs/src/advance.rs (B11 Red-state stub)
    - packages/zero-ivm-rs/src/exists_op.rs (B2 Red-state stub)
    - packages/zero-ivm-rs/src/take_op.rs (B3 secondary-site Red-state stub)
decisions:
  - Plan file (34-01-PLAN.md) authored as Task 0 — was missing from the worktree at executor start; reconstructed inline from VALIDATION Wave 0 Requirements + RESEARCH §Wave 0 Gaps.
  - Red-state stubs use `include_str!` source-level inspection (not runtime AST→config translation) — sidesteps the SchemaCache db-path requirement; Wave 1 will replace with proper behavioral assertions once partition_key threading + set_prev_snapshot napi method exist.
  - Allow-list `keys[]` Wave 0 entries use `canonical_key_hint` (human-readable descriptor); Wave 1 will populate exact runtime canonicalKey by rerunning the corpus. The driver's coarse hint match is documented as a placeholder; pattern[] matching is fully implemented.
  - harness-fuzz.ts ships as a STUB with explicit `dryRun: true` gate. Live mode returns an instructive error. Wave 1 wires the live `subscribeAndHydrate` path from harness-coverage.ts:100-224 (or extracts a shared primitive both files call).
  - arb-ast.ts column selection currently draws columns from any table (not table-local). Smoke runs produce some semantically invalid ASTs; the harness is expected to reject them as `error` outcomes (counted but not failing). Wave 1 tightens the chain to draw column-from-table.
metrics:
  duration_seconds: 1108
  duration_human: ~18 minutes
  completed_date: 2026-04-29
  tasks_completed: 9
  files_created: 9
  files_modified: 8
  total_lines_added: 1683
---

# Phase 34 Plan 01: Wave 0 Scaffolding Summary

**One-liner:** Lay down differential-fuzz infrastructure (fast-check arbitraries, allow-list, batched harness shim, fuzz driver) + FUZZ-02 production-shape schema + Red-state Rust test stubs for the 4 BLOCKING fixes (B1/B2/B3/B11), keeping `ast-fuzz.ts` and `seed.sql` untouched per D-04 + SKILL.md hard rules.

## Objective Recap

Per `34-VALIDATION.md` Wave 0 Requirements, create test fixtures and harness scaffolding so Wave 1 (Track 2 fixes + FUZZ-01 driver build) and Wave 2 (FUZZ-02 schema fuzz) consume a known-good baseline. All scaffolding is additive — no semantic edits to existing TS, no edits to `seed.sql` or `ast-fuzz.ts`.

## What Shipped

### 1. Allow-list config (`tools/ivm-parity/parity-allowlist.json`)
Hybrid format (CD-04 RESOLVED):
- **5 `keys[]`** — exact canonical-key entries for catalogued divergences from `PARITY_STATUS.md`: `seed_18` (duplicate CSQ alias), `fuzz_00132/133` (FlippedJoin OR-branch), `fuzz_00139/140` (scalar EXISTS).
- **4 `patterns[]`** — shape-pattern entries for deferred B-items the random generator will newly surface: B5 (OrExists short-circuit), B6 (EXISTS_LIMIT downgrade), B7 (FlippedJoin), B12 (companion scalar drift).

Each entry has `id`, `reason`, `phase_to_fix`, `tracking` reference (per Manual-Only Verifications: "every entry must have a reason and phase_to_fix").

### 2. fast-check arbitraries (`tools/ivm-parity/arb-ast.ts`, 333 lines)
- `buildArbitraries(zeroSchema)` returns `{ arbAst, tables }`.
- Per-operator arbitraries via `fc.letrec`: `simple`, `csq`, `cond` (with And/Or recursion), `ast`.
- Schema-driven: re-uses the `adaptSchema` pattern from `ast-fuzz.ts:112-166` — additions to `zero-schema.ts` (FUZZ-02 tables) auto-extend the fuzz surface.
- Default fast-check shrinker (CD-01 — researcher decided default is adequate for AST shapes).
- Smoke: `npx tsx arb-ast.ts` → 8 (now 11) tables loaded, 3 sample ASTs printed, exit 0.

### 3. Shared roundtrip primitive (`tools/ivm-parity/harness-fuzz.ts`, 177 lines)
- Exports `astHash`, `rowsHash`, `diffRows`, `runOneAst`.
- `runOneAst({ dryRun: true })` returns synthetic `ok` outcome — Wave 0 smoke verification works without zero-cache binaries.
- `runOneAst({ dryRun: false })` returns instructive error explaining Wave 1 needs to integrate the live path.
- `diffRows(ts, rs)` is real-mode-ready: `{ tables: { onlyTs[], onlyRs[], different[] }, canonicalKey }` where `canonicalKey = sha1(diff)[:16]` — used downstream for allow-list lookup.

### 4. fast-check driver (`tools/ivm-parity/random-ast-fuzz.ts`, 228 lines)
- `fc.assert(fc.asyncProperty(arb.ast, async ast => { ... }))` per iteration.
- Diverges that match `keys[]` (Wave 0 coarse hint match) or `patterns[]` (full implementation) are silently passed.
- Argv: `--dry-run`, `--num-runs N`. Env: `FUZZ_NUM_RUNS`, `FUZZ_SEED`, `FUZZ_VERBOSE`.
- Exit codes: `0` (clean), `1` (unexpected divergences), `2` (config error).
- Smoke: `FUZZ_NUM_RUNS=10 npm run fuzz-check -- --dry-run` → `ok=10 allowed=0 unexpected=0 error=0`.

### 5. npm scripts (`tools/ivm-parity/package.json`)
- New: `fuzz-check`, `fuzz-shrink`, `fuzz-record`.
- `test` script extended: BFS first (`fuzz`), then fast-check (`fuzz-check`) — preserves D-04 ordering.

### 6. FUZZ-02 schema (`tools/ivm-parity/zero-schema.ts` + `schema.sql`)
Three new tables matching `apps/zbugs/shared/schema.ts` density target (D-09):
- **`events`**: jsonb (metadataJson) + timestamptz (occurredAt) + numeric (amount) + bigint (quantity) + boolean (isProcessed) + 3 nullable string FKs (NULL semantics — D-11). GIN index on jsonb.
- **`big_id_records`**: i64 > 2^53 organic surface (D-12 — B8/B9). Self-FK chain.
- **`event_tags`**: compound key (eventId, tagKey) + jsonb (B3 partition-Take coverage).

Zero replicates rich PG types as TEXT in the wire protocol; the `string()` declarations in zero-schema.ts carry the wire-format. Real types live in schema.sql.

### 7. FUZZ-02 seed fixtures (`tools/ivm-parity/seed-extras.sql`)
- 8 events: NULL ≠ NULL pair (`ev-null-1`/`ev-null-2`), NULL in OR, NULL in JOIN keys, DST round-trips (spring 03-08, fall 11-01), jsonb literal-equality fixtures.
- 7 big_id_records: 5 IDs straddling `2^53 = 9007199254740992` (safe-int-max, exact, first-unsafe, second-unsafe, far-unsafe) + 2 NULL-parent orphans for NULL ≠ NULL in JOIN keys.
- 6 event_tags: compound-key partition window with multiple values per event (Take(limit=2) on per-event partition gives meaningful exclusion).

Strictly additive (SKILL.md hard rule #2). All new IDs prefixed `ev-` / `eb-` / `et-`.

### 8. Rust Red-state stubs (D-18)
One `#[ignore]`'d test per BLOCKING fix, asserting CURRENT BROKEN behavior so Wave 1 fix flips green by removing `#[ignore]` AND inverting the assertion.

| Fix | Test | Site | Spec source |
| --- | --- | --- | --- |
| **B1** | `ast_to_config::tests::test_b1_skip_before_conditions` | `ast_to_config.rs:226-238` (Skip after conditions) | TS `builder.ts:302-345` |
| **B2** | `exists_op::tests::test_b2_parent_sizes_real_count` | `exists_op.rs:152` (`.max(1)` poison) | TS `exists.ts` push semantics |
| **B3** | `ast_to_config::tests::test_b3_partition_key_threading` + `take_op::tests::test_b3_partition_state_consistency` | `ast_to_config.rs:240-247` (hard-coded `None`) + `take_op.rs:55-67, 317-329` | TS `builder.ts:626-632` + `take.ts:710-757` |
| **B11** | `advance::tests::test_b11_descendants_from_prev` | `advance.rs:464-477` (reads from `db_path` post-tx) | TS `pipeline-driver.ts:1542-1577` |

Each test uses `include_str!` source inspection (avoids SchemaCache db-path coupling). Each cites the TS spec line in its `#[ignore]` reason and doc comment.

Default `cargo test` runs are GREEN (stubs are skipped). `cargo test -- --ignored` runs the stubs and they PASS against the current broken behavior — exactly as Wave 0 D-18 specifies.

### 9. TS↔RS differential stubs (`tools/ivm-parity/queries/wave0-stubs/`)
Four hand-written ASTs with optional `mutation_block` for push-path divergence:
- **`b1-skip-after-exists.json`** — `messages.where(EXISTS(attachments)).start({...})`
- **`b2-or-with-empty-children.json`** — `channels.where(OR(visibility=public, EXISTS(conversations)))` + add-conversation-to-empty-channel mutation
- **`b3-related-with-limit.json`** — `channels.where(public).related(conversations.related(messages.limit(2)))` + edit/add message mutations
- **`b11-cascade-delete.json`** — `channels(id=b11-test).related(conversations.related(messages))` + cascade-delete-in-tx mutation

Each documents `spec_source`, `expected_status: "broken-pre-wave1"`, `wave1_action` (what to assert post-fix).

## Verification

| Check | Result |
| --- | --- |
| `cd packages/zero-ivm-rs && cargo test --release --quiet --lib` | 191 passed; 0 failed; **2 ignored** (B2 + B3 stubs) |
| `cd packages/zqlite-rs && cargo test --release --quiet --lib` | 129 passed; 0 failed; **4 ignored** (B1 + B3 + B11 stubs + 1 pre-existing) |
| `cargo test -- --ignored test_b1 test_b2 test_b3 test_b11` | 4 passed (Red-state assertions confirm broken behavior) |
| `FUZZ_NUM_RUNS=10 npm run fuzz-check -- --dry-run` | `ok=10 allowed=0 unexpected=0 error=0` |
| `jq '.keys|length, .patterns|length' parity-allowlist.json` | `5, 4` |
| `for f in queries/wave0-stubs/*.json; do jq -e .fix_id "$f"; done` | OK: B1, B2, B3, B11 |
| `npx tsx arb-ast.ts` | 11 tables loaded (8 base + 3 FUZZ-02), 3 sample ASTs |
| `npx tsx harness-fuzz.ts --smoke` | Synthetic ok outcome, exit 0 |

## Commits (atomic, with `--no-verify` per parallel-executor protocol)

| # | Hash | Type | Subject |
| --- | --- | --- | --- |
| 0 | `fa19ff1bf` | docs | plan Wave 0 scaffolding for differential fuzz + B-fix stubs |
| 1 | `905e63f4c` | feat | add parity-allowlist.json — hybrid 5 keys + 4 patterns |
| 2 | `ba672d6ac` | feat | scaffold fast-check arbitraries (arb-ast.ts) |
| 3 | `2acece391` | feat | add harness-fuzz.ts shim — stub primitive for fast-check driver |
| 4 | `15a64432e` | feat | add random-ast-fuzz.ts driver — fast-check + allow-list |
| 5 | `ed34dd4ab` | chore | add fuzz-check / fuzz-shrink / fuzz-record npm scripts |
| 6 | `80453252c` | feat | FUZZ-02 schema additions — events / big_id_records / event_tags |
| 7 | `63c257605` | feat | seed-extras FUZZ-02 fixtures (NULL semantics + i64 > 2^53) |
| 8 | `b4b970719` | test | Red-state stubs for B1 / B2 / B3 / B11 fix sites |
| 9 | `94d23be46` | test | TS↔RS differential test stubs for B1 / B2 / B3 / B11 |

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Plan file 34-01-PLAN.md was missing**
- **Found during:** Initial context load (Step 1 — load_plan).
- **Issue:** The phase directory contained `34-CONTEXT.md`, `34-RESEARCH.md`, `34-VALIDATION.md`, `34-DISCUSSION-LOG.md` but NO `34-01-PLAN.md`. The orchestrator's prompt referenced "Wave 0 scaffolding" matching the VALIDATION doc's Wave 0 list.
- **Fix:** Authored `34-01-PLAN.md` as Task 0 (commit `fa19ff1bf`), reconstructing the 9-task scope from VALIDATION Wave 0 Requirements + RESEARCH §Wave 0 Gaps. Per Rule 3, this is a blocking issue — could not execute a non-existent plan, but had complete unambiguous specs for what needed to ship.
- **Files modified:** `.planning/phases/34-differential-fuzz-schema-extension/34-01-PLAN.md` (NEW).

**2. [Rule 1 - Bug] B11 Red-state stub initially failed**
- **Found during:** Task 8 verification (`cargo test -- --ignored`).
- **Issue:** The B11 stub asserted `!src.contains("pub fn set_prev_snapshot(")` — but `include_str!` reads the full file INCLUDING the assertion's own literal string. So the substring was always present, causing the test to fail with "Found it — Wave 1 fix may have landed without removing the stub."
- **Fix:** Tightened the search to `"#[napi]\n    pub fn set_prev_snapshot"` — matches the actual `#[napi]` attribute pattern, which won't appear in a doc-string literal.
- **Files modified:** `packages/zqlite-rs/src/advance.rs` (revised assertion).
- **Commit:** Folded into commit `b4b970719`.

**3. [Rule 1 - Bug] B3 take_op stub used incorrect MockInput constructor**
- **Found during:** Task 8 compilation.
- **Issue:** Wrote `MockInput { nodes: vec![...] }` — but `take_op.rs::tests::MockInput` uses `Arc<Mutex<Vec<Node>>>` for shared state, exposed via `MockInput::new(vec)`.
- **Fix:** Changed to `MockInput::new(vec![make_node(1), make_node(2)])`.
- **Files modified:** `packages/zero-ivm-rs/src/take_op.rs`.
- **Commit:** Folded into commit `b4b970719`.

### Authentication Gates

None.

### Out-of-Scope Discoveries (deferred)

None observed. The four BLOCKING fix sites (B1/B2/B3/B11) line numbers cited in RESEARCH match current code exactly — Phase 30 audit fixes did not move them. The Wave 0 stubs reference the exact line numbers in the source as of commit `4508c2ed5` (Phase 34 base).

## Known Stubs

The harness-fuzz live mode is intentionally stubbed (Wave 1 fills it in):
| Stub | File | Line | Reason | Resolution |
| --- | --- | --- | --- | --- |
| `runOneAst({dryRun: false})` returns instructive error | `tools/ivm-parity/harness-fuzz.ts` | 137-145 | Wave 0 ships shim shape; Wave 1 wires `subscribeAndHydrate` from `harness-coverage.ts:100-224`. | Phase 34 Wave 1 (Track 1 fuzzer build). |
| Allow-list `keys[]` use `canonical_key_hint` (descriptor) | `tools/ivm-parity/parity-allowlist.json` | 14, 21, 28, 35, 42 | Wave 0 doesn't have runtime canonicalKey for the 5 catalogued divergences (PARITY_STATUS.md doesn't store the post-Phase-34 canonicalKey). Wave 1 will rerun the corpus and populate exact strings. | Phase 34 Wave 1 (when first fast-check live run produces a divergence catalog). |
| `arb-ast.ts` column selection cross-tabulates tables × columns | `tools/ivm-parity/arb-ast.ts` | 196-203 | Some generated ASTs have columns that don't exist on the chosen table (e.g., `participants.where(visibleTo = ...)`). Harness will reject as `error` outcome. | Phase 34 Wave 1 (chain `arbTable.chain(t => arbColumn(t))`). |
| Rust Red-state stubs assert source-level patterns, not runtime config order | All 4 stub tests | various | Wave 0 stubs sidestep the SchemaCache `db_path` requirement that runtime `ast_to_operator_configs` calls would impose. Wave 1 (post-fix) replaces with proper behavioral assertions on the config Vec. | Phase 34 Wave 1 (the fix landings — each plan replaces its stub). |

These stubs do NOT prevent Wave 0 from being complete. Wave 0's purpose is to lay infrastructure; Wave 1 brings the runtime path online. The Wave 1 plans (34-02 through 34-N) will reference these stubs by name and replace them with full assertions as the fixes land.

## Threat Flags

None — Phase 34 changes touch only test infrastructure (`tools/ivm-parity/`) and Rust unit-test modules. No new network surface, no auth path, no schema for production data, no trust-boundary changes. The FUZZ-02 PG schema additions are isolated to the parity test database (port 6434).

## TDD Gate Compliance

Plan type is `scaffolding` (not `tdd`), so RED/GREEN/REFACTOR gate sequence enforcement does not apply. However, the Red-state stub philosophy (D-18) is itself a TDD-like pattern: tests are written FIRST asserting the broken behavior, Wave 1 fixes flip them green by removing `#[ignore]` and inverting the assertion. Each Wave 1 plan that lands a B-fix MUST include the test-flip in the same atomic commit as the source fix.

## Self-Check: PASSED

**Files created (verified via `ls`):**
- `.planning/phases/34-differential-fuzz-schema-extension/34-01-PLAN.md` — FOUND
- `tools/ivm-parity/arb-ast.ts` — FOUND
- `tools/ivm-parity/harness-fuzz.ts` — FOUND
- `tools/ivm-parity/random-ast-fuzz.ts` — FOUND
- `tools/ivm-parity/parity-allowlist.json` — FOUND (5 keys, 4 patterns confirmed via jq)
- `tools/ivm-parity/queries/wave0-stubs/b1-skip-after-exists.json` — FOUND
- `tools/ivm-parity/queries/wave0-stubs/b2-or-with-empty-children.json` — FOUND
- `tools/ivm-parity/queries/wave0-stubs/b3-related-with-limit.json` — FOUND
- `tools/ivm-parity/queries/wave0-stubs/b11-cascade-delete.json` — FOUND

**Files modified (verified via `git diff --stat`):**
- `tools/ivm-parity/zero-schema.ts` (+85 lines)
- `tools/ivm-parity/schema.sql` (+50 lines)
- `tools/ivm-parity/seed-extras.sql` (+55 lines)
- `tools/ivm-parity/package.json` (+4 lines, -1 line)
- `packages/zqlite-rs/src/ast_to_config.rs` (+80 lines, B1+B3 stubs)
- `packages/zqlite-rs/src/advance.rs` (+62 lines, B11 stub)
- `packages/zero-ivm-rs/src/exists_op.rs` (+44 lines, B2 stub)
- `packages/zero-ivm-rs/src/take_op.rs` (+64 lines, B3 secondary-site stub)

**Commits verified via `git log`:**
- `fa19ff1bf` docs(34-01) — FOUND
- `905e63f4c` feat(34-01) parity-allowlist — FOUND
- `ba672d6ac` feat(34-01) arb-ast.ts — FOUND
- `2acece391` feat(34-01) harness-fuzz.ts — FOUND
- `15a64432e` feat(34-01) random-ast-fuzz.ts — FOUND
- `ed34dd4ab` chore(34-01) npm scripts — FOUND
- `80453252c` feat(34-01) FUZZ-02 schema — FOUND
- `63c257605` feat(34-01) seed-extras — FOUND
- `b4b970719` test(34-01) Red-state stubs — FOUND
- `94d23be46` test(34-01) differential stubs — FOUND

All claims verified. No missing files, no missing commits.
