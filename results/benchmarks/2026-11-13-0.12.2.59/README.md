# 0.12.2.59 release benchmarks, 2026-11-13

Build: tag 0.12.2.59 (6cbf268), `cargo build --release` with default features, from a detached worktree.
Same protocol, scripts and settings as `../2026-11-03-0.11.0.56/`: `locomo-benches/locomo_bench.py`
(fresh ingest of 5,882 turns, 1,977 queries) and `lme-benches/longmemeval_bench.py` (`longmemeval_oracle.json`,
500 questions, fresh ingest of 10,866 turns, `--rebuild-vectors`). `MENTISDB_EMBEDDINGS=fastembed`. Fresh server
and data dir, ports 9591-9595, nothing shared with a running daemon. Rerank = RRF (`--reranking` /
`--enable-reranking`, k=50).

| Benchmark | Metric | 0.12.1.58 (release notes) | 0.12.2.59 ingest A | 0.12.2.59 ingest B | 0.12.1.58 on the same chains A / B |
|---|---|---|---|---|---|
| LoCoMo-10P | default R@10 | 76.7 | 76.7 (1516/1977) | - | 76.7 |
| LoCoMo-10P | rerank R@10 | 76.9 | 76.9 (1521) | - | 76.9 |
| LoCoMo-10P | default R@5 | 67.9 | 67.9 (1342) | - | 67.9 |
| LoCoMo-10P | rerank R@5 | - | 68.6 (1356) | - | 68.6 |
| LongMemEval | default R@5 | 70.0 (350/500) | 69.6 (348) | 69.8 (349) | 69.6 / 69.8 |
| LongMemEval | rerank R@5 | 70.6 (353/500) | 70.0 (350) | 70.2 (351) | 70.0 / 70.2 |

**No ranking change.** 0.12.1.58, rebuilt and run against the chains 0.12.2.59 ingested, returned the same
hits and the same top-5 lists for 500 of 500 LongMemEval questions in all four chain x mode runs, and the same
LoCoMo results. Scored against a daemon that loaded the chain from disk, the scores themselves are
bit-identical.

**The LongMemEval gap to the stored 70.0 / 70.6 is ingest order, not code.** The harness ingests with 4
concurrent workers, so the order thoughts land in varies. Two fresh ingests on the same 0.12.2.59 binary gave
348 vs 349 (default) and 350 vs 351 (rerank), with 49 of 500 top-5 lists differing between them. The 0.12.1.58
run's per-question output was not stored, so it could not be diffed. Comparing a release against stored
LongMemEval numbers carries about 1-3 questions of noise; scoring both binaries against the same ingested data
is the reliable comparison.

Files: `locomo_*` and `lme_*` (ingest A) / `lme2_*` (ingest B) are 0.12.2.59; `v58_*` is 0.12.1.58 scoring the
same chains (`lme-1794578370` = A, `lme-full` = B); `v59r_*` is 0.12.2.59 re-scored on the chain reloaded from
disk. The machine was heavily loaded (load average 72), so throughput in `timings.txt` is not comparable with
earlier runs.
