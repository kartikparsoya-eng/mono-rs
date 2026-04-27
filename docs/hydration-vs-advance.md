# Hydration vs Advance: How Zero Serves Users

## The Real Flow: User Opens a Slack-like App

### Step 1: User connects → Hydration

When User A opens the app, their Zero client establishes a WebSocket to `zero-cache`. The client subscribes to queries like:

```ts
// User A's queries
z.query.channels.where('visibility', '=', 'public');
z.query.messages.where('channelId', '=', 'general').limit(50);
z.query.participants.where('userId', '=', 'userA');
```

Each user gets their own **ViewSyncer** instance (`view-syncer.ts`) which owns a **PipelineDriver** (`pipeline-driver.ts`). The PipelineDriver calls `addQueries()` which triggers **hydration** — fetching the full initial result set from SQLite.

- **Rust path**: `rustHydrate(dbPath, queriesJson)` — opens SQLite, runs SQL for all queries in parallel via Rayon, returns all matching rows
- **TS path**: builds IVM operator tree, calls `fetch()` down each pipeline

User B connects separately → gets their own ViewSyncer → their own PipelineDriver → their own hydration. **Completely independent.**

### Step 2: Someone posts a message → Advance

User C sends a message in `#general`. This flows:

```
Client C → zero-server (mutation) → PostgreSQL (INSERT)
  → logical replication → zero-cache replicator
  → SQLite snapshot update → ALL ViewSyncers notified
```

Now **every** connected ViewSyncer gets an advance signal. Each PipelineDriver calls `advance()` to determine: "did this PG change affect any of MY user's queries?"

- **Rust advance** (filter-only pipelines): evaluates the new row against each pipeline's WHERE clause. "Does `channelId = 'general'` match User A's `where('channelId', '=', 'general')`?" If yes, emit an ADD change.
- **TS advance** (joins/takes/exists): pushes the change through the IVM operator tree — handles join propagation, LIMIT refetch, EXISTS flips.

User A's PipelineDriver: "Yes, this message matches my query" → sends poke with the new row.
User B's PipelineDriver (watching `#random`): "No match" → no poke sent.

## Architecture Diagram

```
                    ┌─────────────────────────────────┐
                    │         PostgreSQL               │
                    └──────────────┬──────────────────┘
                                   │ replication
                    ┌──────────────▼──────────────────┐
                    │   zero-cache Replicator          │
                    │   (single SQLite replica)        │
                    └──────────────┬──────────────────┘
                                   │ advance signal (shared)
                    ┌──────────────▼──────────────────┐
                    │      Snapshotter (shared)        │
                    │   computes diff: what rows       │
                    │   changed between versions       │
                    └───┬──────────┬──────────┬───────┘
                        │          │          │
                   ┌────▼───┐ ┌───▼────┐ ┌───▼────┐
                   │ VS: A  │ │ VS: B  │ │ VS: C  │
                   │Pipeline│ │Pipeline│ │Pipeline│
                   │Driver  │ │Driver  │ │Driver  │
                   └────┬───┘ └───┬────┘ └───┬────┘
                        │         │          │
                   hydrate:    hydrate:   hydrate:
                   RUST        RUST       RUST
                   advance:    advance:   advance:
                   TS (+Rust   TS (+Rust  TS (+Rust
                   filters)    filters)   filters)
```

## Are hydration and advance independent?

**Independent per-user:**

- Each user has their own ViewSyncer, PipelineDriver, and IVM operator trees
- Hydration is fully independent — User A's queries don't affect User B's
- Advance evaluation is independent — each PipelineDriver checks its own pipelines

**Shared across all users:**

- The SQLite replica (single copy of the data)
- The Snapshotter diff (computed once, consumed by all ViewSyncers)
- `rust_dispatch_poke` — the optimization where Rust evaluates the diff against ALL ViewSyncers' filter pipelines in one batched Rayon call, instead of N separate calls

## Hydration vs Advance comparison

|                   | Hydration                                  | Advance                                              |
| ----------------- | ------------------------------------------ | ---------------------------------------------------- |
| **When**          | User connects or adds a query              | PG transaction committed                             |
| **What**          | Full result set from scratch               | Incremental delta (what changed?)                    |
| **Rust coverage** | All operators (filter, join, take, exists) | Filter-only (joins/takes/exists fall back to TS)     |
| **Frequency**     | Once per query subscription                | Every PG write that touches subscribed tables        |
| **Hot path?**     | Only at connection time                    | Yes — every mutation fans out to all connected users |

## Why this matters at scale

In a 10,000-user Slack:

- **Hydration** happens 10,000 times (once per user connect). Rust parallelism via Rayon gives ~12x speedup over TS for the initial load.
- **Advance** happens on **every single message send** across all 10,000 users' pipelines. That's why the Rust `rust_dispatch_poke` batched fan-out is the critical hot path — it evaluates one PG change against thousands of filter pipelines in parallel.

## Key code locations

- `packages/zero-cache/src/services/view-syncer/view-syncer.ts` — ViewSyncer per-user lifecycle
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts` — hydration (`addQueries`) and advance (`advance`) entry points
- `packages/zero-cache/src/services/view-syncer/pipeline-driver.ts:1430` — `#reevaluateRustAdvance` gating logic (filter-only check)
- `packages/zqlite-rs/src/hydrate.rs` — Rust hydration core
- `packages/zqlite-rs/src/advance.rs` — Rust advance (`rust_fan_out`, `rust_dispatch_poke`)
- `packages/zero-ivm-rs/src/filter.rs` — Rust filter evaluation used by both paths
