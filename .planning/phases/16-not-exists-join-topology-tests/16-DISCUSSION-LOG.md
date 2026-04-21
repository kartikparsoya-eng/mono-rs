# Phase 16: NOT EXISTS & Join Topology Tests - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-04-21
**Phase:** 16-not-exists-join-topology-tests
**Areas discussed:** NOT EXISTS fix approach, Join topology test design, Test file organization

---

## NOT EXISTS Fix Approach

| Option | Description | Selected |
|--------|-------------|----------|
| Encode in name string | Change regex to capture 'not-exists' from operator name | |
| Metadata parameter | Pass existsType as separate field in decorator callback | |

**User's choice:** Encode in name string (initial choice)

**Follow-up:** Discovered that `builder.ts:669` always generates `:exists(rel)` even for NOT EXISTS, and D-44 blocks modifying it. Pivoted to alternative approaches:

| Option | Description | Selected |
|--------|-------------|----------|
| Inspect operator property | Inspect operator's existsType from instance in pipeline-driver.ts | ✓ |
| New delegate callback | Add decorateExistsInput with existsType parameter | |
| Document limitation only | Accept D-44 constraint, test and document | |

**User's choice:** Inspect operator property
**Notes:** Avoids modifying builder.ts (D-44), keeps change contained to pipeline-driver.ts

---

## Join Topology Test Design

| Option | Description | Selected |
|--------|-------------|----------|
| Standard set (3 topologies) | Parent→child→grandchild, sibling joins, parent with exists on child | ✓ |
| Comprehensive (6 topologies) | Above plus self-join, many-to-many, diamond dependency | |
| Focused (2 topologies) | Only parent→child→grandchild and sibling | |

**User's choice:** Standard set (3 topologies)

---

## Test File Organization

| Option | Description | Selected |
|--------|-------------|----------|
| Extend edge-cases file | Add to existing pipeline-driver.edge-cases.test.ts | |
| Separate test files | New pipeline-driver.not-exists.test.ts and pipeline-driver.join-topology.test.ts | ✓ |
| One new file for Phase 16 | Single pipeline-driver.coverage-gaps.test.ts | |

**User's choice:** Separate test files

## Claude's Discretion

- Test data design (schemas, row counts, assertions)
- Whether to add Rust unit tests for NOT EXISTS edge cases

## Deferred Ideas

None
