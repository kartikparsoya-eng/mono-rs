#!/usr/bin/env bash
set -euo pipefail

#=============================================================================
# build-zero-cache.sh — Build a zero-cache Docker image from mono-rs source
#
# This script:
#   1. Builds JS packages (npm run build)
#   2. Stages the Docker build context (.docker-ctx/) with JS output + NAPI module stubs
#   3. Stages patched SQLite sources (Rocicorp fork with BEGIN CONCURRENT)
#   4. Builds the Docker image (Rust cross-compile + slim runtime)
#
# Prerequisites:
#   - Docker Desktop running
#   - npm install already done
#   - At least one prior: cd packages/zqlite-rs && cargo fetch
#     (to populate the cargo registry with patched SQLite sources)
#
# Usage:
#   bash scripts/build-zero-cache.sh [OPTIONS]
#
# Options:
#   --skip-js-build     Skip npm run build (use existing JS output)
#   --skip-rust-build   Skip Rust cross-compile (reuse cached .node from last build)
#   --tag=NAME          Docker image tag (default: rocicorp/zero:local)
#   --no-cache          Pass --no-cache to docker build
#   --help              Show this help
#
# Examples:
#   bash scripts/build-zero-cache.sh                          # full build
#   bash scripts/build-zero-cache.sh --skip-js-build          # rust + docker only
#   bash scripts/build-zero-cache.sh --tag=myapp/zero:v1      # custom tag
#=============================================================================

MONO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ZERO_PKG="$MONO_ROOT/packages/zero"
DOCKER_CTX="$MONO_ROOT/.docker-ctx"
IMAGE_TAG="rocicorp/zero:local"
SKIP_JS_BUILD=false
SKIP_RUST_BUILD=false
DOCKER_NO_CACHE=""

for arg in "$@"; do
  case $arg in
    --skip-js-build)   SKIP_JS_BUILD=true ;;
    --skip-rust-build) SKIP_RUST_BUILD=true ;;
    --tag=*)           IMAGE_TAG="${arg#--tag=}" ;;
    --no-cache)        DOCKER_NO_CACHE="--no-cache" ;;
    --help)
      sed -n '3,/^#=====/p' "${BASH_SOURCE[0]}" | head -n -1
      exit 0
      ;;
    *)
      echo "Unknown option: $arg (use --help for usage)"
      exit 1
      ;;
  esac
done

echo "============================================"
echo "  mono-rs zero-cache Docker builder"
echo "============================================"
echo ""
echo "  Repo:    $MONO_ROOT"
echo "  Image:   $IMAGE_TAG"
echo ""

# ── Step 1: Prerequisites ────────────────────────────────────────────
echo "▶ [1/5] Checking prerequisites..."

if ! docker info > /dev/null 2>&1; then
  echo "ERROR: Docker is not running. Start Docker Desktop first."
  exit 1
fi

if [ ! -d "$MONO_ROOT/packages/zqlite-rs" ]; then
  echo "ERROR: packages/zqlite-rs not found"
  exit 1
fi

echo "  ✓ Prerequisites OK"
echo ""

# ── Step 2: Build JS ─────────────────────────────────────────────────
if [ "$SKIP_JS_BUILD" = false ]; then
  echo "▶ [2/5] Building JS (npm run build)..."
  cd "$MONO_ROOT"
  npm run build 2>&1 | tail -5
  echo "  ✓ JS build complete"
else
  echo "▶ [2/5] Skipping JS build"
fi

if [ ! -f "$ZERO_PKG/out/zero/src/cli.js" ]; then
  echo "ERROR: JS not built. Run: npm run build"
  exit 1
fi
echo ""

# ── Step 3: Stage Docker build context ───────────────────────────────
echo "▶ [3/5] Staging Docker build context..."

# Clean and recreate
rm -rf "$DOCKER_CTX"
mkdir -p "$DOCKER_CTX"

# Copy JS output
cp -r "$ZERO_PKG/out" "$DOCKER_CTX/out"

# Stage zqlite-rs NAPI module (index.js + package.json)
ZQLITE_MOD="$DOCKER_CTX/node_modules_extra/zqlite-rs"
mkdir -p "$ZQLITE_MOD"
cp "$MONO_ROOT/packages/zqlite-rs/index.js" "$ZQLITE_MOD/"
cp "$MONO_ROOT/packages/zqlite-rs/index.d.ts" "$ZQLITE_MOD/" 2>/dev/null || true
echo '{"name":"zqlite-rs","version":"0.0.0","main":"index.js","type":"commonjs"}' > "$ZQLITE_MOD/package.json"

