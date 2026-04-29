---
phase: 34-differential-fuzz-schema-extension
plan: 04
subsystem: ivm-parity
tags: [phase-34, fuzz, schema, jsonb, timestamptz, numeric, bigint, null-semantics, wave-1, FUZZ-02]
requirements: [FUZZ-02]
dependency_graph:
  requires:
    - 34-01  # Wave 0 scaffolding (schema framework + pattern stubs)
  provides:
    - events / big_id_records / event_tags tables in ivm-parity harness
    - rich-type fixtures (jsonb, timestamptz, numeric, bigint, NULL) for differential fuzz
    - 4 new MUTATIONS entries exercising D-09..D-12 rich-type advancement paths
  affects:
    - tools/ivm-parity/zero-schema.ts
    - tools/ivm-parity/schema.sql
    - tools/ivm-parity/schema.json
    - tools/ivm-parity/seed-extras.sql
    - tools/ivm-parity/harness-advance-coverage.ts
tech_stack:
  added: []
  patterns:
    - "FUZZ-02 -test- infix namespace convention (Phase 34) — distinguishes additive-test fixtures from baseline seed.sql rows so SKILL.md hard rule 2 stays enforceable across multi-author edits"
    - "Zero rich-type encoding: declare JSONB/TIMESTAMPTZ/NUMERIC as string() in zero-schema.ts; PG schema.sql carries the real PG type; replication round-trip preserves precision via text encoding"
    - "Production-shape density per D-09: each rich type appears at least twice on the events table (2x JSONB, 2x TIMESTAMPTZ) so silent stale-value coercion divergences cannot hide behind a single passing column"
key_files:
  created: []
  modified:
    - tools/ivm-parity/zero-schema.ts
    - tools/ivm-parity/schema.sql
    - tools/ivm-parity/schema.json
    - tools/ivm-parity/seed-extras.sql
    - tools/ivm-parity/harness-advance-coverage.ts
decisions:
  - "Reuse `conversations(id)` as the FK target for events.relatedTicketId since the xyne-style ivm-parity schema has no `tickets` table — keeps the column name plan-traceable while using a real existing table for the join. Documented inline in zero-schema.ts."
  - "Use `boolean()` from @rocicorp/zero (per apps/zbugs/shared/schema.ts:30) for events.isProcessed — the existing ivm-parity schema imported only string()/number(), so this is a new import."
  - "Add bigIdRecordRelationships (self-FK parentBigId → bigIdRecord.id) — not in the plan's spec but required to exercise the NULL ≠ NULL self-JOIN path that the D-11 fixtures target. Rule 2 deviation."
  - "Add events.processedAt index (events_processedAt_idx) and events_auditTrail_gin in addition to the plan-required occurredAt and metadataJson indexes — the D-09 production-shape mandate (2x columns of each rich type) means both columns need index coverage to stay representative of zbugs."
metrics:
  duration: "~25 min"
  completed: "2026-04-30T02:32:00+05:30"
---

# Phase 34 Plan 04: FUZZ-02 Schema Extension Summary

Extended the ivm-parity differential fuzz harness with production-shape rich types (D-09) and seed fixtures targeting NULL semantics (D-11) and type-coercion boundaries (D-12). Schema-as-data per D-14 means the AST fuzzer auto-picks up the new tables at runtime — no fuzzer-logic edits needed.

## What Changed

### Tables added to `tools/ivm-parity/zero-schema.ts` and `schema.sql`

| Table             | PK Style       | Rich Types                                                              | Purpose                                                  |
| ----------------- | -------------- | ----------------------------------------------------------------------- | -------------------------------------------------------- |
| `events`          | single (id)    | 2x JSONB (metadataJson, auditTrail), 2x TIMESTAMPTZ (occurredAt, processedAt), NUMERIC(20,6) amount, BIGINT quantity, BOOLEAN, FK→users + FK→conversations | D-09 production-shape rich-type table (mirrors zbugs density) |
| `big_id_records`  | single (id)    | TIMESTAMPTZ createdAt; PK is TEXT but seeded with i64 > 2^53 values     | D-12 B8/B9 organic surface for f64 coercion                |
| `event_tags`      | composite (eventId, tagKey) | NUMERIC(5,4) confidence, JSONB metadataJson                | D-09 partition-Take coverage with composite-PK + jsonb     |

Indexes added: `events_occurredAt_idx DESC`, `events_processedAt_idx`, `events_actorUserId_idx`, `events_metadataJson_gin`, `events_auditTrail_gin`, `big_id_records_parentBigId_idx`, `event_tags_eventId_idx`.

### D-09 Production-Shape Density — Confirmed

