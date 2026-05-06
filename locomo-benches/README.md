# LoCoMo Benchmark

Tests retrieval recall (R@k) against the **LoCoMo** benchmark — ~1,977 QA pairs
from long social conversations (up to ~300 turns) spanning weeks of interaction.

## Reference scores

| System | R@10 | Notes |
|--------|------|-------|
| MemPalace v5 | 88.9% | Hybrid, no rerank |
| MemPalace v5 + Sonnet | 100.0% R@5 | top-50 retrieval + LLM rerank |

## Quick start

```bash
# Start mentisdb
mentisdb &

# Full run (~1,977 QA pairs)
bash locomo-benches/run_locomo.sh

# Dev run (limited queries per persona)
bash locomo-benches/run_locomo.sh --limit 50

# Custom top-k
bash locomo-benches/run_locomo.sh --top-k 5
```

## Manual control

```bash
# Dev run
python3 locomo-benches/locomo_bench.py \
    --top-k 10 \
    --limit 50

# Full run
python3 locomo-benches/locomo_bench.py \
    --top-k 10 \
    --output results/locomo.json

# Force re-ingest
python3 locomo-benches/locomo_bench.py --top-k 10 --force-reingest
```

## How it works

All persona conversations are ingested into a single chain with:
- `ContinuesFrom` relations linking sequential turns within each session
- Importance weighting: even-indexed turns = 0.8 (speaker_a), odd = 0.2 (speaker_b)
- Turn-index and speaker tags on every thought
- Vector sidecar (fastembed-minilm) built after ingestion

Queries are evaluated via `ranked-search` with graph expansion (depth 3).
Hit = substring containment between gold evidence text and any top-k result.

Question types:
- **single** — single-hop (1 target turn)
- **multi** — multi-hop (2+ target turns)

## Improving scores

See the main `lme-benches/README.md` for scoring signal documentation.
The largest lever is vector sidecars — without them only lexical + graph scoring fires.