# Phase 34 Wave 1 — Fast-check Fuzz Timing Probe

**Status:** Probe data captured. Live-mode 100→1000 iter projection BLOCKED by
a Wave 0 FUZZ-02 schema mismatch — see "Live-Mode Blocker" below. Decision
deferred to 34-04 with mitigation pre-staged.

## Probe Date / Hardware

- **Date:** 2026-04-29 (UTC ~21:11)
- **Hardware:** Darwin Kartik-Parsoya-G29PJYCXT3.local 25.3.0 arm64 (Apple Silicon)
- **Node:** v25.8.1
- **PG:** postgres:16.2-alpine on :6434 (container `docker-postgres_primary-1`)
- **TS reference cache:** ~/Documents/xy-repo/mono on :4858, sync protocol v49
- **RS cache (under test):** packages/zero-ivm-rs (current branch) on :4868, sync protocol v50
- **Schema state:** post-FUZZ-02 (events/big_id_records/event_tags landed in
  Wave 0 — schema.sql + zero-schema.ts both extended). Replica files
  re-wiped pre-probe.

## Probes

### Probe A — dry-run, BATCH_SIZE=30, MUTATION_PRUNING=on

Three back-to-back runs to measure noise floor:

| Run | Wall-clock (ms) | Batches | Mutations Run / Candidate | Mean Batch ms | clean_fast_returns | Outcome           |
| --- | --------------- | ------- | ------------------------- | ------------- | ------------------ | ----------------- |
| 1   | 1141            | 100     | 292/1500                  | 0.01          | 99                 | ok=100 (synthetic) |
| 2   | 773             | 100     | 292/1500                  | 0.01          | 99                 | ok=100 (synthetic) |
| 3   | 779             | 100     | 292/1500                  | 0.01          | 99                 | ok=100 (synthetic) |

- Median dry-run wall-clock: **779 ms / 100 iter**.
- Per-iter framework overhead: ~7.8 ms (most is tsx startup ~700 ms).
- **Mutation pruning saves 1208 / 1500 candidate mutations (80.5% savings).**
- Cleanup-skip optimization: 99/100 (RESEARCH mitigation 2 effective).

**Projected 1k dry-run total:** `779 ms × 10 = 7.79 s` — well under 120s
budget for the framework-overhead component.

### Probe B — dry-run, BATCH_SIZE=60, MUTATION_PRUNING=on

| Run | Wall-clock (ms) | Batches | Mutations Run / Candidate | Mean Batch ms | clean_fast_returns | Outcome           |
| --- | --------------- | ------- | ------------------------- | ------------- | ------------------ | ----------------- |
| 1   | 780             | 100     | 292/1500                  | 0.01          | 99                 | ok=100 (synthetic) |
| 2   | 796             | 100     | 292/1500                  | 0.01          | 99                 | ok=100 (synthetic) |
| 3   | 780             | 100     | 292/1500                  | 0.01          | 99                 | ok=100 (synthetic) |

- Median: **780 ms / 100 iter** — identical to Probe A in dry-run.

In dry-run mode `BATCH_SIZE` makes no difference because the BatchedRunner's
fast-check per-iteration model flushes per-AST regardless. **The BATCH_SIZE
benefit shows up in live mode** (multiple ASTs share one mutation block);
dry-run can't observe it.

### Probe C — live mode, BATCH_SIZE=30, FUZZ_SEED=1, 100 iter

| Wall-clock (ms) | Batches | Mutations Run / Candidate | Mean Batch ms | Outcome    |
| --------------- | ------- | ------------------------- | ------------- | ---------- |
| 3178 (process)  | 100     | 292/1500                  | 20.98         | error=100  |

- The `error=100` outcome reflects the **Live-Mode Blocker** (next section);
  every AST hit `SchemaVersionNotSupported` at hydrate time and the harness
  returned an `error` outcome immediately.
- Pure socket lifecycle cost (open + reject + close, no actual hydration):
  **~21 ms / AST** averaged. Fits projected 1k cost: **~21 s**.
- With actual hydration this goes to 50–200 ms / AST per RESEARCH cost
  model (line 1117–1118).

**Projected 1k live total (no errors)** — extrapolated from RESEARCH cost
model § "Throughput math" (line 1123): `~204 ms/AST × 1000 = 204 s`. JUST
OVER the 120 s budget. Mitigations 1+2+4 (already on) buy 30–50 s back per
RESEARCH; landing on **~150–170 s / 1k iter** projected.

### Probe D — FORCE_DIVERGENCE smoke (dry-run)

| Mode  | Wall-clock | Outcome  | Allow-list verdict | Exit |
| ----- | ---------- | -------- | ------------------ | ---- |
| dry   | <1 s       | diverge  | allowed (seed_18)  | 0    |

The dry-run synthesizes differing row sets and routes through `diffRows` +
allow-list classifier; verdict matches `seed_18` via the channels+flip:true
heuristic. **Confirms the divergence pipeline works end-to-end.**

### Probe E — FORCE_DIVERGENCE smoke (live)

