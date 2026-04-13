# MentisDB Roadmap

## 0.8.8 — Episode Provenance & LLM Reranking
- ~~Episode provenance tracking (source_episode field)~~ ✓
- ~~LLM-based reranking (opt-in, for closing the LongMemEval gap)~~ ✓

## 0.9.0 — Interop & Scale
- Optional LLM-extracted memories (keep no-LLM core, add opt-in pipeline)
- LangChain/LlamaIndex integration (Python bindings via REST)
- Webhooks (notify external systems on thought append)

## 1.0.0 — Production Stability
- API stability guarantees (lock public crate API, semver contract)
- Token tracking (per-agent token budget and usage metrics)
- Self-improving agent primitives
- Browser extension

## Shipped
- 0.8.7: entity_type field, type registry per chain, dashboard entity_type display/filter, dashboard modal UX overhaul, resolve_chain_key trim/filter, wizard Issue #14 fix, schema v4+ compatibility
- 0.8.6: RRF reranking, memory branching with BranchesFrom, irregular verb lemma expansion, BM25 DF cutoffs
- 0.8.5: Session cohesion tuning, graph relation scores, fastembed integration

## Benchmarks (as of 0.8.8)
- LoCoMo 10-persona: 73.0% R@10 (fresh chain)
- LoCoMo 10-persona w/ RRF: 73.0% R@10 (multi-type +0.5%)
- LongMemEval: 57.6% R@5, 62.6% R@10 (first baseline)
- Write latency: -13.8% vs pre-0.8.0

## Competitive Position
| Property | MentisDB | Mem0 | Graphiti/Zep | Letta | Cognee |
|----------|----------|------|-------------|-------|--------|
| No LLM required | Yes | No | No | No | No |
| Cryptographic integrity | Yes | No | No | No | No |
| Embedded storage | Yes | No | No | No | No |
| Language | Rust | Python | Python | Python | Python |
| Temporal facts | Yes (0.8.2) | Updates | Yes | No | No |
| Memory dedup | Yes (0.8.2) | Yes | Merge | No | Partial |
| Hybrid retrieval | BM25+vec+graph | vec+kw | semantic+kw+graph | No | vec+graph |
| RRF reranking | Yes (0.8.6) | No | No | No | No |
| Memory branching | Yes (0.8.6) | No | No | No | No |
