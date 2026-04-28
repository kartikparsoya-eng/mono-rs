# Testing

## Framework

- **Vitest** (4.1.3) — test runner, assertions, mocking
- **Config:** Per-package `vitest.config.ts`

## Test Categories

### Unit Tests (`*.test.ts`)

- Co-located with source files
- Run without external dependencies
- Use `:memory:` SQLite databases
- Example: `packages/zqlite/src/db.test.ts` (183 lines)

### PostgreSQL Integration Tests (`*.pg.test.ts`)

- Require running PostgreSQL instance
- Use `@testcontainers/postgresql` for isolated PG instances
- Multi-version matrix: PG 15, 16, 17, 18
- Vitest project configs: `vitest.config.pg15.ts`, etc.
- Example: `packages/zero-cache/src/services/view-syncer/cvr-store.pg.test.ts`

### Benchmarks

- Vitest bench mode (`vitest bench`)
- Separate config: `vitest.config.bench.ts`
- Location: `packages/zero-cache/bench/`
- Files: `benchmark.ts`, `bench.ts`, `wal-bench.ts`, `wal2-bench.ts`

## Test Patterns

### Setup/Teardown

```typescript
import {afterEach, beforeEach, describe, expect, test} from 'vitest';

describe('feature', () => {
  let db: Database;
  beforeEach(() => {
    db = new Database(lc, ':memory:');
  });
  afterEach(() => {
    db[Symbol.dispose]();
  });
});
```

### LogContext in Tests

```typescript
import {createSilentLogContext} from '../../shared/src/logging-test-utils.ts';
const lc = createSilentLogContext();
```

### Test Utilities

- `packages/shared/src/logging-test-utils.ts` — `createSilentLogContext`, `TestLogSink`
- `packages/zero-cache/src/test/lite.ts` — `DbFile`, `expectTables`
- `packages/zero-cache/src/services/replicator/test-utils.ts` — `fakeReplicator`, `ReplicationMessages`

### Mocking

- `vi.useFakeTimers()` for time-dependent tests
- `vi.fn()` / `vi.spyOn()` for function mocking
- `mockttp` and `nock` for HTTP mocking

### Property-Based Testing

- `fast-check` (^3.18.0) used in some tests

## CI

- Tests run via `vitest run`
- CI-aware timeouts: `const TIMEOUT = (CI ? 2 : 1) * 30_000`
- CI retry: `retry: CI ? 2 : 0`
- Coverage disabled in CI, enabled locally (HTML + Clover reporters)

## Key Test Suites (Rewrite Targets)

| Test File                                                              | Lines | Tests                   |
| ---------------------------------------------------------------------- | ----- | ----------------------- |
| `packages/zqlite/src/db.test.ts`                                       | 183   | Database/Statement API  |
| `packages/zqlite/src/table-source.test.ts`                             | ~500  | TableSource IVM input   |
| `packages/zqlite/src/database-storage.test.ts`                         | ~200  | Storage interface       |
| `packages/zero-cache/src/services/view-syncer/snapshotter.test.ts`     | 705   | Snapshot diff iteration |
| `packages/zero-cache/src/services/replicator/change-processor.test.ts` | ~500  | CDC processing          |
| `packages/zero-cache/src/services/view-syncer/pipeline-driver.test.ts` | ~800  | IVM pipeline            |
| `packages/zero-cache/src/db/statements.test.ts`                        | ~100  | StatementRunner         |
