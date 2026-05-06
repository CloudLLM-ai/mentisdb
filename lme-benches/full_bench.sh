#!/usr/bin/env bash
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

pkill -f mentisdb 2>/dev/null || true
sleep 1

MENTISDB_VERBOSE=false MENTISDB_THOUGHT_SOUNDS=false ./target/release/mentisdb </dev/null >/tmp/mentisdb.log 2>&1 &
for i in $(seq 1 20); do
    if curl -sf http://127.0.0.1:9472/health >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

CHAIN="lme-$(date +%s)"
echo "Chain: $CHAIN"

python3 lme-benches/longmemeval_bench.py \
    --data data/longmemeval_oracle.json \
    --force-reingest \
    --chain "$CHAIN" \
    --eval-workers 8 \
    --output "results/longmemeval-${CHAIN}.jsonl" \
    2>&1
