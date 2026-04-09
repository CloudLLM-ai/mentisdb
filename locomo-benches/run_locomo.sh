#!/usr/bin/env bash
# Run the LoCoMo benchmark against a running mentisdbd instance.
#
# Each persona-pair gets its own chain (locomo-TS-itemN) for isolation.
# Vector sidecars are rebuilt after each item's ingestion.
#
# Usage:
#   bash locomo-benches/run_locomo.sh              # full run (~1,986 QA pairs)
#   bash locomo-benches/run_locomo.sh --limit 20   # dev run (first 20 items)
#
# Any unrecognised flag is forwarded verbatim to locomo_bench.py.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TOP_K=10
WORKERS=4
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --workers)  WORKERS="$2"; shift 2 ;;
        --top-k)    TOP_K="$2"; shift 2 ;;
        *)          EXTRA_ARGS+=("$1"); shift ;;
    esac
done

# ---------------------------------------------------------------------------
# Step 1 — check Python deps
# ---------------------------------------------------------------------------
echo "Checking Python dependencies…"
python3 -c "import requests, json, concurrent.futures" 2>/dev/null || {
    echo "Installing missing deps…"
    pip3 install requests datasets
}

# ---------------------------------------------------------------------------
# Step 2 — check mentisdbd is reachable
# ---------------------------------------------------------------------------
echo "Checking mentisdbd at http://127.0.0.1:9472…"
if ! curl -sf http://127.0.0.1:9472/health >/dev/null 2>&1; then
    echo "ERROR: mentisdbd is not running on port 9472."
    echo "Start it with:  mentisdbd &"
    exit 1
fi
echo "mentisdbd is up."

# ---------------------------------------------------------------------------
# Step 3 — run the benchmark
# ---------------------------------------------------------------------------
mkdir -p "$REPO_ROOT/results"
CHAIN="locomo-$(date +%s)"
OUTPUT="$REPO_ROOT/results/locomo-${CHAIN}.json"

echo ""
echo "Chain  : $CHAIN"
echo "Output : $OUTPUT"
echo ""

python3 "$SCRIPT_DIR/locomo_bench.py" \
    --top-k   $TOP_K \
    --chain   "$CHAIN" \
    --workers $WORKERS \
    --output  "$OUTPUT" \
    "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"