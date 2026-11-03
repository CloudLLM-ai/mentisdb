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

## Slow or throttled machines

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
taskpolicy -b nice -n 15 python3 locomo-benches/locomo_bench.py \
    --top-k 10 --rebuild-timeout 7200 --request-timeout 120
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