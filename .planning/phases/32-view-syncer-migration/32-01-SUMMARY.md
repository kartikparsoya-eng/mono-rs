---
phase: 32-view-syncer-migration
plan: 01
subsystem: view-syncer / decoder
tags: [decoder, i64, streaming, regression, bigint, cr-01]
requires:
  - 31-streaming-primitives-and-wrappers (consumer of `decodeAdvanceChunkBuf` shipped in 31-02)
provides:
  - bug-free i64 boundary decoding in `readJsonValue` for both `decodeAdvanceResultBuf` and `decodeAdvanceChunkBuf`
  - 7-test regression contract pinning Number-vs-BigInt type discrimination at the safe-integer boundary
affects:
  - packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts (readJsonValue case 1 rewrite)
  - packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts (+7 tests)
  - packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts (snapshot updated)
tech-stack:
  added: []
  patterns: [magnitude-gate-before-precision-loss]
key-files:
  created: []
  modified:
    - packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts
    - packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts
    - packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts
decisions:
  - 'Symmetric inclusive gate `<= -0x200000` (per RESEARCH §3 P-07) — minor BigInt-vs-Number conservatism at MIN_SAFE_INTEGER itself, but mathematically symmetric with `>= 0x200000` at the positive boundary'
  - 'Removed unused MAX_SAFE_INTEGER / MIN_SAFE_INTEGER module-level constants (oxlint flagged as unused after gate moved to hi-magnitude check)'
  - 'Updated snapshot in pipeline-driver.test.ts (insert test) to reflect MAX_SAFE_INTEGER → Number — corrects the type discontinuity that previously sat exactly at MAX_SAFE_INTEGER'
metrics:
  duration_minutes: 16
  completed_at: 2026-04-29T13:44:05Z
  tasks_completed: 3
  files_modified: 3
requirements_completed: ['CR-01']
---

# Phase 32 Plan 01: CR-01 i64 Decoder Safe-Integer Fix Summary

**One-liner:** Move the safe-integer guard in `readJsonValue` case 1 BEFORE the lossy
`hi * 2^32 + lo` Number multiplication, gating on `hi` magnitude (`>= 0x200000` /
`<= -0x200000`) so the BigInt fallback fires before precision loss can defeat it.

---

## Fix Shape (per CONTEXT D-12..D-13 + RESEARCH §3 P-06/P-07 + §4)

### Before

```typescript
case 1: {
  // i64
  const lo = view.getUint32(offset, true);
  const hi = view.getInt32(offset + 4, true);
  const n = hi * 0x100000000 + lo;   // <-- LOSSY when |hi| >= 2^21
  offset += 8;
  if (n >= MAX_SAFE_INTEGER || n <= MIN_SAFE_INTEGER) {
    return [BigInt(hi) * BigInt(0x100000000) + BigInt(lo >>> 0), offset];
  }
  return [n, offset];
}
```

### After

```typescript
case 1: {
  // i64. Gate on hi magnitude BEFORE Number arithmetic (CR-01 fix per
  // CONTEXT D-12..D-13 + RESEARCH §3 P-06/P-07 + §4). 2^53 = 2^21 * 2^32,
  // so |hi| >= 2^21 (= 0x200000) means the value exceeds MAX_SAFE_INTEGER
  // and JS Number arithmetic loses precision. Construct BigInt directly
  // in that case so the safe-integer guard never operates on a lossy value.
  // Use `<= -0x200000` (inclusive) for symmetry with `>= 0x200000` so that
  // MIN_SAFE_INTEGER - 1 lands in the BigInt branch (mirror of case 2).
  const lo = view.getUint32(offset, true);
  const hi = view.getInt32(offset + 4, true);
  offset += 8;
  if (hi >= 0x200000 || hi <= -0x200000) {
    return [BigInt(hi) * 0x100000000n + BigInt(lo), offset];
  }
  return [hi * 0x100000000 + lo, offset];
}
```

