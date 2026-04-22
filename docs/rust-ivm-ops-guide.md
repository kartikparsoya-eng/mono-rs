# Rust IVM Operations Guide

## Prerequisites

- **Rust toolchain** -- stable (edition 2021). Install via [rustup](https://rustup.rs/).
- **Node.js 20+** -- required by napi-rs for native module compilation.
- **SQLite with WAL2** -- the zero-cache replicator uses WAL2 mode; the Rust
  diff reader relies on snapshot isolation via `BEGIN DEFERRED` on two
  read-only connections.
- **Rayon** -- included as a Cargo dependency; no separate install needed.

## Building

```bash
cd packages/zqlite-rs
npm run build
```

This invokes `napi build --release` which produces the platform-specific
`.node` binary (e.g. `zqlite-rs.darwin-arm64.node`). The binary is loaded
at runtime via `require('zqlite-rs')`.

To run Rust tests independently:

```bash
cd packages/zqlite-rs
cargo test
```

## Enabling / Disabling

| Environment Variable    | Value                    | Effect                                                         |
| ----------------------- | ------------------------ | -------------------------------------------------------------- |
| `ZERO_DISABLE_RUST_IVM` | `1`                      | Disables all Rust IVM paths. Pure TypeScript pipeline is used. |
| `ZERO_DISABLE_RUST_IVM` | unset or any other value | Rust IVM enabled (default).                                    |

When enabled, the system still falls back to TypeScript automatically if:

- The native module failed to load.
- Any pipeline in the client group is ineligible for Rust advancement.
- A runtime error occurs during `rust_advance`.

## Monitoring

### Spans and Metrics

- `ivm.advance-time` histogram -- records per-change advancement latency,
  tagged by `table` and `type`. Reported in seconds.
- `ivm.conflict-rows-deleted` counter -- rows removed due to unique-key
  conflicts during advancement.
- Debug-level log lines from `pipeline-driver.ts`:
  - `rust_advance <prev> => <curr>: N changes, M pipelines` -- Rust path entered.
  - `Rust advanced to <version>` -- Rust path completed successfully.
  - `Rust advance failed, falling back to TS: <error>` -- fallback triggered.
  - `Rust advance version mismatch, retrying` -- snapshot race detected.

### Fallback Logging

All fallback events are logged at `warn` level with the reason string.
Search for `falling back to TS` or `ResetPipelinesSignal` in logs to
identify fallback frequency.

## Troubleshooting

| Symptom                                              | Likely Cause                                                                         | Fix                                                                                    |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------- |
| `Rust advance not available` assertion               | Native module not built or not found in `node_modules`.                              | Run `cd packages/zqlite-rs && npm run build`. Verify the `.node` binary exists.        |
| `Failed to parse syncable_tables`                    | Table spec JSON mismatch between TS and Rust.                                        | Ensure `zqlite-rs` is rebuilt after schema changes.                                    |
| `version mismatch: expected X, got Y`                | Snapshot advanced between snapshot open and version check.                           | Automatically retried once. If persistent, check replicator throughput.                |
| `schema-change on table T` (reset)                   | A DDL change was detected in the changelog.                                          | Expected behavior. Pipelines reset and re-hydrate.                                     |
| `truncation on table T` (truncate)                   | A TRUNCATE operation was detected.                                                   | Expected behavior. Pipelines reset and re-hydrate.                                     |
| All queries falling back to TS                       | One or more pipelines have `related`, `limit`, companions, or correlated subqueries. | Review query ASTs. Simplify queries or accept TS performance for those client groups.  |
| `Rust advance failed, falling back to TS` repeatedly | Runtime panic or SQLite error in Rust code.                                          | Check full error message. May indicate corrupt replica or incompatible SQLite version. |

## Rollback

To immediately disable Rust IVM without redeploying:

```bash
export ZERO_DISABLE_RUST_IVM=1
```

Restart the zero-cache process. All IVM advancement will use the
TypeScript path. No data loss or pipeline corruption occurs -- the Rust
path is stateless and the TS path is always available as a fallback.

To re-enable, unset the variable and restart:

```bash
unset ZERO_DISABLE_RUST_IVM
```
