#!/usr/bin/env bash
# Run random-AST fuzz against multiple seeds, aggregate unique canonical keys.
set -uo pipefail

SEEDS=(20260430 1 42 100 7777 314159 2718281 999 1234 56789 11 22 88 555 909090)
ITERS=${FUZZ_NUM_RUNS:-500}
OUT=/tmp/multi-seed-fuzz.log
KEYDB=/tmp/multi-seed-keys.txt
> "$OUT"
> "$KEYDB"

for s in "${SEEDS[@]}"; do
  echo "=== seed=$s ==="
  STDOUT=$(FUZZ_NUM_RUNS=$ITERS FUZZ_SEED=$s \
    PARITY_TS_URL=ws://localhost:4858/sync/v49/connect \
    PARITY_RS_URL=ws://localhost:4868/sync/v50/connect \
    npx tsx random-ast-fuzz.ts 2>&1 || true)
  echo "===== seed=$s =====" >> "$OUT"
  echo "$STDOUT" >> "$OUT"
  echo "$STDOUT" | grep -E "^random-ast-fuzz: ok=" | head -1
  # canonicalKeys discovered this seed (dedup within seed)
  KEYS=$(echo "$STDOUT" | grep -oE "canonicalKey=[a-f0-9]+" | sort -u)
  if [ -n "$KEYS" ]; then
    while IFS= read -r line; do
      key="${line#canonicalKey=}"
      if grep -qx "$key" "$KEYDB" 2>/dev/null; then
        echo "  dup: $key"
      else
        echo "$key" >> "$KEYDB"
        echo "  NEW: $key (first at seed=$s)"
        # Capture the AST for the new key — first matching AST in stdout for this key
        AST_LINE=$(echo "$STDOUT" | grep -B 0 -A 1 "canonicalKey=$key" | grep "^    AST: " | head -1)
        if [ -n "$AST_LINE" ]; then
          echo "$AST_LINE" >> "/tmp/multi-seed-asts.txt"
        fi
      fi
    done <<< "$KEYS"
  fi
done

echo ""
TOTAL=$(wc -l < "$KEYDB" | tr -d ' ')
echo "=== UNIQUE divergence canonicalKeys: $TOTAL ==="
cat "$KEYDB"
echo ""
echo "Sample ASTs for each unique key saved to /tmp/multi-seed-asts.txt"