**Why this works:** `2^53 = 2^21 * 2^32`. JS Numbers are exact for integers up to
`2^53 - 1` (`MAX_SAFE_INTEGER`). When `|hi| >= 2^21 = 0x200000`, the magnitude of
`hi * 2^32` already exceeds `2^53`, so the f64 multiplication is lossy. By gating on
`hi` magnitude (which is exact in i32 / Number), we avoid ever producing the lossy
`n` and route directly to BigInt for those cases.

---

## Six Boundary Cases Pinned

| # | Input | Encoding (hi, lo) | Type | Value |
|---|-------|-------------------|------|-------|
| 1 | `Number.MAX_SAFE_INTEGER` | (0x001FFFFF, 0xFFFFFFFF) | Number | `9007199254740991` |
| 2 | `MAX_SAFE_INTEGER + 1 = 2^53` | (0x00200000, 0x00000000) | BigInt | `9007199254740992n` |
| 3 | `i64::MAX` | (0x7FFFFFFF, 0xFFFFFFFF) | BigInt | `9223372036854775807n` |
| 4 | `i64::MIN` | (0x80000000 = -2147483648, 0x00000000) | BigInt | `-9223372036854775808n` |
| 5 | `Number.MIN_SAFE_INTEGER` | (0xFFE00000 = -2097152, 0x00000001) | BigInt | `-9007199254740991n` |
| 6 | `MIN_SAFE_INTEGER - 1 = -2^53` | (0xFFE00000 = -2097152, 0x00000000) | BigInt | `-9007199254740992n` |

**Note on case 5 (Rule 1 deviation):** The plan/RESEARCH had a math error claiming
the encoding was `hi=0xFFE00001 signed=-2097151, lo=1` and the expected return was
Number. Empirical verification (Node `BigInt` math) confirmed the correct two's
complement encoding is `hi=0xFFE00000 signed=-2097152, lo=1` — which lands in the
inclusive `hi <= -0x200000` BigInt branch. The intent of the plan (test the boundary)
is preserved; the encoding and assertion are corrected to actual mathematics. The
buggy encoding decoded to `-9007194959773695` (off by `2^32` from MIN_SAFE_INTEGER).

**Plus 1 parity test** in `decodeAdvanceResultBuf` describe block:
`i64::MAX` → BigInt `9223372036854775807n` (guards the shared `readJsonValue` from
regressing in either decoder path).

---

## Verification Gate Results

| Command | Files | Tests | Result |
|---------|-------|-------|--------|
| `vitest run src/services/view-syncer/decode-advance-buf.test.ts` | 1 | 14 | All pass |
| `vitest run src/services/view-syncer/pipeline-driver src/services/view-syncer/decode-advance-buf` | 14 | 155 | All pass |
| `FUZZ_NUM_RUNS=100 vitest run streaming-vs-buffered-parity.fuzz.test.ts` | 1 | 1 | Pass |
| `cargo test --release --lib -- --test-threads=1` (zqlite-rs) | — | 128 passed, 1 ignored | Matches Phase 31 baseline |
| `cargo test --release --lib -- --test-threads=1` (zero-ivm-rs) | — | 170 passed | Pass |

**Phase 30-05 broader-glob `pipeline-driver.*.test.ts` blocking gate:** zero failed
test files — passed.

---

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Plan/RESEARCH math error: case 5 boundary encoding**
- **Found during:** Task 1/2 execution. Initial RED test of case 5 used the
  plan's specified `hi=0xFFE00001 signed=-2097151, lo=1` encoding, but on the
  buggy code path it returned `-9007194959773695` (lossy Number from
  `-2097151 * 2^32 + 1`), not `MIN_SAFE_INTEGER` as claimed. After applying the
  GREEN fix, that test still failed because the encoding itself was incorrect.
- **Issue:** The plan's `must_haves.truths` and Task 1 `<behavior>` block stated
  `MIN_SAFE_INTEGER` decodes from `hi=0xFFE00001, lo=1`. Two's complement math
  (verified empirically with Node BigInt) shows the correct encoding is
  `hi=0xFFE00000, lo=1`.
- **Fix:** Updated case 5 test to use the correct encoding `hi=0xFFE00000, lo=1`
  with the assertion `BigInt -9007199254740991n`. The symmetric inclusive
  `<= -0x200000` gate (locked by RESEARCH §3 P-07) routes this to BigInt.
