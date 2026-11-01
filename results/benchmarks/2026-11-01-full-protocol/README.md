# Full LoCoMo-10P and LongMemEval protocol run, 2026-11-01

Build: master at b660085 plus the ranking-knob and thesaurus-parity commits (defaults: relative vector fusion r=0.8,
duplicate collapse 0.9, thesaurus weight 0.3, DF scope global). Embeddings: `MENTISDB_EMBEDDINGS=fastembed`
(`fastembed-minilm`, 384d; the harness also rebuilds `local-text-v1`). Same scripts and settings as the 0.10.9.54
README table: `locomo-benches/locomo_bench.py` (single chain with all 10 personas, graph depth 3, fresh ingest of
5,882 turns, 1,977 queries from `Nithish2410/benchmark-locomo` items/test) and `lme-benches/longmemeval_bench.py`
(`longmemeval_oracle.json`, 500 questions, fresh ingest of 10,866 turns, `--rebuild-vectors`). Fresh server and data
dir, ports 19491/19492, nothing shared with a running daemon. Rerank = RRF (`--reranking` / `--enable-reranking`, k=50).

| Benchmark | Mode | R@5 | R@10 | R@20 | q/s |
|---|---|---|---|---|---|
| LoCoMo-10P | default | 67.9 | 76.7 | 83.4 | 428 |
| LoCoMo-10P | rerank | 68.6 | 76.9 | 83.5 | 429 |
| LongMemEval | default | 69.8 | 75.6 | 81.4 | 371 |
| LongMemEval | rerank | 70.2 | 75.8 | 82.0 | 362 |

LoCoMo single-hop / multi-hop, default: R@5 71.9 / 53.0, R@10 80.4 / 62.9, R@20 86.0 / 73.8
(rerank: R@5 72.8 / 53.2, R@10 80.4 / 64.1, R@20 86.0 / 74.2).
LongMemEval overall R@10 and R@20 are the question-count-weighted sums of the per-type rows in `lme_*.txt`.
LoCoMo R@5 comes from a separate `--top-k 5` evaluation of the same chain, R@10/R@20 from `--top-k 10`.

The earlier "73.1 -> 75.2 / 68.0 -> 70.0" numbers were a different protocol: the same 1,977 LoCoMo queries and 500
LongMemEval questions and the same scripts, but run with `--skip-ingest` against chains ingested by older builds
(LoCoMo chain with the `local-text-v1` 256d sidecar, not fastembed; LongMemEval chain with fastembed) and compared
config against config on one binary. They measure the effect of the retune only; the table above is the
publishable full-protocol run.
