# mentisdb Benchmarks

Retrieval recall benchmarks comparing mentisdb against published results from
MemPalace (the current SOTA on these evaluations as of April 2026).

## Benchmarks

| Benchmark | Questions | Metric | MemPalace baseline | MemPalace best |
|-----------|-----------|--------|-------------------|----------------|
| LongMemEval | 500 | R@5 | 96.6% (no API) | 100% (+ Haiku rerank) |
| ConvoMem | 75,336 | R@5 overall | 92.9% | — |

All measure **retrieval recall** — for each question, does the gold
evidence turn appear in the top-k thoughts returned by the memory system?
Higher is better.

The **LoCoMo** benchmark lives in `locomo-benches/`.

---

## LongMemEval — step by step

### 1. Python dependencies

```bash
pip3 install requests datasets
```

The dataset is already checked in at `data/longmemeval_oracle.json` — no
download needed.

### 2. Start mentisdb

```bash
mentisdb &
```

### 3. Run the benchmark

The shell script is fully automatic:

```bash
bash lme-benches/run_longmemeval.sh
```

It queries `GET /v1/chains`, picks the `lme-*` chain with the most thoughts
(a full ingest beats a dev run), and skips ingestion automatically. If no
`lme-*` chain exists it creates a fresh one and ingests.

**First run or after ingestion changes** (e.g. importance weighting was
updated) — force a fresh ingest:

```bash
bash lme-benches/run_longmemeval.sh --force-reingest
```

**Dev run** — first 50 questions, fast feedback loop:

```bash
bash lme-benches/run_longmemeval.sh --limit 50
```

Note: `--limit 50` only covers a subset of question types and gives an
optimistic score. The full 500-question run is the authoritative number.

**`--workers` note:** 4 is the safe default for ingestion. Raising it speeds
things up but risks overwhelming the daemon's write queue. If you see
404/500 errors mid-ingest, reduce workers or restart the daemon and re-run.

### 4. What the output means

The script reports R@5, R@10, and R@20 simultaneously (retrieves top-20 per
query, checks at each cutoff) and prints a diagnostics section:

- **Near-miss analysis** — how many misses appear in top-10/top-20 tells you
  whether the problem is ranking order (evidence retrieved but ranked too low)
  or a true lexical gap (evidence never surfaces).
- **Score breakdown on misses** — avg lexical/graph/recency/total scores on
  missed queries, showing which signals are weak.
- **Evidence length stats** — short evidence is harder for substring matching;
  useful for identifying structurally difficult question types.
- **Sample misses** — question, gold evidence snippet, top-1 retrieved snippet,
  and score breakdown for the worst-performing question type.

### 5. Manual control

```bash
# Re-evaluate an existing chain without re-ingesting
python3 lme-benches/longmemeval_bench.py \
    --data data/longmemeval_oracle.json \
    --chain lme-1234567890 \
    --top-k 5

# Force re-ingest an existing chain
python3 lme-benches/longmemeval_bench.py \
    --data data/longmemeval_oracle.json \
    --chain lme-1234567890 \
    --force-reingest
```

### 6. Slow or throttled machines

The vector rebuild (`POST /v1/vectors/rebuild`) is synchronous: the harness
holds one request open until the daemon has embedded the whole chain, which
takes about a minute on a quiet machine and well over ten minutes on efficiency cores
(`taskpolicy -b`) or under heavy load. Two flags control the client timeouts:

| Flag | Default | Applies to |
|------|---------|------------|
| `--rebuild-timeout SECONDS` | 3600 | each vector sidecar rebuild request |
| `--request-timeout SECONDS` | 60 | each append (ingest) and `ranked-search` (evaluation) request |

If ingest, the rebuild or evaluation cannot finish, the harness stops with a
`PHASE FAILED: <phase>: ...` line on stderr and exit code 2, instead of going on
to score a half-built chain. Exit codes: 0 = finished at or above the pass
threshold, 1 = finished below it, 2 = a phase failed and the run has no result.
A rebuild that times out is still running inside the daemon; wait for it (or
restart the daemon) before the next run. A rebuild that the daemon rejects with
an HTTP error (for example a build without the `local-embeddings` feature) only
warns, and the run continues on lexical and graph scoring.

```bash
taskpolicy -b nice -n 15 python3 lme-benches/longmemeval_bench.py \
    --data data/longmemeval_oracle.json --chain lme-full \
    --force-reingest --rebuild-timeout 7200 --request-timeout 120
```

---

## ConvoMem

```bash
# Quick smoke test — 100 items per category
python3 lme-benches/convomem_bench.py \
    --categories all \
    --limit 100 \
    --top-k 5

# Single category
python3 lme-benches/convomem_bench.py \
    --categories user asst \
    --limit 500 \
    --top-k 5

# Full run (~75k pairs, ~30 min)
python3 lme-benches/convomem_bench.py \
    --top-k 5 \
    --output results/convomem.json
```

Dataset is downloaded automatically from HuggingFace (`Salesforce/ConvoMem`)
on first run and cached locally by the `datasets` library.

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