- **Files modified:** packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts
- **Commit:** `6b8ee486f` (folded into the GREEN fix commit)

**2. [Rule 2 - Missing critical functionality] Removed unused MAX_SAFE_INTEGER / MIN_SAFE_INTEGER constants**
- **Found during:** Task 2 GREEN.
- **Issue:** oxlint flagged `no-unused-vars` errors (2 errors) after the gate
  moved to `hi`-magnitude check.
- **Fix:** Removed both `const` declarations. Comment text retained for
  documentation context.
- **Files modified:** packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts
- **Commit:** `6b8ee486f` (folded into the GREEN fix commit)

**3. [Rule 1 - Snapshot] Updated `pipeline-driver.test.ts > insert` snapshot**
- **Found during:** Task 3 verification gate. The test inserts
  `BigInt(Number.MAX_SAFE_INTEGER)` into SQLite; the buggy decoder returned
  BigInt `9007199254740991n`. Post-fix, MAX_SAFE_INTEGER routes to the Number
  branch (since `hi=0x001FFFFF` is NOT `>= 0x200000`).
- **Issue:** The inline snapshot expected BigInt; the fix correctly returns Number.
  This is a correctness improvement that moves the type discontinuity from
  MAX_SAFE_INTEGER (incorrect) to MAX_SAFE_INTEGER + 1 = 2^53 (correct boundary).
- **Fix:** Ran `vitest -u` to update the snapshot from `9007199254740991n` to
  `9007199254740991`. In scope (DIRECTLY caused by my CR-01 fix).
- **Files modified:** packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts
- **Commit:** `a8f401b8e` (separate commit so the test/snapshot update is reviewable independently)

### Deferred (out-of-scope pre-existing failures)

These existed BEFORE my changes (verified via `git stash` test):
- `src/db/wal-checkpoint.test.ts > wal_checkpoint with BEGIN IMMEDIATE` — fails on base
- `src/db/wal-checkpoint.test.ts > wal_checkpoint with BEGIN CONCURRENT` — fails on base
- `src/services/replicator/incremental-sync.test.ts > does not notify on incomplete backfills` — fails on base

Not addressed (out of scope per Phase 32-01 boundary). Logged here for visibility.

---

## Confirmation: No Production Code Outside `decode-advance-buf.ts` Modified

Files changed by this plan:
- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` (production fix)
- `packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts` (regression tests)
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` (snapshot follow-on, test-only)

No changes to:
- `pipeline-driver.ts` (Phase 31 surface — frozen per D-22)
- Any Rust crate (`zero-ivm-rs`, `zqlite-rs`)
- `view-syncer.ts` (out of scope; that's 32-02)
- Any schema/protocol/CVR/replicator code

---

## Commits

| Hash | Type | Message |
|------|------|---------|
| `610ccdb90` | test | add CR-01 i64 boundary regression tests |
| `6b8ee486f` | fix | gate i64 safe-integer check on hi magnitude before lossy Number arithmetic (CR-01) |
| `a8f401b8e` | test | update pipeline-driver snapshot for MAX_SAFE_INTEGER → Number after CR-01 fix |

---

## Threat Flags

None — no new external trust surface introduced. T-32-01-01 (Tampering on
`readJsonValue` i64 branch) is now mitigated per its register entry. T-32-01-04
(Repudiation: decoder bug masking incorrect data) is mitigated by the 7 boundary
regression tests pinned in CI.

---

## Self-Check: PASSED

- File `packages/zero-cache/src/services/view-syncer/decode-advance-buf.ts` exists with the new gate (line 275).
- File `packages/zero-cache/src/services/view-syncer/decode-advance-buf.test.ts` exists with 7 new tests (case 1-6 + parity).
- Commit `610ccdb90` exists in `git log`.
- Commit `6b8ee486f` exists in `git log`.
- Commit `a8f401b8e` exists in `git log`.
- All verification gates (decoder tests + pipeline-driver glob + parity fuzz + Rust regression) pass.

---

_Phase: 32-view-syncer-migration_
_Plan completed: 2026-04-29T13:44:05Z_