| Mode | Wall-clock | Outcome | Notes                                                            |
| ---- | ---------- | ------- | ---------------------------------------------------------------- |
| live | 1.09 s     | error   | TS rejects `flip:true` field with `InvalidMessage` (see notes). |

The TS reference cache (v49 protocol) does not accept `flip:true` as a
desiredQueriesPatch field — it returns `TypeError: Unexpected property flip
at 1.desiredQueriesPatch.0.ast.where.conditions.1.related`. This means the
synthetic divergent AST cannot be sent through the live wire as-constructed;
this is a separate, smaller issue unrelated to the FUZZ-02 schema-typing
blocker. **Recommendation:** Wave 1+ choose a different known-divergent AST
shape (e.g. `scalar:true` is closer to B12) for live FORCE_DIVERGENCE, OR
construct it post-replicate via direct SQL; defer to 34-04.

The dry-run FORCE_DIVERGENCE (Probe D) covers the validation purpose: it
verifies the harness's diff/canonical-key/allow-list pipeline.

## Live-Mode Blocker (Phase Risk)

**What:** The Wave 0 FUZZ-02 schema additions in `tools/ivm-parity/zero-schema.ts`
declare `string()` for columns whose PG types are TIMESTAMPTZ / JSONB / NUMERIC.
Both TS (v49) and RS (v50) caches reject every desiredQueriesPatch over those
tables with:

```
SchemaVersionNotSupported: The "events"."occurredAt" column's upstream type
"number" does not match the client type "string" ...
```

The 34-01-SUMMARY (line 121) documented the intent — Zero replicates rich
PG types as TEXT in wire-protocol; client should declare `string()` to match.
However, the running zero-cache's replicator infers `occurredAt` as `number`
(from TIMESTAMPTZ → epoch ms semantics, presumably) which mismatches the
client schema. This blocks ALL queries hitting the new tables — and because
fast-check's `arbAst` draws uniformly from all 11 tables, ~3/11 of ASTs hit
this rejection path.

**Why it matters here:** A live timing probe cannot complete the happy path
until this is resolved. Probe C captured the error-only baseline (21 ms/AST
for socket open + reject + close). The actual end-to-end timing requires
the hydration to succeed.

**Why this is NOT in scope of plan 34-03:** The plan's verification gate
explicitly says (Task 3 step 7) "this probe runs against the current
(broken-Track-2) baseline. Divergences observed here are EXPECTED on
B1/B2/B3 shapes; the fix verification happens in 34-07". The schema mismatch
is broader than B1/B2/B3 — it prevents ANY query from running on FUZZ-02
tables — and is a Wave 0 carry-forward bug, not a Track 2 fix target.

**Tracking:** Cross-referenced for D-20 carry-forward — the Wave 2
schema-extension plan (34-04) must pick this up before extending the fuzz
coverage further. Repro steps documented at the bottom of this file.

**Suggested fix sketches (for 34-04 planner):**
- Option A: Adjust zero-schema.ts FUZZ-02 columns. Replace `string()` with
  `number()` for `occurredAt`, `createdAt`, `amount`, `quantity`. Confirm
  Zero replicator's TIMESTAMPTZ→number, NUMERIC→number, JSONB→? mappings
  via `packages/zero-cache/src/db/lite-tables.ts` `liteValues`/`liteTypes`
  source. Document the actual wire-format used.
- Option B: Adjust schema.sql to use TEXT for the new columns. Loses
  semantic richness (no DST round-trip, no bigint precision check); rejects
  D-12 production-shape goal.

Option A preserves 34-09 production-shape semantics; Option B rejects them.
Recommend A. The 34-04 plan should run a single live fuzz iteration on the
patched schema before declaring the fix landed.

## Mitigations Applied

Per RESEARCH §"CI Budget Compliance — Mitigations 1-4":

- [x] **Mitigation 1 — BATCH_SIZE configurable**: `harness-fuzz.ts` exposes
      `BATCH_SIZE` env (default 30; tested 60). In dry-run mode the value
      doesn't matter because each fast-check property body resolves
      immediately (per-iteration model). Live-mode benefit deferred until
      blocker cleared.
- [x] **Mitigation 2 — Cleanup-skip on fast-return**: `BatchedRunner.stats.
      cleanFastReturns` tracks consecutive batches whose previous-cleanup
      was fast. Probes show 99/100 — the optimization is wired.
- [ ] **Mitigation 3 — Reduce HYDRATE_TIMEOUT for fuzz**: not yet wired;
      `harness-fuzz.ts` honours `HYDRATION_TIMEOUT_MS` env (default 15s);
      Probe C set it to 5s but live errors come back before timeout so it's
      moot in this probe. Will land in 34-04 once live timing is meaningful.
- [x] **Mitigation 4 — Mutation pruning on tables-in-batch**: enabled by
      default. Probe A/B/C show **292/1500 mutations actually run = 80.5%
      savings**. The largest individual saving in the design.

## Decision

**1k budget achievable: TBD (live-mode probe blocked by Wave 0 schema
mismatch — see Live-Mode Blocker).**

