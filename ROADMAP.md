# MentisDB Roadmap

## 0.8.7 — Knowledge Structure
- ~~Custom entity/relation types (entity_type field, type registry per chain)~~ ✓
- Episode provenance tracking (DerivedFrom relation, source_episode field)
- LLM-based reranking (opt-in, for closing the LongMemEval gap)

## 0.9.0 — Interop & Scale
- Cross-chain graph queries (follow ThoughtRelation.chain_key at query time)
- Optional LLM-extracted memories (keep no-LLM core, add opt-in pipeline)
- LangChain/LlamaIndex integration (Python bindings via REST)
- Webhooks (notify external systems on thought append)

## 1.0.0 — Production Stability
- API stability guarantees (lock public crate API, semver contract)
- Token tracking (per-agent token budget and usage metrics)
- Self-improving agent primitives
- Browser extension

## Benchmarks (as of 0.8.7)
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
