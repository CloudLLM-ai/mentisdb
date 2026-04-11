# MentisDB Roadmap

## 0.8.6 — Retrieval Quality
- Irregular verb lemma expansion ("went"→"go", ~38% of LoCoMo lexical gaps)
- Lightweight reranking (RRF between lexical-only and vector-only, no LLM)
- Per-field BM25 DF cutoffs (content vs tags vs concepts)

## 0.8.7 — Knowledge Structure
- Custom entity/relation types (entity_type field, type registry per chain)
- Episode provenance tracking (DerivedFrom relation, source_episode field)
- Memory chain branching (BranchesFrom relation, branch_from() method, sub-agent isolation)

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

## Benchmarks (as of 0.8.5)
- LoCoMo 2-persona: 88.7% R@10 (baseline 55.8%, +32.9pp)
- LoCoMo 10-persona: 74.6% R@10 (0.8.5, was 74.2% at 0.8.1)
- LoCoMo 10-persona single-hop: 79.0% R@10
- LoCoMo 10-persona multi-hop: 58.4% R@10
- LongMemEval: 67.6% R@5, 73.2% R@10 (baseline 57.2%, +10.4pp)
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
| Memory branching | Planned (0.8.7) | No | No | No | No |
