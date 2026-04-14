# MentisDB Roadmap

## 0.9.0 — Interop & Scale
- Optional LLM-extracted memories (keep no-LLM core, add opt-in pipeline)
- LangChain/LlamaIndex integration (Python bindings via REST)

## 1.0.0 — Production Stability
- API stability guarantees (lock public crate API, semver contract)
- Token tracking (per-agent token budget and usage metrics)
- Self-improving agent primitives
- Browser extension

## Shipped
- 0.8.9: webhooks (HTTP callbacks on thought append — register/list/get/delete), irregular verb lemma expansion at query time
- 0.8.7: entity_type field, type registry per chain, dashboard entity_type display/filter, dashboard modal UX overhaul, resolve_chain_key trim/filter, wizard Issue #14 fix, schema v4+ compatibility
- 0.8.6: RRF reranking, per-field BM25 DF cutoffs, memory branching with BranchesFrom
- 0.8.5: Session cohesion tuning, graph relation scores, fastembed integration

## Benchmarks (as of 0.8.9)
- LoCoMo 10-persona: 72.0% R@10 (0.8.9)
- LoCoMo 10-persona w/ RRF: 73.0% R@10 (multi-type +0.5%)
- LongMemEval (fresh chain): 66.8% R@5, 74.1% R@10 (0.8.9)

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
