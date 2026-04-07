# mentisdb Benchmarks

Retrieval recall benchmarks comparing mentisdb against published results from
MemPalace (the current SOTA on these three evaluations as of April 2026).

## Benchmarks

| Benchmark | Questions | Metric | MemPalace baseline | MemPalace best |
|-----------|-----------|--------|-------------------|----------------|
| LongMemEval | 500 | R@5 | 96.6% (no API) | 100% (+ Haiku rerank) |
| ConvoMem | 75,336 | R@5 overall | 92.9% | — |
| LoCoMo | 1,986 | R@10 | 88.9% (no rerank) | 100% (+ Sonnet, top-50) |

All three measure **retrieval recall** — for each question, does the gold
evidence turn appear in the top-k thoughts returned by the memory system?
Higher is better.

---

## LongMemEval — step by step

### 1. Python dependencies

```bash
pip3 install requests datasets
```

### 2. Download the dataset

```bash
mkdir -p data
wget -P data https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main/longmemeval_oracle.json
```

The file lands at `data/longmemeval_oracle.json` (~10 MB).

### 3. Start mentisdbd

```bash
mentisdbd &
```

### 4. Run the benchmark

Use the provided shell script (handles chain naming and workers for you):

```bash
bash benchmarks/run_longmemeval.sh
```

Or run manually with full control:

```bash
# Dev run — first 50 questions, fast
python3 benchmarks/longmemeval_bench.py \
    --data data/longmemeval_oracle.json \
    --limit 50 \
    --top-k 5 \
    --chain lme-dev-$(date +%s) \
    --workers 4

# Full run — all 500 questions
python3 benchmarks/longmemeval_bench.py \
    --data data/longmemeval_oracle.json \
    --top-k 5 \
    --chain lme-full-$(date +%s) \
    --workers 4 \
    --output results/longmemeval.jsonl
```

**Important:** always use a fresh `--chain` name per run. Re-using a chain
from a previous run double-ingests the sessions and inflates result counts.
The shell script handles this automatically with a timestamp suffix.

**`--workers` note:** 4 is the safe default. Raising it speeds up ingestion
but risks overwhelming the daemon's write queue. If you see 404/500 errors
mid-ingest, reduce workers or restart the daemon and re-run.

### 5. Re-run evaluation without re-ingesting

Once a chain is populated you can skip ingestion and just re-evaluate:

```bash
python3 benchmarks/longmemeval_bench.py \
    --data data/longmemeval_oracle.json \
    --chain lme-full-1234567890 \
    --skip-ingest \
    --top-k 5
```

---

## ConvoMem

```bash
# Quick smoke test — 100 items per category
python3 benchmarks/convomem_bench.py \
    --categories all \
    --limit 100 \
    --top-k 5

# Single category
python3 benchmarks/convomem_bench.py \
    --categories user asst \
    --limit 500 \
    --top-k 5

# Full run (~75k pairs, ~30 min)
python3 benchmarks/convomem_bench.py \
    --top-k 5 \
    --output results/convomem.json
```

Dataset is downloaded automatically from HuggingFace (`Salesforce/ConvoMem`)
on first run and cached locally by the `datasets` library.

---

## LoCoMo

```bash
# Dev run — first 20 persona-pairs
python3 benchmarks/locomo_bench.py \
    --top-k 10 \
    --limit 20 \
    --chain locomo-dev-$(date +%s)

# Full run (~1,986 QA pairs)
python3 benchmarks/locomo_bench.py \
    --top-k 10 \
    --chain locomo-$(date +%s) \
    --output results/locomo.json
```

Dataset is downloaded automatically from HuggingFace (`snap-research/locomo`).

---

## Interpreting scores

- **R@5** — evidence appears in top 5 retrieved thoughts
- **R@10** — evidence appears in top 10 retrieved thoughts

Use `--top-k 5` to compare directly to MemPalace R@5 numbers.
Use `--top-k 10` for LoCoMo to match their honest-baseline comparison.

`--top-k 50` inflates scores artificially (MemPalace's 100% LoCoMo used
top-50 which exceeds the session count for many items — their own
BENCHMARKS.md flags this as a caveat).

---

## Improving scores

mentisdb's `ranked-search` uses hybrid scoring:
- `lexical` — BM25-style term overlap
- `vector` — semantic embedding similarity (requires vector sidecars)
- `graph` — relation-aware traversal bonus
- `recency` — time-based boost

**Largest quality lever:** generate vector sidecar files for benchmark chains.
Without sidecars only lexical + graph scoring fires.

**To simulate LLM reranking** (matches MemPalace's 100% configuration):
use `--top-k 50` and post-process with an LLM before checking R@5.
