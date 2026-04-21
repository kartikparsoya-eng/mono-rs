# Phase 17: Data Type Diversity Tests - Context

**Gathered:** 2026-04-21
**Status:** Ready for planning

<domain>
## Phase Boundary

Prove the Rust IVM path handles all SQLite/JS value types correctly by testing JSON columns, NULL-heavy datasets, Unicode strings, and boundary numbers. Pure test-writing phase — no production code changes expected.

</domain>

<decisions>
## Implementation Decisions

### Test Structure
- **D-78:** One test file per data type requirement — 4 files total:
  - `pipeline-driver.json.test.ts` (DAT-01)
  - `pipeline-driver.null.test.ts` (DAT-02)
  - `pipeline-driver.unicode.test.ts` (DAT-03)
  - `pipeline-driver.numbers.test.ts` (DAT-04)

### Data Fixtures
- **D-79:** Hybrid approach — simple cases inline in tests, complex edge cases (deeply nested JSON, combining characters, etc.) in a shared fixtures file
- Fixtures file: `pipeline-driver.fixtures.ts` (or similar)

### Rust-vs-TS Comparison
- **D-80:** Dual-path comparison — each test runs BOTH Rust and TS paths (via `ZERO_DISABLE_RUST_IVM` toggle) and asserts outputs are identical
- This catches divergences between the two implementations automatically

### Carried Forward
- **D-35:** No test modifications — new tests in new files only
- **D-44:** No modifications to `packages/zql/`

</decisions>

<canonical_refs>
## Canonical References

No external specs — requirements fully captured in decisions above and REQUIREMENTS.md (DAT-01 through DAT-04).

- `.planning/REQUIREMENTS.md` — DAT-01 to DAT-04 acceptance criteria
- `packages/zero-ivm-rs/src/filter.rs` — Rust filter implementation (handles value comparisons)
- `packages/zero-ivm-rs/src/join.rs` — Rust join implementation (handles join key matching)
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` — existing test patterns to follow

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `pipeline-driver.test.ts` test helpers (createSource, runPipeline setup patterns)
- `pipeline-driver.edge-cases.test.ts` and `pipeline-driver.not-exists.test.ts` as structural models

### Established Patterns
- Tests use vitest with `describe`/`it` blocks
- Pipeline setup: create AST, build pipeline via `buildPipeline()`, push changes, call `advance()`
- ZERO_DISABLE_RUST_IVM env var disables Rust path for comparison

### Integration Points
- Tests exercise the same `advance()` entry point as production
- Value serialization happens at napi boundary (JS→Rust→JS)

</code_context>

<specifics>
## Specific Ideas

- JSON: nested objects, arrays, mixed types within arrays, empty objects/arrays
- NULL: NULL in filter predicates (=, !=, <, >), NULL join keys, NULL sort keys, all-NULL columns
- Unicode: emoji (multi-codepoint like 👨‍👩‍👧‍👦), CJK characters, RTL (Arabic/Hebrew), combining characters (é vs e+combining acute)
- Numbers: MAX_SAFE_INTEGER, MAX_SAFE_INTEGER+1, -0 vs 0, very small floats, integer vs float comparison (1 vs 1.0)

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 17-data-type-diversity-tests*
*Context gathered: 2026-04-21*
