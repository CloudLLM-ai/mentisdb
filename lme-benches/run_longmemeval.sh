#!/usr/bin/env bash
# Run the LongMemEval benchmark against a running mentisdbd instance.
#
# Smart chain selection: queries the daemon for all chains starting with "lme",
# picks the one with the most thoughts (i.e. a full ingest), and skips
# ingestion automatically. If no lme-* chain exists, a fresh one is created
# and ingestion runs.
#
# Usage:
#   bash lme-benches/run_longmemeval.sh              # auto-select or create chain
#   bash lme-benches/run_longmemeval.sh --limit 50   # dev run (first 50 questions)
#   bash lme-benches/run_longmemeval.sh --force-reingest   # re-ingest even if chain exists
#
# Any unrecognised flag is forwarded verbatim to longmemeval_bench.py.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_FILE="$REPO_ROOT/data/longmemeval_oracle.json"
DATA_URL="https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json"
WORKERS=4
TOP_K=5
EXTRA_ARGS=()

# ---------------------------------------------------------------------------
# Parse flags — forward unknowns to the Python script
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --workers) WORKERS="$2"; shift 2 ;;
        *)         EXTRA_ARGS+=("$1"); shift ;;
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
# Step 4 — pick the best existing lme-* chain, or create a fresh one
# ---------------------------------------------------------------------------
CHAIN=$(curl -sf http://127.0.0.1:9472/v1/chains | python3 -c "
import sys, json
d = json.load(sys.stdin)
lme = [(c['chain_key'], c.get('thought_count', 0))
       for c in d.get('chains', [])
       if c['chain_key'].startswith('lme')]
if lme:
    best = max(lme, key=lambda x: x[1])
    print(best[0])
" 2>/dev/null || true)

if [[ -n "$CHAIN" ]]; then
    THOUGHT_COUNT=$(curl -sf http://127.0.0.1:9472/v1/chains | python3 -c "
import sys, json
d = json.load(sys.stdin)
for c in d.get('chains', []):
    if c['chain_key'] == '${CHAIN}':
        print(c.get('thought_count', 0))
        break
" 2>/dev/null || echo "?")
    echo "Found lme chain: ${CHAIN} (${THOUGHT_COUNT} thoughts) — ingestion will be skipped."
else
    CHAIN="lme-$(date +%s)"
    echo "No lme-* chain found — will create: ${CHAIN}"
fi

# ---------------------------------------------------------------------------
# Step 5 — run the benchmark
# ---------------------------------------------------------------------------
mkdir -p "$REPO_ROOT/results"
OUTPUT="$REPO_ROOT/results/longmemeval-${CHAIN}.jsonl"

echo ""
echo "Chain : $CHAIN"
echo "Output: $OUTPUT"
echo ""

python3 "$SCRIPT_DIR/longmemeval_bench.py" \
    --data    "$DATA_FILE" \
    --top-k   $TOP_K \
    --chain   "$CHAIN" \
    --workers $WORKERS \
    --output  "$OUTPUT" \
    "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
