#!/usr/bin/env bash
# Run the LongMemEval benchmark against a running mentisdbd instance.
#
# Usage:
#   bash benchmarks/run_longmemeval.sh              # full run, 500 questions
#   bash benchmarks/run_longmemeval.sh --limit 50   # dev run, first 50 questions
#   bash benchmarks/run_longmemeval.sh --workers 8  # faster ingestion (if daemon is stable)
#
# Pass any extra flags — they are forwarded to longmemeval_bench.py.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_FILE="$REPO_ROOT/data/longmemeval_oracle.json"
DATA_URL="https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json"
CHAIN="lme-$(date +%s)"
WORKERS=4
TOP_K=5

# ---------------------------------------------------------------------------
# Step 1 — check Python deps
# ---------------------------------------------------------------------------
echo "Checking Python dependencies…"
python3 -c "import requests, json, concurrent.futures" 2>/dev/null || {
    echo "Installing missing deps…"
    pip3 install requests
}

# ---------------------------------------------------------------------------
# Step 2 — download dataset if not present
# ---------------------------------------------------------------------------
if [ ! -f "$DATA_FILE" ]; then
    echo "Downloading longmemeval_oracle.json…"
    mkdir -p "$REPO_ROOT/data"
    wget -q --show-progress -O "$DATA_FILE" "$DATA_URL" || \
        curl -L --progress-bar -o "$DATA_FILE" "$DATA_URL"
    echo "Saved to $DATA_FILE"
else
    echo "Dataset already present: $DATA_FILE"
fi

# ---------------------------------------------------------------------------
# Step 3 — check mentisdbd is reachable
# ---------------------------------------------------------------------------
echo "Checking mentisdbd at http://127.0.0.1:9472…"
if ! curl -sf http://127.0.0.1:9472/health >/dev/null 2>&1; then
    echo "ERROR: mentisdbd is not running on port 9472."
    echo "Start it with:  mentisdbd &"
    exit 1
fi
echo "mentisdbd is up."

# ---------------------------------------------------------------------------
# Step 4 — run the benchmark
# ---------------------------------------------------------------------------
mkdir -p "$REPO_ROOT/results"
OUTPUT="$REPO_ROOT/results/longmemeval-$CHAIN.jsonl"

echo ""
echo "Chain : $CHAIN"
echo "Output: $OUTPUT"
echo ""

python3 "$SCRIPT_DIR/longmemeval_bench.py" \
    --data "$DATA_FILE" \
    --top-k $TOP_K \
    --chain "$CHAIN" \
    --workers $WORKERS \
    --output "$OUTPUT" \
    "$@"
