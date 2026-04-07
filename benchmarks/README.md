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

## Prerequisites

```bash
# 1. mentisdbd must be running
mentisdbd &

# 2. Python deps
pip install requests datasets
```

---

## LongMemEval

```bash
# Download the dataset
git clone https://github.com/xiaowu0162/LongMemEval
cd LongMemEval
# follow their README to download data/longmemeval_oracle.json
cd ..

# Run (fresh chain per run — timestamps avoid re-using stale data)
python benchmarks/longmemeval_bench.py \
    --data LongMemEval/data/longmemeval_oracle.json \
    --top-k 5 \
    --chain longmemeval-$(date +%s) \
    --workers 16

# Dev run (first 50 questions)
python benchmarks/longmemeval_bench.py \
    --data LongMemEval/data/longmemeval_oracle.json \
    --top-k 5 \
    --limit 50 \
    --chain longmemeval-dev

# Skip re-ingestion if chain already populated
python benchmarks/longmemeval_bench.py \
    --data LongMemEval/data/longmemeval_oracle.json \
    --top-k 5 \
    --chain longmemeval-1234567890 \
    --skip-ingest \
    --output results/longmemeval.jsonl
```

**How it works:**
Each haystack session turn is stored as an `Observation` thought tagged with
`session:{id}` and `role:{user|assistant}`. For each question, `ranked-search`
is called with the question text and top-k thoughts are checked for the gold
evidence turn via substring match.

---

## ConvoMem

```bash
# Full run (75k pairs, ~30 min)
python benchmarks/convomem_bench.py \
    --top-k 5 \
    --output results/convomem.json

# Single category, 500 items
python benchmarks/convomem_bench.py \
    --categories user asst \
    --limit 500 \
    --top-k 5

# All categories, limited (quick smoke test)
python benchmarks/convomem_bench.py \
    --categories all \
    --limit 100 \
    --top-k 5
```

**How it works:**
Each item is a self-contained conversation. A fresh mentisdb chain is used per
item (prevents bleed between independent conversations). The question is
searched and evidence is matched by substring. For abstention items, a correct
result means the evidence is NOT in the top-k (model should say "I don't know").

---

## LoCoMo

```bash
# Full run (1,986 QA pairs)
python benchmarks/locomo_bench.py \
    --top-k 10 \
    --chain locomo-$(date +%s)

# Dev run (first 20 persona-pairs)
python benchmarks/locomo_bench.py \
    --top-k 10 \
    --limit 20 \
    --chain locomo-dev \
    --output results/locomo.json
```

**How it works:**
Each LoCoMo item is one persona-pair with a full multi-session conversation
(up to ~300 turns) and a set of QA pairs. The conversation is ingested into a
per-item chain; each question is evaluated against that chain. Multi-hop
questions are evaluated the same way — mentisdb's graph traversal in
ranked-search is expected to surface evidence for implicit connections.

---

## Interpreting scores

- **R@5** — evidence appears in top 5 retrieved thoughts
- **R@10** — evidence appears in top 10 retrieved thoughts

Use `--top-k 5` to compare directly to MemPalace R@5 numbers.
Use `--top-k 10` for LoCoMo to match their honest-baseline comparison.

Using `--top-k 50` will inflate scores (MemPalace's 100% LoCoMo used top-50
which exceeds the session count for many items — their own BENCHMARKS.md flags
this as a caveat).

---

## Improving scores

mentisdb's `ranked-search` uses hybrid scoring:
- `lexical` — BM25-style term overlap
- `vector` — semantic embedding similarity (requires vector sidecars)
- `graph` — relation-aware traversal bonus
- `recency` — time-based boost

**To enable vector search (largest quality gain):**
Generate and push vector sidecar files for each chain. Without sidecars,
only lexical + graph scoring applies.

**To simulate LLM reranking** (closes the gap to MemPalace's 100% claim):
Use `--top-k 50` and post-process with an LLM to rerank before checking R@5.
