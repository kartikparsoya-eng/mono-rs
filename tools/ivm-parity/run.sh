#!/usr/bin/env bash
# IVM Parity Test — end-to-end script
#
# Compares the REFERENCE TS IVM (from main branch via git worktree) against
# the current branch's Rust-only IVM. This is the definitive correctness
# check: if both produce identical hydration/advance results for 1084 ASTs,
# the Rust IVM is a faithful replacement.
#
# Usage:
#   ./run.sh                  # full 1084 corpus
#   ./run.sh 100              # first 100 only
#   ./run.sh 100 1            # 100 queries, parallelism=1 (for debugging crashes)
#
# Prerequisites:
#   - PostgreSQL running on port 6434 (docker: npm run db-up from apps/zbugs)
#   - 'main' branch exists locally (for TS reference server)
#
# The script automatically creates the parity DB and seeds it if needed.
#
# Environment overrides:
#   PARITY_PG_URL       PostgreSQL connection string
#   TS_PORT             TS server port (default: 4858)
#   RS_PORT             RS server port (default: 4868)
#   TS_REF_BRANCH       Branch for TS reference server (default: main)
#   SKIP_TS_BUILD       Set to 1 to skip rebuilding the TS worktree (reuse previous)

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

CORPUS_LIMIT="${1:-1084}"
MAX_PARALLEL="${2:-4}"
PG_HOST="${PARITY_PG_HOST:-127.0.0.1}"
PG_PORT="${PARITY_PG_PORT:-6434}"
PG_USER="${PARITY_PG_USER:-user}"
PG_PASS="${PARITY_PG_PASS:-password}"
PG_URL="${PARITY_PG_URL:-postgresql://${PG_USER}:${PG_PASS}@${PG_HOST}:${PG_PORT}/parity}"
PG_URL_POSTGRES="postgresql://${PG_USER}:${PG_PASS}@${PG_HOST}:${PG_PORT}/postgres"
TS_PORT="${TS_PORT:-4858}"
RS_PORT="${RS_PORT:-4868}"
TS_ADMIN_PORT=$((TS_PORT + 1))
RS_ADMIN_PORT=$((RS_PORT + 1))
TS_REF_COMMIT="${TS_REF_COMMIT:-HEAD}"
SKIP_TS_BUILD="${SKIP_TS_BUILD:-0}"

MONO_ROOT="../.."
WORKTREE_DIR="/tmp/ivm-parity-ts-ref"

cleanup() {
  echo ""
  echo "[parity] cleaning up..."
  lsof -ti:${TS_PORT},${TS_ADMIN_PORT},${RS_PORT},${RS_ADMIN_PORT} 2>/dev/null | xargs kill -9 2>/dev/null || true
  echo "[parity] done."
}
trap cleanup EXIT

# ── 0. Kill stale servers & clean replica files ──────────────────────
echo "[parity] killing stale servers on ports ${TS_PORT},${RS_PORT}..."
lsof -ti:${TS_PORT},${TS_ADMIN_PORT},${RS_PORT},${RS_ADMIN_PORT} 2>/dev/null | xargs kill -9 2>/dev/null || true
rm -f /tmp/ivm-parity-ts.db* /tmp/ivm-parity-rs.db*
sleep 1

# Drop and recreate CVR/CDB databases for clean state
for DB in parity_cvr_ts parity_cdb_ts parity_cvr_rs parity_cdb_rs; do
  psql "${PG_URL_POSTGRES}" -c "DROP DATABASE IF EXISTS ${DB}" > /dev/null 2>&1 || true
  psql "${PG_URL_POSTGRES}" -c "CREATE DATABASE ${DB}" > /dev/null 2>&1 || true
done

