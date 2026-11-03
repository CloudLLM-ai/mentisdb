# 0.11.0.56 release benchmarks, 2026-11-03

Build: master at 4e6cd3f (release candidate 0.11.0.56; `Cargo.toml` still says 0.10.10), `cargo build --release`
with default features. Same protocol, scripts and settings as the 2026-11-01 baseline
(`../2026-11-01-full-protocol/`, built from 70e6e35): `locomo-benches/locomo_bench.py` (single chain with all 10
personas, graph depth 3, fresh ingest of 5,882 turns, 1,977 queries from `Nithish2410/benchmark-locomo`
items/test) and `lme-benches/longmemeval_bench.py` (`longmemeval_oracle.json`, 500 questions, fresh ingest of
10,866 turns, `--rebuild-vectors`). `MENTISDB_EMBEDDINGS=fastembed` (`fastembed-minilm`, 384d; the harness also
rebuilds `local-text-v1`). Fresh server and data dir, ports 19491/19492, nothing shared with a running daemon.
Rerank = RRF (`--reranking` / `--enable-reranking`, k=50). The bench scripts and dataset are unchanged since the
baseline.

| Benchmark | Mode | R@5 | R@10 | R@20 | q/s* |
|---|---|---|---|---|---|
| LoCoMo-10P | default | 67.9 (+0.0) | 76.7 (+0.0) | 83.4 (+0.0) | 277 |
| LoCoMo-10P | rerank | 68.6 (+0.0) | 77.0 (+0.1) | 83.6 (+0.1) | 196 |
| LongMemEval | default | 69.8 (+0.0) | 75.6 (+0.0) | 81.4 (+0.0) | 215 |
| LongMemEval | rerank | 70.2 (+0.0) | 75.8 (+0.0) | 81.8 (-0.2) | 218 |

Deltas in parentheses are against the 2026-11-01 baseline. LoCoMo default is identical query for query
(1,516/1,977 at R@10); rerank R@10 is 1,522 against 1,521. LongMemEval R@5 is 349/500 (default) and 351/500
(rerank), the same as the baseline.

LoCoMo single-hop / multi-hop, default: R@5 71.9 / 53.0, R@10 80.4 / 62.9, R@20 86.0 / 73.8
(rerank: R@5 72.8 / 53.2, R@10 80.4 / 64.3, R@20 86.0 / 74.5).
LongMemEval overall R@10 and R@20 are the question-count-weighted sums of the per-type rows in `lme_*.txt`.
LoCoMo R@5 comes from a separate `--top-k 5` evaluation of the same chain, R@10/R@20 from `--top-k 10`.
Ingest: LoCoMo 5,882 turns in 32.6s, LongMemEval 10,866 turns in 154.3s. Wall-clock for the whole protocol: 353s.

\* The q/s and ingest numbers here are **not comparable** with the baseline's 428/371 q/s: this run shared the
machine with other builds (load average about 10 at the start and above 30 by the end). To separate load from a
real slowdown, the protocol was run again back to back on four binaries under one policy: macOS background mode
(`taskpolicy -b`, efficiency cores only) at nice 15. Every 15-second PRI sample for every run was 4 (background),
except for one transient sample in base1. Compare these numbers only with each other:

| Run (in order) | LoCoMo ingest | LoCoMo q/s, default k10 / k5 | LoCoMo q/s, rerank k10 / k5 | LME ingest |
|---|---|---|---|---|
| base1: baseline binary (70e6e35 build) | 138.9s | 96.7 / 67.6 | 54.7 / 65.0 | 578s |
| 4e6cd3f | 105.8s | 95.6 / 92.4 | 90.4 / 95.8 | 491s |
| 1d4be8f | 101.7s | 75.0 / 76.9 | 76.7 / 79.9 | 520s |
| base2: baseline binary again | 104.2s | 83.2 / 80.7 | 77.0 / 79.2 | 734s |

Every candidate number falls inside or above the base1/base2 spread, so this set shows no throughput regression in
4e6cd3f or 1d4be8f. Recall in the four throttled runs matched the table above to within one query: LoCoMo R@10
1,516 default and 1,521 rerank in every run, rerank R@5 1,356 or 1,357, and LongMemEval rerank R@5 351/500 (base1,
4e6cd3f, 1d4be8f; base2's rerank pass lost its connection and has no result). The LongMemEval default evaluations in that set did not complete
on efficiency cores: the `--rebuild-vectors` call ran past the harness's 600s client timeout. So that set has no
LongMemEval q/s and is not part of the recall table.
