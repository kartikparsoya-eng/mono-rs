#!/usr/bin/env bash
# IVM Parity Test — end-to-end script
#
# Starts TS and Rust zero-cache servers, runs the full hydration parity
# sweep (1084 ASTs), and reports results.
#
# Usage:
#   ./run.sh                  # full 1084 corpus
#   ./run.sh 100              # first 100 only
#   ./run.sh 100 1            # 100 queries, parallelism=1 (for debugging crashes)
#
# Prerequisites:
#   - PostgreSQL running on port 6434 (docker: npm run db-up from apps/zbugs)
#   - zero-ivm-rs symlink intact (script fixes it automatically)
#
# The script automatically creates the parity DB and seeds it if needed.
#
# Environment overrides:
#   PARITY_PG_URL   PostgreSQL connection string
#   TS_PORT         TS server port (default: 4858)
#   RS_PORT         RS server port (default: 4868)

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

# ── 1. Fix zero-ivm-rs symlink (breaks after npm run build) ─────────
MONO_ROOT="../.."
if [ ! -f "${MONO_ROOT}/packages/zero/out/zero-ivm-rs/index.js" ]; then
  echo "[parity] fixing zero-ivm-rs symlink..."
  rm -rf "${MONO_ROOT}/packages/zero/out/zero-ivm-rs"
  ln -s ../../../packages/zero-ivm-rs "${MONO_ROOT}/packages/zero/out/zero-ivm-rs"
fi

# ── 2. Check PostgreSQL ──────────────────────────────────────────────
echo "[parity] checking PostgreSQL at ${PG_HOST}:${PG_PORT}..."
if ! psql "${PG_URL_POSTGRES}" -c "SELECT 1" > /dev/null 2>&1; then
  echo "[parity] ERROR: cannot connect to PostgreSQL at ${PG_HOST}:${PG_PORT}"
  echo "         start it with: cd apps/zbugs && npm run db-up"
  exit 1
fi

# ── 3. Create and seed parity DB (idempotent) ────────────────────────
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

# ── 4. Compile schema if needed ──────────────────────────────────────
if [ ! -f schema.json ] || [ zero-schema.ts -nt schema.json ]; then
  echo "[parity] compiling zero-schema.ts -> schema.json..."
  (cd "${MONO_ROOT}" && npx zero-deploy-permissions \
    -p tools/ivm-parity/zero-schema.ts \
    --output-file tools/ivm-parity/schema.json)
fi

# Deploy permissions to the database
echo "[parity] deploying permissions to PG..."
psql "${PG_URL}" -v ON_ERROR_STOP=1 -f schema.json > /dev/null

# ── 5. Start TS server (Rust IVM off, query planner off) ─────────────
echo "[parity] starting TS zero-cache on :${TS_PORT} (Rust OFF, QP OFF)..."
ZERO_DISABLE_RUST_IVM=1 \
ZERO_ENABLE_QUERY_PLANNER=false \
ZERO_PORT=${TS_PORT} \
ZERO_UPSTREAM_DB="${PG_URL}" \
ZERO_REPLICA_FILE=/tmp/ivm-parity-ts.db \
ZERO_SCHEMA_FILE="$(pwd)/schema.json" \
ZERO_APP_ID=parity_ts \
ZERO_NUM_SYNC_WORKERS=2 \
ZERO_ADMIN_PASSWORD=devdev \
ZERO_AUTH_SECRET=devsecret \
  npx zero-cache > /tmp/ivm-parity-ts.log 2>&1 &
TS_PID=$!
echo "[parity]   PID=${TS_PID}  log=/tmp/ivm-parity-ts.log"

# ── 6. Start RS server (Rust IVM on, query planner off) ──────────────
echo "[parity] starting RS zero-cache on :${RS_PORT} (Rust ON, QP OFF)..."
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

# ── 7. Wait for both servers ─────────────────────────────────────────
echo "[parity] waiting for servers to be ready..."
for port in ${TS_PORT} ${RS_PORT}; do
  for i in $(seq 1 30); do
    if curl -s -o /dev/null -w "" "http://localhost:${port}/" 2>/dev/null; then
      echo "[parity]   :${port} ready (${i}s)"
      break
    fi
    if [ $i -eq 30 ]; then
      echo "[parity] ERROR: server on :${port} did not start within 30s"
      echo "         check log: /tmp/ivm-parity-$([ $port = $TS_PORT ] && echo ts || echo rs).log"
      exit 1
    fi
    sleep 1
  done
done

# ── 8. Run parity sweep ──────────────────────────────────────────────
echo ""
echo "[parity] running hydration sweep: ${CORPUS_LIMIT} ASTs, parallelism=${MAX_PARALLEL}"
echo "───────────────────────────────────────────────────────────────────"
CORPUS_LIMIT=${CORPUS_LIMIT} \
MAX_PARALLEL=${MAX_PARALLEL} \
RS_PORT=${RS_PORT} \
  npx tsx harness-coverage.ts
echo "───────────────────────────────────────────────────────────────────"

# ── 9. Summary ────────────────────────────────────────────────────────
echo ""
echo "[parity] results written to:"
echo "         coverage_run.json    (machine-readable)"
echo "         parity_gaps.md       (human-readable divergences)"
echo ""
echo "[parity] logs:"
echo "         tail -f /tmp/ivm-parity-ts.log"
echo "         tail -f /tmp/ivm-parity-rs.log"
