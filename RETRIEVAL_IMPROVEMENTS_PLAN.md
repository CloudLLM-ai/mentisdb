# Retrieval Improvements Plan

Goal: improve MentisDB retrieval quality and scalability without breaking append-only semantics.

The four tracks are mostly parallelizable if we agree on shared interfaces first.

## Phase 0: Shared Interfaces

Do this first, then split work.

### 0.1 Add Retrieval Pipeline Concepts

Define internal structs/enums, likely in `src/search/ranked.rs` or new `src/search/pipeline.rs`.

```rust
pub enum RetrievalRoute {
    Lexical,
    SemanticVector,
    ExplicitGraph,
    ImplicitGraph,
    PprGraph,
    PrfExpandedLexical,
    SummaryHierarchy,
}

pub struct RouteScore {
    pub route: RetrievalRoute,
    pub score: f32,
}

pub struct QueryIntent {
    pub temporal: bool,
    pub entity_focused: bool,
    pub agent_focused: bool,
    pub causal: bool,
    pub semantic: bool,
    pub summary_or_global: bool,
}
```

### 0.2 Preserve Current Defaults

All new behavior should be gated behind fields in `RankedSearchQuery` / graph config first.

Example:

```json
{
  "graph": {
    "mode": "bidirectional",
    "algorithm": "bfs",
    "max_depth": 2
  }
}
```

Later allowed values:

```text
algorithm: "bfs" | "ppr"
```

Initial default: current behavior.

### 0.3 Shared Test Fixtures

Create common integration fixtures for:

- explicit relations
- implicit auto edges
- temporal memories
- summaries with `Summarizes`
- entity-tagged thoughts
- vocabulary mismatch queries

Useful test file:

- `tests/search_pipeline_integration_tests.rs`

## Workstream A: PPR Graph Expansion

Can run independently after Phase 0.

### Objective

Replace or augment bounded BFS with Personalized PageRank over explicit + implicit graph edges.

### Files

- `src/search/graph.rs`
- `src/search/expansion.rs`
- `src/search/ranked.rs`
- `tests/search_pipeline_integration_tests.rs`

### Design

Build a weighted graph view:

| Edge Source | Weight |
|---|---:|
| explicit `References` | 1.0 |
| explicit `Supports` | 1.1 |
| explicit `DerivedFrom` | 1.2 |
| explicit `Corrects` / `Invalidates` | query-dependent |
| implicit cosine edge | cosine score, e.g. `0.85..1.0` |
| temporal adjacency | optional low weight, e.g. `0.15` |

Add a PPR function:

```rust
pub struct PprConfig {
    pub damping: f32,
    pub max_iters: usize,
    pub tolerance: f32,
    pub max_nodes: usize,
    pub include_implicit_edges: bool,
}

pub struct PprResult {
    pub scores: HashMap<ThoughtLocator, f32>,
    pub seed_paths: HashMap<ThoughtLocator, Vec<GraphExpansionPath>>,
}
```

Algorithm:

1. Seed vector from lexical/vector top results.
2. Expand graph neighborhood up to `max_nodes`.
3. Run power iteration.
4. Return ranked graph scores.
5. Merge with existing ranked scoring via current RRF/score fields.

### Tests

- PPR ranks a 2-hop relevant node above unrelated lexical match.
- Implicit cosine edges contribute to PPR.
- PPR respects `max_nodes`.
- PPR deterministic across runs.
- PPR disabled keeps exact old BFS behavior.

### Verification

- `cargo test ppr`
- `cargo test --test search_pipeline_integration_tests`
- Compare LoCoMo R@10 against BFS.

### Parallel Output

Commit:

```text
feat(search): add personalized pagerank graph expansion
```

## Workstream B: PRF Query Expansion

Can run independently after Phase 0.

### Objective

Improve lexical recall by expanding queries using pseudo-relevance feedback from top lexical hits.

### Files

- `src/search/lexical.rs`
- `src/search/ranked.rs`
- `src/search/query_expansion.rs` new
- `tests/search_query_expansion_tests.rs`

