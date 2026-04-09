# LoCoMo Benchmark

Tests retrieval recall (R@k) against the **LoCoMo** benchmark — 1,986 QA pairs
from long social conversations (up to ~300 turns) spanning weeks of interaction.

## Reference scores

| System | R@10 | Notes |
|--------|------|-------|
| MemPalace v5 | 88.9% | Hybrid, no rerank |
| MemPalace v5 + Sonnet | 100.0% R@5 | top-50 retrieval + LLM rerank |

## Quick start

```bash
# Start mentisdbd
mentisdbd &

# Full run (~1,986 QA pairs, ~30 min)
bash locomo-benches/run_locomo.sh

# Dev run (first 20 persona-pairs)
bash locomo-benches/run_locomo.sh --limit 20

# Custom top-k
bash locomo-benches/run_locomo.sh --top-k 5
```

## Manual control

```bash
pip install datasets requests

# Dev run
python3 locomo-benches/locomo_bench.py \
    --top-k 10 \
    --limit 20 \
    --chain locomo-dev-$(date +%s)

# Full run
python3 locomo-benches/locomo_bench.py \
    --top-k 10 \
    --chain locomo-$(date +%s) \
    --output results/locomo.json
```

## How it works

Each LoCoMo item is a multi-session persona-pair conversation. The benchmark:

1. **Ingests** the full conversation into a fresh chain per item, with:
   - `ContinuesFrom` relations linking sequential turns
   - Importance weighting: user turns = 0.8, assistant turns = 0.2
   - Speaker and turn-index tags for every thought

2. **Queries** each QA pair via `ranked-search` with graph expansion (depth 3)

3. **Evaluates** substring-containment hit: does the gold evidence text appear
   in any of the top-k retrieved thoughts?

   - `single` / `multi`: correct if evidence found
   - `adv`: correct if evidence NOT found (should not hallucinate)
   - `summary`: skipped (generation, not retrieval)

4. **Reports** R@k plus near-miss analysis (R@10, R@20, R@50) and score
   breakdowns to identify whether misses are ranking problems or lexical gaps.

## Improving scores

See the main `lme-benches/README.md` for scoring signal documentation.
The largest lever is vector sidecars — without them only lexical + graph scoring fires.