# Stage zero-ivm-rs NAPI module (index.js + ts/ + package.json)
IVM_MOD="$DOCKER_CTX/node_modules_extra/zero-ivm-rs"
mkdir -p "$IVM_MOD"
cp "$MONO_ROOT/packages/zero-ivm-rs/index.js" "$IVM_MOD/"
cp "$MONO_ROOT/packages/zero-ivm-rs/index.d.ts" "$IVM_MOD/" 2>/dev/null || true
if [ -d "$MONO_ROOT/packages/zero-ivm-rs/ts" ]; then
  cp -r "$MONO_ROOT/packages/zero-ivm-rs/ts" "$IVM_MOD/ts"
fi
# Replace .ts source with hand-stripped JS (Node 22 can't natively import .ts in container)
cat > "$IVM_MOD/ts/rust-take-storage.ts" << 'STRIPPED_JS'
export class RustTakeStorage {
  #store;
  #prefix;
  constructor(store, opID) {
    this.#store = store;
    this.#prefix = String(opID);
  }
  get(key, def) {
    const json = this.#store.get(this.#prefix + ':' + key);
    if (json === null || json === undefined) return def;
    return JSON.parse(json);
  }
  set(key, value) {
    this.#store.set(this.#prefix + ':' + key, JSON.stringify(value));
  }
  del(key) {
    this.#store.del(this.#prefix + ':' + key);
  }
  *scan(options) {
    const fullPrefix = this.#prefix + ':' + (options?.prefix ?? '');
    const results = this.#store.scan(fullPrefix);
    for (const pair of results) {
      const rawKey = pair[0];
      const key = rawKey.slice(this.#prefix.length + 1);
      yield [key, JSON.parse(pair[1])];
    }
  }
}
STRIPPED_JS
echo '{"name":"zero-ivm-rs","version":"0.0.0","main":"index.js","type":"commonjs"}' > "$IVM_MOD/package.json"
echo '{"type":"module"}' > "$IVM_MOD/ts/package.json"

# Strip workspace-internal deps from package.json
node -e '
  const fs = require("fs");
  const p = JSON.parse(fs.readFileSync("'"$ZERO_PKG/package.json"'", "utf8"));
  const deps = p.dependencies || {};
  for (const [k, v] of Object.entries(deps)) {
    if (v === "0.0.0") delete deps[k];
  }
  delete p.devDependencies;
  fs.writeFileSync("'"$DOCKER_CTX/package.json"'", JSON.stringify(p, null, 2));
'

# Stage patched SQLite sources (Rocicorp fork with BEGIN CONCURRENT)
SQLITE_SRC="$HOME/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libsqlite3-sys-0.31.0/sqlite3"
if [ -f "$SQLITE_SRC/sqlite3.c" ]; then
  mkdir -p "$DOCKER_CTX/sqlite3-patch"
  cp "$SQLITE_SRC/sqlite3.c" "$DOCKER_CTX/sqlite3-patch/"
  cp "$SQLITE_SRC/sqlite3.h" "$DOCKER_CTX/sqlite3-patch/"
  echo "  ✓ Patched SQLite sources staged ($(wc -c < "$DOCKER_CTX/sqlite3-patch/sqlite3.c") bytes)"
else
  echo "  ✗ FATAL: No patched SQLite found at $SQLITE_SRC"
  echo "  Run: cd packages/zqlite-rs && cargo fetch"
  exit 1
fi

echo "  ✓ Build context staged at $DOCKER_CTX"
echo ""

# ── Step 4: Handle Rust skip ─────────────────────────────────────────
if [ "$SKIP_RUST_BUILD" = true ]; then
  echo "▶ [4/5] Skipping Rust cross-compile (reusing cached layers)"
  DOCKER_NO_CACHE=""  # must use cache if skipping rust
fi

# ── Step 5: Build Docker image ───────────────────────────────────────
echo "▶ [5/5] Building Docker image: $IMAGE_TAG"
echo "         This takes ~5-10min first build, ~30s cached."
echo ""

cd "$MONO_ROOT"
docker build \
  -f Dockerfile.zero-cache \
  -t "$IMAGE_TAG" \
  $DOCKER_NO_CACHE \
  . 2>&1 | tail -100

echo ""
echo "  ✓ Image built: $IMAGE_TAG"
SIZE=$(docker images "$IMAGE_TAG" --format '{{.Size}}' 2>/dev/null | head -1)
echo "  ✓ Size: $SIZE"
echo ""
echo "============================================"
echo "  Done! Run with:"
echo ""
echo "  docker run --rm -p 4848:4848 \\"
echo "    -e ZERO_UPSTREAM_DB=postgresql://user:pass@host:5432/db \\"
echo "    -e ZERO_CHANGE_DB=postgresql://user:pass@host:5432/db \\"
echo "    $IMAGE_TAG zero-cache-start"
echo "============================================"