### Design

Start with non-LLM PRF.

Pipeline:

1. Run original lexical query.
2. Take top `N` hits, e.g. 5.
3. Extract high-IDF candidate terms.
4. Remove stopwords, original query terms, too-common terms.
5. Weight terms using Rocchio-like formula.
6. Run expanded lexical query.
7. Fuse original + expanded route via RRF.

Config:

```rust
pub struct PrfConfig {
    pub enabled: bool,
    pub feedback_docs: usize,
    pub expansion_terms: usize,
    pub min_idf: f32,
    pub original_weight: f32,
    pub expansion_weight: f32,
}
```

Expose later as:

```json
{
  "query_expansion": {
    "mode": "none",
    "feedback_docs": 5,
    "expansion_terms": 8
  }
}
```

Later allowed values:

```text
mode: "none" | "prf"
```

### Important Guardrails

- Do not mutate the stored query.
- Do not write expansion terms into thoughts.
- Keep expansion route visible in result metadata.
- Avoid expansion if top lexical hits are weak/noisy.

### Tests

- Query `"trip cost"` expands to `"invoice vendor payment"` from feedback docs.
- Expanded route recovers a result missed by original lexical.
- Expansion disabled preserves exact old results.
- No feedback docs means no expansion.
- Very common terms are filtered.

### Verification

- `cargo test query_expansion`
- LoCoMo smoke test first 200 queries.
- LongMemEval check for no R@5 regression.

### Parallel Output

Commit:

```text
feat(search): add pseudo-relevance query expansion route
```

## Workstream C: Append-Only Hierarchical Summaries

Can run mostly independently. Needs final integration with ranked search.

### Objective

Create persistent summary thoughts that improve long-horizon retrieval without rewriting original memories.

### Files

- `src/lib.rs`
- `src/search/ranked.rs`
- `src/search/summary_index.rs` new
- `tests/hierarchical_summary_tests.rs`
- optional dashboard/TUI later

### Design

Use existing primitives:

- `ThoughtType::Summary`
- `ThoughtRelationKind::Summarizes`
- `refs`
- timestamps
- agent/session metadata

No destructive compaction.

Add API:

```rust
pub struct SummaryBuildConfig {
    pub window_size: usize,
    pub overlap: usize,
    pub by_session: bool,
    pub by_agent: bool,
    pub by_entity_type: bool,
}

pub fn build_summary_candidates(&self, config: SummaryBuildConfig) -> Vec<SummaryCandidate>;
```

Important: MentisDB core should not require an LLM. So split into two layers:

Core:

- selects ranges/clusters that need summaries
- returns candidate source thought IDs
- can create extractive summaries if needed

Daemon/API:

- optional LLM-generated summary content
- append as normal `Summary` thought

### Summary Hierarchy

Level 0:

- raw thoughts

Level 1:

- session/topic summaries

Level 2:

- chain-level rolling summaries

Relations:

- summary -> raw thoughts via `Summarizes`
- summary -> previous summary via `ContinuesFrom` or `Summarizes`

### Retrieval Integration

When query looks broad/global:

- search summaries first
- expand down from summaries to summarized raw thoughts
- boost raw thoughts whose parent summary matched

### Tests

- Building summary candidates does not mutate chain.
- Appended summaries preserve hash-chain integrity.
- Summary retrieval can find raw thought through `Summarizes`.
- Re-running candidate selection skips already summarized ranges.
- Works across sessions and entity types.

### Verification

- `cargo test hierarchical_summary`
- Full integrity test after summary append.
- Benchmark global/query-summary questions separately.

### Parallel Output

Commit:

```text
feat(memory): add append-only hierarchical summary candidates
```

## Workstream D: Query-Aware Routing

Can run independently after Phase 0, but best integrated after A/B/C.

### Objective

Route queries to the right retrieval signals before scoring.

### Files

- `src/search/query_intent.rs` new
- `src/search/ranked.rs`
- `src/server.rs` only if API fields needed
- `tests/query_intent_tests.rs`

