#!/usr/bin/env bash
# Run the LoCoMo benchmark against a running mentisdb instance.
#
# All persona conversations are ingested into a single chain with
# ContinuesFrom relations and importance weighting. Vector sidecar
# is rebuilt after ingestion.
#
# Usage:
#   bash locomo-benches/run_locomo.sh              # full run (~1,977 queries)
#   bash locomo-benches/run_locomo.sh --limit 50    # dev run
#
# Any unrecognised flag is forwarded verbatim to locomo_bench.py.

set -euo pipefail

# Silence per-thought audio during benchmarks (startup jingle stays on).
export MENTISDB_THOUGHT_SOUNDS=false

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TOP_K=10
WORKERS=4
DATA_DIR="$REPO_ROOT/data"
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --workers)   WORKERS="$2"; shift 2 ;;
        --top-k)     TOP_K="$2"; shift 2 ;;
        --data-dir)  DATA_DIR="$2"; shift 2 ;;
        *)           EXTRA_ARGS+=("$1"); shift ;;
    esac
done

# ---------------------------------------------------------------------------
# Step 1 — check Python deps
# ---------------------------------------------------------------------------
echo "Checking Python dependencies…"
python3 -c "import requests, json, concurrent.futures" 2>/dev/null || {
    echo "Installing missing deps…"
    pip3 install requests
}

# ---------------------------------------------------------------------------
# Step 2 — check mentisdb is reachable
# ---------------------------------------------------------------------------
echo "Checking mentisdb at http://127.0.0.1:9472…"
if ! curl -sf http://127.0.0.1:9472/health >/dev/null 2>&1; then
    echo "ERROR: mentisdb is not running on port 9472."
    echo "Start it with:  mentisdb &"
    exit 1
fi
echo "mentisdb is up."

# ---------------------------------------------------------------------------
# Step 3 — download dataset if not present
# ---------------------------------------------------------------------------
mkdir -p "$DATA_DIR"
for F in locomo_items.jsonl locomo_test.jsonl; do
    if [ ! -f "$DATA_DIR/$F" ]; then
        echo "Downloading $F from HuggingFace…"
        curl -sL "https://huggingface.co/datasets/Nithish2410/benchmark-locomo/resolve/main/$F" \
            -o "$DATA_DIR/$F"
        echo "Saved to $DATA_DIR/$F"
    else
        echo "Dataset file present: $DATA_DIR/$F"
    fi
done

# ---------------------------------------------------------------------------
# Step 4 — run the benchmark
# ---------------------------------------------------------------------------
mkdir -p "$REPO_ROOT/results"
CHAIN="locomo-$(date +%s)"
OUTPUT="$REPO_ROOT/results/locomo-${CHAIN}.json"

echo ""
echo "Chain  : $CHAIN"
echo "Output : $OUTPUT"
echo ""

python3 "$SCRIPT_DIR/locomo_bench.py" \
    --top-k    $TOP_K \
    --chain    "$CHAIN" \
    --workers  $WORKERS \
    --data-dir "$DATA_DIR" \
    --output   "$OUTPUT" \
    "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"