`grep -c JSONB tools/ivm-parity/schema.sql` → **7** (events.metadataJson NOT NULL + DEFAULT cast, events.auditTrail, event_tags.metadataJson NOT NULL + DEFAULT cast, plus GIN index references — counts the keyword occurrences).

`grep -c TIMESTAMPTZ tools/ivm-parity/schema.sql` → **5** (events.occurredAt, events.processedAt, big_id_records.createdAt, plus DDL boilerplate).

Both metrics meet the D-09 floor (≥3 each).

### Seed fixtures appended to `tools/ivm-parity/seed-extras.sql`

All test IDs use `-test-` infix per Phase 34 namespace convention.

| Coverage Area              | IDs added                                                                                          | Count | Rationale                                              |
| -------------------------- | -------------------------------------------------------------------------------------------------- | ----- | ------------------------------------------------------ |
| D-11 NULL semantics        | ev-null-test-1, ev-null-test-2, ev-no-ticket-test, ev-pure-or-test-1                                | 4     | NULL ≠ NULL in equi-join; NULL FK; NULL in OR three-valued logic |
| D-12 i64 boundary          | 9007199254740991, 9007199254740992, 9007199254740993, 9007199254740994, 9999999999999999            | 5     | safe-int-max → 2^53 exact → first/second/far-unsafe     |
| D-12 DST round-trip        | ev-dst-spring-test (2026-03-08T06:30Z), ev-dst-fall-test (2026-11-01T05:30Z)                        | 2     | US Eastern spring-forward gap + fall-back fold         |
| D-10 jsonb non-trivial     | ev-jsonb-test-1 (priority/tags array + reviewer/decision audit), ev-jsonb-empty-test                | 2     | Non-trivial vs empty jsonb — pitfall 8 silent coercion |
| D-09 compound-PK + jsonb   | (ev-pure-or-test-1, priority|category) × (ev-jsonb-test-1, priority|category)                       | 4     | Partition-Take with NUMERIC(5,4) confidence + JSONB    |

### MUTATIONS extension — `tools/ivm-parity/harness-advance-coverage.ts`

4 new entries appended to the `MUTATIONS[]` array (positions 17–20):

1. **INSERT events ev-test-1** — populates all D-09 prod-shape columns; FK targets `u1` and `co-1` (real seed IDs).
2. **UPDATE events.metadataJson** — jsonb-edit propagation surface.
3. **UPDATE events.actorUserId NULL→u2 on ev-null-test-1** — NULL semantics edit; explicit `undo` reverts to NULL. Targets the seed-extras-introduced -test- infixed row, so SKILL.md hard rule 2 stays enforced (no baseline seed touched).
4. **INSERT event_tags compound-PK row** — partition-Take with composite key + JSONB.

`cleanupDb` extended with FK-ordered DELETEs (event_tags before events) plus a defensive UPDATE restoring `ev-null-test-1.actorUserId = NULL` in case the per-step undo did not fire mid-batch.

## Verification Results

| Check                                            | Result                                                                                              |
| ------------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| `grep -E "table\\('events'\\)|table\\('big_id_records'\\)|table\\('event_tags'\\)" zero-schema.ts | wc -l` | 3 ≥ 3                                                                                          |
| `grep -c "CREATE TABLE events\|CREATE TABLE big_id_records\|CREATE TABLE event_tags" schema.sql`     | 3 ≥ 3                                                                                          |
| `grep -c JSONB schema.sql`                       | 7 ≥ 3                                                                                              |
| `grep -c TIMESTAMPTZ schema.sql`                 | 5 ≥ 3                                                                                              |
| `grep -q auditTrail schema.sql && grep -q processedAt schema.sql`        | both succeed (D-09 prod-shape)                                                                  |
| `grep -q "PRIMARY KEY (\"eventId\", \"tagKey\")" schema.sql`              | succeeds                                                                                       |
| `grep -c GIN schema.sql` (≥2 required)           | 7 — events_metadataJson_gin + events_auditTrail_gin + DDL keyword refs                            |
| `npm run deploy-schema`                          | exit 0 — schema.json regenerated with 11 tables (8 existing + 3 FUZZ-02)                          |
| `npx tsx --check zero-schema.ts`                 | exit 0                                                                                             |
| `npx tsx --check harness-advance-coverage.ts`    | exit 0                                                                                             |
| Schema runtime load test                         | `Schema has 11 tables: channels, users, participants, conversations, messages, attachments, departments, team_members, events, big_id_records, event_tags` |
| `grep -c "ev-null-test-\|ev-pure-or-test-\|ev-jsonb-test-\|ev-dst-spring-test\|ev-dst-fall-test\|ev-no-ticket-test" seed-extras.sql` | 13 ≥ 5                          |
| `grep -c "9007199254740991\|9007199254740992\|9007199254740993" seed-extras.sql`  | 6 ≥ 3                                                                                          |
| `grep -E "INSERT INTO event_tags" seed-extras.sql | wc -l`         | 1 ≥ 1                                                                                          |
| `grep -c -- "-test- infix" seed-extras.sql`      | 1 ≥ 1                                                                                              |
| `grep -c "insert event ev-test-1\|edit event ev-test-1\|flip events.actorUserId.*ev-null-test-1\|add event_tag" harness-advance-coverage.ts` | 4 ≥ 4 |