# ── 1. Prepare TS reference server from a committed snapshot ─────────
# We use a detached git worktree so the TS server runs the committed code
# (with TS IVM still intact) while the RS server runs the working tree
# (with Rust-only IVM and uncommitted changes).
RESOLVED_COMMIT=$(cd "${MONO_ROOT}" && git rev-parse "${TS_REF_COMMIT}")
if [ "${SKIP_TS_BUILD}" != "1" ]; then
  echo "[parity] preparing TS reference server from commit ${RESOLVED_COMMIT:0:10}..."

  # Remove stale worktree if it exists
  if [ -d "${WORKTREE_DIR}" ]; then
    echo "[parity]   removing stale worktree..."
    (cd "${MONO_ROOT}" && git worktree remove --force "${WORKTREE_DIR}" 2>/dev/null || true)
    rm -rf "${WORKTREE_DIR}"
  fi

  # Create detached worktree from the reference commit
  echo "[parity]   creating detached worktree at ${WORKTREE_DIR}..."
  (cd "${MONO_ROOT}" && git worktree add --detach "${WORKTREE_DIR}" "${RESOLVED_COMMIT}")

  # Install deps and build only what's needed (skip full dts which may have pre-existing type errors)
  echo "[parity]   installing deps + building native modules (this may take a minute)..."
  (cd "${WORKTREE_DIR}" && npm install && npx turbo run build --filter=zqlite-rs --filter=zero-ivm-rs)

  echo "[parity]   TS reference build ready."
else
  echo "[parity] SKIP_TS_BUILD=1 — reusing existing worktree at ${WORKTREE_DIR}"
  if [ ! -d "${WORKTREE_DIR}" ]; then
    echo "[parity] ERROR: ${WORKTREE_DIR} does not exist. Run without SKIP_TS_BUILD first."
    exit 1
  fi
fi

# ── 2. Fix zero-ivm-rs symlink for RS server (current branch) ───────
if [ ! -f "${MONO_ROOT}/packages/zero/out/zero-ivm-rs/index.js" ]; then
  echo "[parity] fixing zero-ivm-rs symlink..."
  rm -rf "${MONO_ROOT}/packages/zero/out/zero-ivm-rs"
  ln -s ../../../packages/zero-ivm-rs "${MONO_ROOT}/packages/zero/out/zero-ivm-rs"
fi

# ── 3. Check PostgreSQL ──────────────────────────────────────────────
echo "[parity] checking PostgreSQL at ${PG_HOST}:${PG_PORT}..."
if ! psql "${PG_URL_POSTGRES}" -c "SELECT 1" > /dev/null 2>&1; then
  echo "[parity] ERROR: cannot connect to PostgreSQL at ${PG_HOST}:${PG_PORT}"
  echo "         start it with: cd apps/zbugs && npm run db-up"
  exit 1
fi

# ── 4. Create and seed parity DB (idempotent) ────────────────────────
echo "[parity] ensuring parity DB exists and is seeded..."
psql "${PG_URL_POSTGRES}" -c "CREATE DATABASE parity" 2>/dev/null || true

# Check if tables exist — if not, run full migration + seed
TABLE_COUNT=$(psql "${PG_URL}" -t -c "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_name = 'channels'" 2>/dev/null | tr -d ' ')
if [ "${TABLE_COUNT:-0}" = "0" ]; then
  echo "[parity]   seeding: schema.sql + seed.sql + seed-extras.sql..."
  psql "${PG_URL}" -v ON_ERROR_STOP=1 -f schema.sql   > /dev/null
  psql "${PG_URL}" -v ON_ERROR_STOP=1 -f seed.sql     > /dev/null
  psql "${PG_URL}" -v ON_ERROR_STOP=1 -f seed-extras.sql > /dev/null
  echo "[parity]   seeded."
else
  echo "[parity]   parity DB already seeded (channels table exists)."
fi

# ── 5. Compile schema if needed ──────────────────────────────────────
if [ ! -f schema.json ] || [ zero-schema.ts -nt schema.json ]; then
  echo "[parity] compiling zero-schema.ts -> schema.json..."
  (cd "${MONO_ROOT}" && npx zero-deploy-permissions \
    -p tools/ivm-parity/zero-schema.ts \
    --output-file tools/ivm-parity/schema.json)
fi

# Deploy permissions to the database
echo "[parity] deploying permissions to PG..."
psql "${PG_URL}" -v ON_ERROR_STOP=1 -f schema.json > /dev/null