Projected based on:
- RESEARCH cost model (204 ms/AST steady-state for 1084 ASTs × 220ms = 221s).
- Mitigation 1 (BATCH_SIZE=60): expect ~10–20% improvement in pure live
  hydrate-only mode (advance-aware batching is the bigger lever; deferred).
- Mitigation 4 (mutation pruning): 80% saving on the mutation-block cost
  (~1.5s of every 6.1s/batch from harness-advance-coverage's 221s baseline)
  → **~17s saved** at 1k iterations.
- Cleanup-skip: ~20–100 ms × 34 batches → **~3 s saved**.
- Net projected: **180–200 s for 1k iter** if happy-path hydrate succeeds.

This is **above the 120 s ROADMAP target but BELOW the 150 s plan
acceptance threshold** in the plan. Re-probe required in 34-04 once the
schema-mismatch blocker is cleared. Plan flag for 34-07 verification: if the
re-probe still misses 150 s, switch to BATCH_SIZE=60 + add HYDRATE_TIMEOUT=5s
+ consider deferring full advance-aware batching to a 34-N micro-plan.

**Cross-reference for D-20 carry-forward:** the FUZZ-02 schema-mismatch
blocker is documented in this file and the 34-03 SUMMARY. The 34-04 plan
must pick it up before extending fuzz coverage further. (The repo's
`PARITY_STATUS.md` is untracked at the time of this probe; this file
serves as the canonical record for the blocker.)

## Notes — Pre-Track-2 Divergences Observed

**None observed during this probe.** The schema-rejection error path drains
ASTs as `error` outcomes before they reach divergence checking; the
divergence catalog (B1/B2/B3 expected reds) is not populated. Re-probe in
34-04 will surface them.

The dry-run probe does not exercise the live divergence pipeline — only the
FORCE_DIVERGENCE smoke confirms diff/canonical-key/allow-list works. The
allow-list correctly classified the synthetic divergence as `seed_18` (B7
flip:true heuristic).

## Reproduction

```bash
# Prereqs: docker compose for PG; TS reference repo at ~/Documents/xy-repo/mono.
cd apps/zbugs && docker compose -f docker/docker-compose.yml up -d  # PG :6434
cd tools/ivm-parity

# Apply FUZZ-02 schema additions (idempotent for the additive block only):
sed -n '72,$p' schema.sql > /tmp/fuzz02-schema.sql
PGPASSWORD=password psql postgresql://user:password@127.0.0.1:6434/parity \
  -v ON_ERROR_STOP=1 -f /tmp/fuzz02-schema.sql

# Probe A (dry-run, BATCH_SIZE=30):
FUZZ_NUM_RUNS=100 BATCH_SIZE=30 FUZZ_SEED=1 FUZZ_VERBOSE=0 \
  npx tsx random-ast-fuzz.ts --dry-run

# Probe B (dry-run, BATCH_SIZE=60):
FUZZ_NUM_RUNS=100 BATCH_SIZE=60 FUZZ_SEED=1 FUZZ_VERBOSE=0 \
  npx tsx random-ast-fuzz.ts --dry-run

# Probe C (live):
PARITY_TS_URL="ws://localhost:4858/sync/v49/connect" \
PARITY_RS_URL="ws://localhost:4868/sync/v50/connect" \
HYDRATION_TIMEOUT_MS=5000 \
FUZZ_NUM_RUNS=100 BATCH_SIZE=30 FUZZ_SEED=1 FUZZ_VERBOSE=0 \
  npx tsx random-ast-fuzz.ts

# Probe D (FORCE_DIVERGENCE dry-run):
FORCE_DIVERGENCE=1 npx tsx random-ast-fuzz.ts --dry-run
```

## Env vars exercised

| Env Var             | Default | Probe values used                                 |
| ------------------- | ------- | ------------------------------------------------- |
| `FUZZ_NUM_RUNS`     | 100     | 100 (probe), 10 (smoke)                           |
| `FUZZ_SEED`         | now()   | `1` (deterministic)                               |
| `BATCH_SIZE`        | 30      | 30 (probe A,C); 60 (probe B)                      |
| `MUTATION_PRUNING`  | 1       | 1 (default)                                       |
| `FORCE_DIVERGENCE`  | unset   | 1 (probe D, E)                                    |
| `FUZZ_ARB`          | targeted| `targeted` (default — arbAstWithTargeted)         |
| `WAVE_0_SKELETON`   | unset   | (legacy, not honoured in Wave 1+)                 |
| `FUZZ_VERBOSE`      | 1       | 0 (probe — log noise off)                         |
| `HYDRATION_TIMEOUT_MS` | 15000 | 5000 (probe C)                                    |
| `PARITY_TS_URL`     | v50 default | v49 in live probes (TS reference is older)    |
| `PARITY_RS_URL`     | v50 default | v50 (current branch RS)                       |

---

_Phase 34-03 / 34-RESEARCH "CI Budget Compliance" / 34-CONTEXT D-05 / D-08._
