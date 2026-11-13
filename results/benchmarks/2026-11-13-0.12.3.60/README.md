# 0.12.3.60 hotfix benchmarks, 2026-11-13

Build: master at d3b376f (the hotfix's query-filter and append changes; later hotfix commits touch only the
dashboard settings, `.env` handling and tests), `cargo build --release` with default features, from a detached
worktree. Protocol as in `../2026-11-13-0.12.2.59/`, with the comparison that report recommends: ingest once
with d3b376f, then score the same chains with d3b376f and with 0.12.2.59 (`--skip-ingest`), each against a
freshly restarted daemon that loaded the chains from disk. Ports 9591-9595, fresh data dir.

| Benchmark | Metric | d3b376f | 0.12.2.59, same chains | Per-question differences |
|---|---|---|---|---|
| LoCoMo-10P | default R@10 | 76.7 (1516/1977) | 76.7 (1516) | per-type results identical |
| LoCoMo-10P | rerank R@10 | 76.9 (1521) | 76.9 (1521) | identical |
| LoCoMo-10P | default R@5 | 67.9 (1342) | 67.9 (1342) | identical |
| LoCoMo-10P | rerank R@5 | 68.6 (1356) | 68.6 (1356) | identical |
| LongMemEval | default R@5 | 69.8 (349/500) | 69.8 (349) | top-5 identical 500/500, scores bit-identical |
| LongMemEval | rerank R@5 | 70.2 (351/500) | 70.2 (351) | top-5 identical 500/500, scores bit-identical |

Append latency (`probe_*.json`: 3,000 sequential single-client `POST /v1/thoughts`, mean / p50 / p90 / p99 ms):
d3b376f 4.52 / 4.50 / 5.28 / 7.76 and 4.36 / 4.35 / 5.13 / 7.85 (two runs), 0.12.2.59 5.04 / 4.82 / 6.31 / 8.74.
The early size reject costs nothing measurable. Load average was 10-20, so timings are rough.

Files: `d3b_*` and `v59_*` are the two binaries scoring the same chains; `ing_*` are the ingest runs
(`ing_d3b2_*` and `ing_v59_*` are fresh ingests into a data dir that already held chains, whose recall moves by
a few questions in both directions: the same ingest-order noise documented for 0.12.2.59).