### Design

Start deterministic, no LLM.

Heuristics:

| Query Pattern | Intent |
|---|---|
| contains date/time words | temporal |
| contains “who”, “which agent”, “by” | agent-focused |
| contains “why”, “because”, “caused” | causal |
| contains “summarize”, “overall”, “all about” | summary/global |
| contains known entity type/concept | entity-focused |
| short abstract query | semantic |

Output:

```rust
pub struct QueryRoutingPlan {
    pub lexical_weight: f32,
    pub vector_weight: f32,
    pub graph_weight: f32,
    pub ppr_weight: f32,
    pub temporal_weight: f32,
    pub summary_weight: f32,
    pub enable_prf: bool,
}
```

### Integration

In ranked search:

1. Build `QueryIntent`.
2. Build `QueryRoutingPlan`.
3. Execute selected routes.
4. Fuse route results.
5. Include route metadata in response.

### Tests

- “when did…” boosts temporal route.
- “who said…” searches agent metadata.
- “why did…” boosts causal/DerivedFrom/CausedBy edges.
- “summarize…” searches summaries first.
- Routing disabled keeps legacy weighting.

### Verification

- `cargo test query_intent`
- LoCoMo category breakdown if labels available.
- Manual probes against existing chains.

### Parallel Output

Commit:

```text
feat(search): add query-aware retrieval routing
```

## Integration Phase

After A-D land independently.

### I.1 Add Combined Ranked Pipeline

In `src/search/ranked.rs`:

Order:

1. Parse query intent.
2. Run lexical original.
3. Optionally run PRF expanded lexical.
4. Run vector route.
5. Run graph route: BFS or PPR.
6. Optionally run summary hierarchy route.
7. Fuse with RRF.
8. Add route score diagnostics.

### I.2 Response Metadata

Expose per-result route contributions:

```json
{
  "score": {
    "total": 12.4,
    "lexical": 4.1,
    "vector": 3.2,
    "graph": 2.8,
    "ppr": 1.7,
    "prf": 0.6
  }
}
```

### I.3 Config Flags

Env vars or API fields:

```text
MENTISDB_GRAPH_ALGORITHM=bfs|ppr
MENTISDB_PRF_QUERY_EXPANSION=true|false
MENTISDB_QUERY_ROUTING=true|false
MENTISDB_SUMMARY_ROUTE=true|false
```

Prefer API fields first, env defaults second.

## Benchmark Plan

### Smoke

- LoCoMo first 200 queries
- LongMemEval 100-query subset if available
- Compare:
  - baseline current
  - PPR only
  - PRF only
  - PPR + PRF
  - all routes

### Full

- LoCoMo-10P R@10
- LongMemEval R@5/R@10/R@20

### Acceptance Criteria

Ship if:

- LoCoMo-10P R@10 improves or holds within -0.5pp
- LongMemEval R@5 does not drop more than 2pp
- Latency increase acceptable, ideally under 25% for default settings
- New features can be disabled

## Suggested Parallel Assignment

### Agent 1: PPR Graph

Owns Workstream A.

Returns:

- implementation
- tests
- benchmark notes

### Agent 2: PRF Query Expansion

Owns Workstream B.

Returns:

- implementation
- tests
- top expansion examples

### Agent 3: Summary Hierarchy

Owns Workstream C.

Returns:

- candidate builder
- append-only summary flow
- tests

### Agent 4: Query Routing

Owns Workstream D.

Returns:

- intent classifier
- routing weights
- tests

### Coordinator

Owns:

- Phase 0 interfaces
- integration
- benchmarks
- docs/changelog
- final release gate

## Best First Slice

If we want maximum impact with minimum risk:

1. Implement PRF query expansion first.
2. Implement PPR graph expansion second.
3. Benchmark both independently.
4. Only then add query routing and summary hierarchy.

Reason: PRF and PPR directly target retrieval quality and can be evaluated fast. Summary hierarchy is valuable but has more product/API surface area.
