# Conventions

## Code Style
- **Language:** TypeScript with strict settings (`~6.0.2`)
- **Module system:** ESM (`"type": "module"` in all packages)
- **Imports:** Explicit `.ts` extensions in import paths
- **Formatting:** `oxfmt` (not prettier)
- **Linting:** `oxlint` (not eslint)

## Naming
- **Files:** `kebab-case.ts` (e.g., `table-source.ts`, `change-processor.ts`)
- **Classes:** PascalCase (e.g., `Database`, `TableSource`, `Snapshotter`)
- **Interfaces:** PascalCase, no `I` prefix (e.g., `Input`, `Output`, `Storage`)
- **Private fields:** `#field` (native private fields, not `_field`)
- **Constants:** UPPER_SNAKE_CASE (e.g., `AUTO_VACUUM_INCREMENTAL`, `MB`)
- **Functions:** camelCase
- **Test files:** `feature.test.ts` (co-located with source)
- **PG-only tests:** `feature.pg.test.ts`

## Patterns

### Disposable Pattern
Classes implement `Disposable` interface for resource cleanup:
```typescript
export class Database implements Disposable {
  [Symbol.dispose]() { this.#db.close(); }
}
```

### Private Fields
Consistent use of ES2022 `#private` fields (not TypeScript `private`):
```typescript
class Database {
  readonly #db: SQLite3Database.Database;
  readonly #threshold: number;
}
```

### LogContext
`@rocicorp/logger` `LogContext` threaded through constructors:
```typescript
constructor(lc: LogContext, path: string, options?: Options) {
  this.#lc = lc.withContext('class', 'Database').withContext('path', path);
}
```

### OpenTelemetry Tracing
Manual spans for performance-sensitive operations:
```typescript
import {trace} from '@opentelemetry/api';
const tracer = trace.getTracer('view-syncer', version);
```

### Error Classes
Custom error classes extending `Error`:
```typescript
export class DatabaseInitError extends Error { ... }
export class InvalidDiffError extends Error { ... }
```

### Iterator/Generator Pattern
Heavy use of `Symbol.iterator` and generator functions, especially in IVM pipeline and snapshotter diff iteration.

### SQL Tagged Templates
`@databases/sql` for safe SQL construction:
```typescript
import {sql} from '@databases/sql';
```

## Error Handling
- Custom error classes per domain
- `try/catch` with `cause` chaining: `throw new Error(msg, {cause})`
- SQLite errors: `SqliteError` from `@rocicorp/zero-sqlite3`
- Logging via `LogContext` (warn for slow queries, error for failures)