### Deferred Verifications (PG not running in worktree)

- **`npm run db-migrate` against fresh parity DB** — Postgres not running on port 6434 in this worktree. SKILL.md prerequisite: `db-up` must be running before sweep. Operator must verify post-merge: `cd tools/ivm-parity && npm run db-migrate` should exit 0.
- **`psql -f seed-extras.sql` INSERT-success check** — same reason. Operator-verifiable on first sweep run.
- **End-to-end `npm run sweep:hydrate` + `sweep:advance`** — requires both caches running; deferred to post-merge sweep.
- **A1 verification (Zero replication of BIGINT > 2^53)** — requires running replicator to observe whether `quantity` lands in SQLite as i64 or string. Will surface organically once 34-07 fuzzer hits the first big_id_records predicate; recorded as Assumption A1 in CONTEXT.

## Replica Wipe Procedure (Operator)

After merging this plan, the existing replica DB files at `/tmp/ivm-parity-*.db` carry the pre-FUZZ-02 schema and the replicator will fail with `relation "events" does not exist` (or column-mismatch) until wiped. Per SKILL.md hard rule 7:

```bash
# 1. Stop both caches.
lsof -i :4858 | awk '/LISTEN/ {print $2}' | xargs -r kill   # TS cache
lsof -i :4868 | awk '/LISTEN/ {print $2}' | xargs -r kill   # RS cache

# 2. Wipe replica DB files.
rm -f /tmp/ivm-parity-ts.db /tmp/ivm-parity-ts.db-shm /tmp/ivm-parity-ts.db-wal
rm -f /tmp/ivm-parity-rs.db /tmp/ivm-parity-rs.db-shm /tmp/ivm-parity-rs.db-wal

# 3. Re-migrate parity PG (idempotent on fresh schema; this run picks
#    up the new events / big_id_records / event_tags DDL + seeds).
cd tools/ivm-parity && npm run db-migrate

# 4. Restart both caches (run.sh handles env vars + ports).
./run.sh ts &   # TS cache on 4858
./run.sh rs &   # RS cache on 4868

# 5. Verify ports.
lsof -i :4858 | grep LISTEN && lsof -i :4868 | grep LISTEN

# 6. Re-run sweep.
npm run sweep:hydrate && npm run sweep:advance
```

Both caches will re-replicate the parity DB into fresh SQLite files using the new schema.

## FK Target Table Names — Confirmed

The ivm-parity schema (xyne-style) uses **plural** PG table names (`users`, `conversations`, `channels`, etc.). The plan referenced `tickets` but the schema has no such table. FK adapted:

- `events.actorUserId` → `users(id)` ✓ (matches plan)
- `events.relatedTicketId` → `conversations(id)` (plan said `tickets(id)` — adapted; column name kept for plan-traceability)
- `event_tags.eventId` → `events(id)` ✓
- `big_id_records.parentBigId` → `big_id_records(id)` (self-FK; matches RESEARCH spec)

Plan's "Adjust FK target table names to match" clause covers this adjustment.

## Decisions Made

1. **`relatedTicketId` FK target → `conversations`.** No `tickets` table exists in the xyne-style schema. `conversations` is the closest analogue (an entity that messages reference). Column name kept to match the plan/research wording.
2. **Added `bigIdRecordRelationships` (self-FK).** The plan only specified eventRelationships and eventTagRelationships. Adding the self-FK on bigIdRecord was necessary to make the D-11 self-JOIN-on-NULL fixture queryable through the IVM pipeline (auto-fix Rule 2 — missing critical functionality for testing the targeted NULL semantics).
3. **Added `events_processedAt_idx` and `events_auditTrail_gin` indexes** — the plan only listed `events_occurredAt_idx` (DESC) and `events_metadataJson_gin`. The D-09 production-shape mandate (2x columns of each rich type) implies both columns need index coverage to stay representative of zbugs density (auto-fix Rule 2).
4. **Permissive `ANYONE_CAN_DO_ANYTHING`** on the 3 new tables, matching the existing pattern. The harness exercises pipelines, not the permission layer.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] FK target table mismatch — `tickets` does not exist**
- **Found during:** Task 1 (writing schema.sql REFERENCES clause)
- **Issue:** Plan and RESEARCH both specify `relatedTicketId TEXT NULL REFERENCES tickets(id)`, but the xyne-style ivm-parity schema has no `tickets` table.
- **Fix:** Repurposed FK to `conversations(id)` (closest existing analogue). Column name kept for plan-traceability. Plan acknowledges this adjustment is allowed: "Adjust if FK targets in Task 1's schema.sql don't exactly match".
- **Files modified:** tools/ivm-parity/schema.sql, tools/ivm-parity/zero-schema.ts (relationship destSchema)
- **Commit:** 565744a71

