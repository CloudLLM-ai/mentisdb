# OpenCode Agent Checkpoint — Pre-Restart

**Date:** 2026-05-27  
**Branch:** `feat/synonym-query-expansion`  
**Workspace:** `/Users/gubatron/workspace/mentisdb`

## Status: COMPLETE — All work committed

### Commits Made (7 total, in order)

1. **`cece231`** — `feat(search): expand irregular verb lemma coverage to 300+ entries`
   - `src/search/lemmas.rs`: Expanded from ~100 to 300+ irregular verbs
   - Added `lemmatize()` helper combining irregular lookup + regular suffix stripping (-ing, -ed, -ies, -s)

2. **`c4a2a4b`** — `feat(search): add static thesaurus module with ~895 headwords`
   - `src/search/thesaurus.rs` (new): Parser for `thesaurus_data.txt` via `include_str!`
   - `src/search/thesaurus_data.txt` (new): ~895 headwords covering verbs, nouns, adjectives, adverbs, technical terms
   - `src/search/mod.rs`: Export new modules

3. **`88e6cdf`** — `feat(search): add embedding-based synonym generator`
   - `src/search/embedding_synonyms.rs` (new): Uses `LocalTextEmbeddingProvider` + `VectorIndex`
   - Research shows it underperforms (NDCG 0.48 vs 0.68 thesaurus) — kept for experimentation

4. **`91bf5c3`** — `feat(search): integrate synonym expansion into lexical scoring and ranked queries`
   - `src/search/lexical.rs`: Fast path when no synonyms + weighted synonym scoring
   - `src/lib.rs`: `RankedSearchQuery` gains `synonyms` + `synonym_weight` (default 0.7)
   - `tests/search_lexical_tests.rs`: +3 new tests
   - `tests/search_ranked_query_tests.rs`: +1 new test (`ranked_query_with_synonyms_boosts_recall`)

5. **`278b406`** — `test(search): add search quality research harness for synonym generators`
   - `tests/search_quality_research.rs` (new): Synthetic corpus (140 docs, 12 queries)
   - Compares manual map, thesaurus auto, embedding auto, combined configs
   - Key finding: thesaurus auto NDCG 0.68 vs baseline 0.52 (+31%)

6. **`b790a4f`** — `bench(lme): expose reranking parameters in evaluation harness`
   - `lme-benches/longmemeval_bench.py`: `ranked_search()` accepts `enable_reranking`, `rerank_k`

7. **`d252c53`** — `docs(changelog): add synonym expansion entries to 0.9.9.45 UNRELEASED`
   - `changelog.txt`: 6 new bullet points for synonym expansion work
   - `WHITEPAPER.tex`: Updated to v0.9.9 (May 26 2026) with new §5.3 "Static Thesaurus Synonym Expansion"

### Test Status

- **Library tests:** 69 passed, 0 failed
- **Integration tests:** 35 passed, 0 failed (lexical: 16, ranked: 18, research: 1)
- **Benchmarks:** Smoke tests pass; baseline regression ~3% (acceptable)
- **Pre-existing failure:** `dashboard_tests::agent_detail_form_hydrates_values_after_dom_insertion` (unrelated)

### Key Decisions / Context

- **Unidirectional thesaurus:** Parser builds forward map only (not bidirectional). Many terms appear as headwords in both directions in practice.
- **synonym_weight = 0.7** is current default. Research showed 0.8 is optimal for NDCG but 0.7 is safer default.
- **Embedding NN rejected:** `LocalTextEmbeddingProvider` uses hash-based features, not true semantic similarity. Combined maps (thesaurus + embedding) degraded quality due to noise.
- **Fast path in lexical.rs:** When `synonyms.is_empty()`, uses original `normalized_terms()` path to avoid allocation overhead. This keeps baseline latency neutral.

### Files Changed

```
 M lme-benches/longmemeval_bench.py
 M src/lib.rs
 M src/search/lemmas.rs
 M src/search/lexical.rs
 M src/search/mod.rs
 M tests/search_lexical_tests.rs
 M tests/search_ranked_query_tests.rs
 A src/search/embedding_synonyms.rs
 A src/search/thesaurus.rs
 A src/search/thesaurus_data.txt
 A tests/search_quality_research.rs
 M changelog.txt
 M WHITEPAPER.tex
```

### Next Steps (if continuing)

1. Re-run `cargo test` after restart to verify environment
2. Optionally: run full LongMemEval benchmark with thesaurus to validate real-world gains
3. Optionally: tune synonym_weight on real benchmark (research showed 0.8 may be better for NDCG)
4. The branch is ready for PR / merge when desired

### Daemon Status

A mentisdb daemon was started during this session (PID may vary after restart). If needed:
```bash
cd /Users/gubatron/workspace/mentisdb
MENTISDB_MCP_PORT=9471 MENTISDB_REST_PORT=9472 ./target/release/mentisdb --mode both
```
