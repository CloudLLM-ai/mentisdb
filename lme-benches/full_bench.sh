#!/usr/bin/env bash
# Full LongMemEval run against a daemon this script starts and stops itself.
# It never stops a daemon it did not start: if something already answers on the
# REST port, it refuses instead of killing it.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

REST_PORT="${MENTISDB_REST_PORT:-9472}"
if curl -sf "http://127.0.0.1:${REST_PORT}/health" >/dev/null 2>&1; then
    echo "A MentisDB daemon already answers on port ${REST_PORT}." >&2
    echo "Stop it yourself, or run on free ports, e.g. MENTISDB_REST_PORT=9592 MENTISDB_MCP_PORT=9591." >&2
    exit 1
fi

MENTISDB_VERBOSE=false MENTISDB_THOUGHT_SOUNDS=false ./target/release/mentisdb </dev/null >/tmp/mentisdb.log 2>&1 &
DAEMON_PID=$!
trap 'kill "$DAEMON_PID" 2>/dev/null || true' EXIT
for i in $(seq 1 20); do
    if curl -sf "http://127.0.0.1:${REST_PORT}/health" >/dev/null 2>&1; then
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
    --base-url "http://127.0.0.1:${REST_PORT}" \
    --eval-workers 8 \
    --output "results/longmemeval-${CHAIN}.jsonl" \
    2>&1