**2. [Rule 2 - Missing critical functionality] No relationship for big_id_records self-FK**
- **Found during:** Task 1
- **Issue:** Plan listed only eventRelationships and eventTagRelationships. The D-11 self-JOIN-on-NULL fixture (parentBigId NULL on the first row) cannot be reached through the IVM pipeline without a relationship declaration on big_id_records.
- **Fix:** Added `bigIdRecordRelationships` mapping `parent: one(parentBigId → bigIdRecord.id)`.
- **Files modified:** tools/ivm-parity/zero-schema.ts
- **Commit:** 565744a71

**3. [Rule 2 - Missing critical functionality] No index on events.processedAt or events.auditTrail**
- **Found during:** Task 1
- **Issue:** Plan listed indexes on the *first* of each rich type (occurredAt DESC, metadataJson GIN) but not on the second (processedAt, auditTrail). D-09 production-shape mandates both columns be representative of zbugs density — that includes index coverage.
- **Fix:** Added `events_processedAt_idx` btree and `events_auditTrail_gin` GIN.
- **Files modified:** tools/ivm-parity/schema.sql
- **Commit:** 565744a71

### Deferred Items

- **Live PG verification of seed inserts** — PG not running on port 6434 in worktree. Operator must run `npm run db-migrate` post-merge to confirm. Plan Task 2's verify step `psql … -f seed-extras.sql` is therefore tracked as deferred.
- **A1 (BIGINT replication shape) verification** — Will surface organically through 34-07 fuzz batch.

## TDD Gate Compliance

Plan type `execute` (not `tdd`) — no RED/GREEN/REFACTOR commits required.

## Commits

| Task | Commit    | Title                                                                          |
| ---- | --------- | ------------------------------------------------------------------------------ |
| 1    | 565744a71 | feat(34-04): add events / big_id_records / event_tags tables (FUZZ-02)         |
| 2    | 80c5aec37 | feat(34-04): seed FUZZ-02 fixtures (NULL semantics + i64 + DST + jsonb)        |
| 3    | edb47365b | feat(34-04): extend MUTATIONS + cleanupDb for FUZZ-02 tables                   |

## Files Modified

- `tools/ivm-parity/zero-schema.ts` (+98 lines): 3 new tables, 3 new relationship blocks, boolean import, schema/permissions extension.
- `tools/ivm-parity/schema.sql` (+55 lines): 3 new CREATE TABLE statements, 7 new indexes (5 btree + 2 GIN).
- `tools/ivm-parity/schema.json` (regenerated): now contains permissions for events, big_id_records, event_tags.
- `tools/ivm-parity/seed-extras.sql` (+75 lines): 19 new INSERT rows (4 events NULL-semantics, 5 big_id_records, 2 events DST, 2 events jsonb, 4 event_tags, plus the comment block).
- `tools/ivm-parity/harness-advance-coverage.ts` (+44 lines): 4 new MUTATIONS entries, 2 new cleanupDb DELETEs + 1 defensive UPDATE.

## Threat Flags

None — all surface introduced is fixture / schema and stays inside the trust boundaries documented in the plan's `<threat_model>`. Threats T-34-09 through T-34-12 are mitigated as planned (additive-only seed-extras with -test- infix; D-09 prod-shape provides the second-jsonb/second-timestamptz surface for T-34-10; replica wipe procedure documented for T-34-11; -test- namespace prevents T-34-12 collisions).

## Self-Check: PASSED

- [x] `tools/ivm-parity/zero-schema.ts` — modifications confirmed (`grep "table('events')" succeeds`).
- [x] `tools/ivm-parity/schema.sql` — modifications confirmed (`grep "CREATE TABLE events" succeeds`).
- [x] `tools/ivm-parity/schema.json` — regenerated and contains "events", "big_id_records", "event_tags".
- [x] `tools/ivm-parity/seed-extras.sql` — modifications confirmed (`grep "ev-null-test-1" succeeds`).
- [x] `tools/ivm-parity/harness-advance-coverage.ts` — modifications confirmed (`grep "ev-test-1" succeeds`).
- [x] Commit 565744a71 found in `git log --oneline`.
- [x] Commit 80c5aec37 found in `git log --oneline`.
- [x] Commit edb47365b found in `git log --oneline`.
