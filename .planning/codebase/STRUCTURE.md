# Structure

## Repository Layout

```
mono-rs/
├── apps/                    # Application demos/samples
├── packages/                # Core packages (npm workspaces)
│   ├── zero-cache/          # Server-side sync engine (main rewrite target)
│   ├── zqlite/              # SQLite source for ZQL (rewrite target)
│   ├── zql/                 # IVM query library (Phase 2 rewrite target)
│   ├── zero-protocol/       # Wire protocol types
│   ├── zero-client/         # Client-side sync
│   ├── zero-react/          # React bindings
│   ├── zero-solid/          # SolidJS bindings
│   ├── zero-react-native/   # React Native bindings
│   ├── zero-schema/         # Schema definitions
│   ├── zero-types/          # Shared types
│   ├── zero-pg/             # PostgreSQL utilities
│   ├── zero-server/         # Server framework
│   ├── zero-events/         # Event system
│   ├── shared/              # Shared utilities
│   ├── otel/                # OpenTelemetry
│   ├── datadog/             # Datadog integration
│   ├── replicache/          # Legacy Replicache compat
│   ├── analyze-query/       # Query analysis
│   ├── ast-to-zql/          # AST conversion
│   └── zql-benchmarks/      # ZQL benchmarks
├── prod/                    # Production configuration
├── tools/                   # Build/dev tools
├── deps/                    # External dependencies (downloaded)
├── turbo.json               # Turborepo config
└── package.json             # Root workspace config
```

## Key Package: `zqlite` (Rewrite Foundation)

```
packages/zqlite/src/
├── db.ts                    # Database/Statement wrapper (340 lines)
├── db.test.ts               # Tests (183 lines)
├── table-source.ts          # IVM data source (685 lines)
├── table-source.test.ts     # Tests
├── database-storage.ts      # IVM state storage
├── database-storage.test.ts # Tests
├── internal/
│   └── statement-cache.ts   # Prepared statement LRU cache
├── query-builder.ts         # SQL query construction
├── query-delegate.ts        # Query delegation
├── options.ts               # Configuration options
├── explain-queries.ts       # Query EXPLAIN (skip list)
├── sqlite-cost-model.ts     # Cost estimation (skip list)
├── sqlite-stat-fanout.ts    # Statistics (skip list)
├── resolve-scalar-subqueries.ts
├── test/                    # Test utilities
└── mod.ts                   # Module exports
```

## Key Package: `zero-cache` (Services)

```
packages/zero-cache/src/
├── server/runner/main.ts    # Entry point
├── services/
│   ├── view-syncer/         # Per-client IVM (main perf target)
│   │   ├── pipeline-driver.ts      # IVM orchestration (900+ lines)
│   │   ├── snapshotter.ts          # SQLite snapshots (590 lines)
│   │   ├── cvr-store.ts            # CVR persistence (1329 lines)
│   │   ├── row-record-cache.ts     # Write-back cache (445 lines)
│   │   ├── client-handler.ts       # WebSocket handler
│   │   ├── cvr.ts                  # CVR types
│   │   └── schema/                 # View syncer schemas
│   ├── replicator/          # PG → SQLite replication
│   │   ├── change-processor.ts     # CDC writer (934 lines)
│   │   ├── incremental-sync.ts     # Incremental replication
│   │   ├── schema/                 # Replicator schemas
│   │   │   ├── change-log.ts
│   │   │   ├── column-metadata.ts
│   │   │   ├── table-metadata.ts
│   │   │   └── replication-state.ts
│   │   └── write-worker.ts         # Write worker
│   ├── change-source/       # CDC sources
│   │   └── pg/              # PostgreSQL CDC
│   │       └── initial-sync.ts     # Bulk loader
│   ├── change-streamer/     # Change fan-out
│   ├── mutagen/             # Mutation handling
│   └── runner.ts            # Service orchestrator
├── db/
│   ├── statements.ts        # StatementRunner
│   └── statements.test.ts
├── types/
│   └── lite.ts              # SQLite value types
└── bench/                   # Benchmarks
    ├── benchmark.ts
    ├── bench.ts
    ├── wal-bench.ts
    └── wal2-bench.ts
```

## Key Package: `zql` (IVM Operators)

```
packages/zql/src/ivm/
├── operator.ts              # Core interfaces (Input, Output, Storage)
├── join.ts                  # Join operator
├── filter.ts                # Filter operator
├── sort.ts                  # Sort operator
├── take.ts                  # Take/limit operator
├── exists.ts                # Exists subquery operator
├── cap.ts                   # Cap operator
├── fan-in.ts / fan-out.ts   # Fan-in/out operators
├── change.ts                # Change types
├── data.ts                  # Row data types
└── array-view.ts            # Materialized view
```

## Naming Conventions
- Source: `feature.ts`, Tests: `feature.test.ts` (co-located)
- PG-dependent tests: `feature.pg.test.ts`
- Internal modules: `internal/` subdirectory
- Schema files: grouped in `schema/` subdirectories
- Benchmarks: `bench/` directory in package root