# ── 6. Start TS reference server (from main branch worktree) ─────────
# This runs the OLD code with TS IVM as the reference implementation.
SCHEMA_ABS="$(pwd)/schema.json"
echo "[parity] starting TS zero-cache on :${TS_PORT} (from commit ${RESOLVED_COMMIT:0:10}, TS IVM)..."
(
  cd "${WORKTREE_DIR}"
  export ZERO_DISABLE_RUST_IVM=1
  export ZERO_ENABLE_QUERY_PLANNER=false
  export ZERO_PORT=${TS_PORT}
  export ZERO_UPSTREAM_DB="${PG_URL}"
  export ZERO_REPLICA_FILE=/tmp/ivm-parity-ts.db
  export ZERO_SCHEMA_FILE="${SCHEMA_ABS}"
  export ZERO_APP_ID=parity_ts
  export ZERO_NUM_SYNC_WORKERS=2
  export ZERO_ADMIN_PASSWORD=devdev
  export ZERO_AUTH_SECRET=devsecret
  exec npx tsx packages/zero-cache/src/server/runner/main.ts
) > /tmp/ivm-parity-ts.log 2>&1 &
TS_PID=$!
echo "[parity]   PID=${TS_PID}  log=/tmp/ivm-parity-ts.log"

# ── 7. Start RS server (current branch, Rust-only IVM) ──────────────
echo "[parity] starting RS zero-cache on :${RS_PORT} (current branch, Rust IVM)..."
ZERO_ENABLE_QUERY_PLANNER=false \
ZERO_PORT=${RS_PORT} \
ZERO_UPSTREAM_DB="${PG_URL}" \
ZERO_REPLICA_FILE=/tmp/ivm-parity-rs.db \
ZERO_SCHEMA_FILE="$(pwd)/schema.json" \
ZERO_APP_ID=parity_rs \
ZERO_NUM_SYNC_WORKERS=2 \
ZERO_ADMIN_PASSWORD=devdev \
ZERO_AUTH_SECRET=devsecret \
  npx zero-cache > /tmp/ivm-parity-rs.log 2>&1 &
RS_PID=$!
echo "[parity]   PID=${RS_PID}  log=/tmp/ivm-parity-rs.log"

# ── 8. Wait for both servers ─────────────────────────────────────────
echo "[parity] waiting for servers to be ready..."
for port in ${TS_PORT} ${RS_PORT}; do
  for i in $(seq 1 60); do
    if curl -s -o /dev/null -w "" "http://localhost:${port}/" 2>/dev/null; then
      echo "[parity]   :${port} ready (${i}s)"
      break
    fi
    if [ $i -eq 60 ]; then
      echo "[parity] ERROR: server on :${port} did not start within 60s"
      echo "         check log: /tmp/ivm-parity-$([ $port = $TS_PORT ] && echo ts || echo rs).log"
      exit 1
    fi
    sleep 1
  done
done

# ── 9. Run parity sweep ──────────────────────────────────────────────
echo ""
echo "[parity] running hydration sweep: ${CORPUS_LIMIT} ASTs, parallelism=${MAX_PARALLEL}"
echo "───────────────────────────────────────────────────────────────────"
CORPUS_LIMIT=${CORPUS_LIMIT} \
MAX_PARALLEL=${MAX_PARALLEL} \
RS_PORT=${RS_PORT} \
  npx tsx harness-coverage.ts
echo "───────────────────────────────────────────────────────────────────"

# ── 10. Summary ───────────────────────────────────────────────────────
echo ""
echo "[parity] TS reference: commit ${RESOLVED_COMMIT:0:10} (TS IVM) on :${TS_PORT}"
echo "[parity] RS under test: current branch (Rust IVM) on :${RS_PORT}"
echo ""
echo "[parity] results written to:"
echo "         coverage_run.json    (machine-readable)"
echo "         parity_gaps.md       (human-readable divergences)"
echo ""
echo "[parity] logs:"
echo "         tail -f /tmp/ivm-parity-ts.log"
echo "         tail -f /tmp/ivm-parity-rs.log"
echo ""
echo "[parity] To rerun without rebuilding TS reference:"
echo "         SKIP_TS_BUILD=1 ./run.sh ${CORPUS_LIMIT}"
