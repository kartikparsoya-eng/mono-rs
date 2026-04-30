# Roadmap — zero-cache Rust Rewrite

## Milestones

- ✅ **v1.0 Rust SQLite Foundation** — (shipped 2026-04-20) → [archive](milestones/v1.0-ROADMAP.md)
- ✅ **v2.0 IVM Operators in Rust** — (shipped 2026-04-21) → [archive](milestones/v2.0-ROADMAP.md)
- ✅ **v3.0 Test Coverage & Correctness Hardening** — (shipped 2026-04-21) → [archive](milestones/v3.0-ROADMAP.md)
- ✅ **v4.0 Parallel IVM Runtime** — (shipped 2026-04-29) → [archive](milestones/v4.0-ROADMAP.md)
- ✅ **v5.0 Streaming + Differential Fuzz** — Phases 30-34 (shipped 2026-04-30) → [archive](milestones/v5.0-ROADMAP.md)
- 📋 **v6.0** — TBD (run `/gsd-new-milestone` to kick off)

## Carry-Forward Tech Debt (v6.0 candidates)

Documented in `.planning/v5.0-MILESTONE-AUDIT.md` and `.planning/notes/2026-04-29-phase-34-scope-decision.md`:

### Operator-environment fix (high priority)

- TS-oracle import resolution at `/private/tmp/ivm-parity-ts-ref` — unblocks live 1k fuzz + npm test sweep that Phase 34 left as deferred UAT items.

### Deep-audit deferred fixes (`.planning/IVM-PORT-AUDIT-DEEP.md`)

- **B5** — OrExists.push_child first-branch short-circuit (small surgical fix)
- **B6** — EXISTS_LIMIT downgrade not honored (security-relevant: PERMISSIONS_EXISTS_LIMIT)
- **B7** — `flip:true` CSQ silently treated as regular Exists. Requires implementing `FlippedJoin` + `UnionFanIn` + `UnionFanOut` operators in Rust. **Sized as its own phase** (large; possibly v6.x). Maps to 2 of the 5 catalogued PARITY_STATUS.md divergences (fuzz_00132/133).
- **B12** — Companion scalar `resolved_value` drift causes spurious resets

### Code-review findings (Phase 33 / Phase 34)

- Phase 33 WR-01 — strict-mode parity advance unusable; carry-forward (architectural: real differential parity comes from `tools/ivm-parity/` two-process harness)
- Phase 34 WR-03 — pre-existing `/tmp/rust_ivm_debug.log` debug write in `advance.rs:1510-1518` (should be `cfg!(debug_assertions)` gated)
- Phase 34 WR-01/WR-02 — fuzz arb coverage signal-degraders (`arb-ast.ts` random-table dilution; `BatchedRunner` BATCH_SIZE plumbing dead)

### Process / CI

- Nyquist VALIDATION.md formal close-out for Phases 31-34 (drafts created but never flipped to `nyquist_compliant: true`)
- CI integration of `npm run fuzz-check:gate` (Phase 34 D-22 explicit deferral; revisit when divergence catalog drains)
- 3 pre-existing pipeline-driver.test.ts whereExists+permissions snapshot failures (verified pre-Phase-34 base)

### Catalogued divergences (PARITY_STATUS.md)

- 2× `OR(simple, EXISTS flip:true)` (fuzz_00132/133) — same root as B7
- 2× `scalar:true` EXISTS (fuzz_00139/140) — companion resolution work
- 1× duplicate CSQ alias (seed_18) — won't-fix

### Live verification carry-forward

- Live 1k fast-check fuzz against running caches (after TS-oracle unblocks)
- Full `tools/ivm-parity/` `npm test` sweep
- Phase 33 production-style soak validation

---

## Phases

<details>
<summary>✅ v5.0 Streaming + Differential Fuzz (Phases 30-34) — SHIPPED 2026-04-30</summary>

- [x] Phase 30: Audit Fixes (5/5 plans) — completed 2026-04-29
- [x] Phase 31: Rust Streaming Primitives + TS Wrappers (2/2 plans) — completed 2026-04-29
- [x] Phase 32: View-Syncer Streaming Migration (2/2 plans) — completed 2026-04-29
- [x] Phase 33: Production Hardening + Benchmarks (3/3 plans) — completed 2026-04-29
- [x] Phase 34: Differential Fuzz + Schema Extension (7/7 plans) — completed 2026-04-30

See `.planning/milestones/v5.0-ROADMAP.md` for full details.

</details>

### 📋 v6.0 (Planning)

Use `/gsd-new-milestone` to:

1. Surface and prioritize the carry-forward items above
2. Add new v6.0 capabilities (FlippedJoin operator family is the biggest open item)
3. Generate fresh `REQUIREMENTS.md` and Phase 35+ roadmap

#### Planned Phases (v6.0)

- [ ] **Phase 35 — Pool & Cascade Hardening** — Two design-level workstreams: B10 (`swap_path` retry + unified poison handling across `connection_pool.rs`/`pipeline_manager.rs`/`table_source.rs`) and NEW-1 (prev-snapshot connection pooling — eliminate per-call `Connection::open` in `emit_descendant_removals`; closes NEW-3 + NEW-5 as side effects). Acceptance: ≥2× cascade-delete throughput on 3-level / 100-root bench; zero `.unwrap()` on inner mutexes in production paths; Phase 33 TTFB+MemPeak thresholds carry-forward; Phase 34 B11 tests carry-forward. 4 plans (35-01..04). Plan: `.planning/phases/35-pool-cascade-hardening/`.

---

## Progress

| Milestone | Phases                 | Status   | Completed  |
| --------- | ---------------------- | -------- | ---------- |
| v1.0      | 12                     | Complete | 2026-04-20 |
| v2.0      | 6                      | Complete | 2026-04-21 |
| v3.0      | —                      | Complete | 2026-04-21 |
| v4.0      | 11                     | Complete | 2026-04-29 |
| v5.0      | 5 (30–34)              | Complete | 2026-04-30 |
| v6.0      | TBD (Phase 35 planned) | Planning | —          |
