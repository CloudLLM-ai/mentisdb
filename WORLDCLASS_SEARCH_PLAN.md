# World-Class Search Plan

## Goals

- Make MentisDB retrieval strong when the agent remembers exact metadata.
- Make MentisDB retrieval strong when the agent remembers only a topic, paraphrase, or rough gist.
- Preserve the append-only thought chain as the canonical source of truth.
- Keep derived search indexes rebuildable and optional.
- Expose search consistently through crate APIs, MCP, REST, CLI, and dashboard surfaces.

## Current Status On Master

- Phase 1 is complete.
- Phase 2 is complete in the core crate.
- Phase 3 is complete in the core crate.
- Phase 4 is complete at the transport layer.
- Phase 5 is complete at the dashboard layer.

## Current Baseline

- Structured retrieval is good when thoughts have strong `tags`, `concepts`, `agent_id`, `agent_name`, `agent_owner`, `thought_type`, and `role` metadata.
- Text search is currently substring matching over thought content, agent metadata, tags, and concepts.
- Results are filtered effectively, but not ranked with lexical or semantic relevance.
- Context expansion already exists via `refs` and typed `relations`, which is a strong foundation for search-driven graph retrieval.

## Non-Goals

- Do not mutate the durable thought record format just to support search ranking.
- Do not make embeddings mandatory for local or offline MentisDB usage.
- Do not couple the core crate to one vector database vendor or hosted embedding provider.

## Phase 1: Lexical And BM25 Search

Status: complete on `master` as of March 25, 2026.

### Objective

Replace substring-only text search with real lexical retrieval and ranking.

### Scope

- Add a derived lexical index for thought text and selected metadata.
- Support tokenized search over:
  - `content`
  - `tags`
  - `concepts`
  - `agent_id`
  - agent display name, aliases, owner, and description
- Rank with BM25-style scoring instead of append-order-only filtering.
- Preserve all existing structured filters and combine them with lexical retrieval.

### Design

- Introduce a search module that owns:
  - normalization
  - tokenization
  - postings lists
  - term statistics
  - lexical scoring
- Treat the lexical index as rebuildable derived state keyed by thought position and thought id.
- Build the index on open and update it incrementally on append.
- Keep the canonical chain storage unchanged.

### Query Model

- Extend the core query path with a search mode concept:
  - `filter_only`
  - `lexical`
- Preserve the existing `ThoughtQuery` structured filters.
- Add a ranked result shape internally:
  - `thought_id`
  - `index`
  - `score`
  - `matched_terms`
  - `match_source` such as `content`, `tag`, `concept`, or `agent_registry`

### Deliverables

- `search/lexical.rs` or equivalent module for tokenization and scoring
- ranked lexical query execution in the core crate
- ranked hit explanations via `matched_terms` and `match_sources`
- REST lexical ranking at `POST /v1/lexical-search`
- regression tests for:
  - synonym-free keyword retrieval
  - multi-term ranking
  - phrase-ish queries
  - metadata hits from tags, concepts, and agent registry fields
  - deterministic ranking on stable fixtures

### Parallel Slice

- Worker 1 owns the lexical index data structures and scoring.
- Worker 2 owns query integration and tests.

## Phase 2: Search Plus Graph Expansion

Status: complete in the core crate on `master` as of March 28, 2026.

### Objective

Turn seed retrieval into usable memory context by expanding across `refs` and `relations`.

### Scope

- Add search-driven context resolution:
  - find top seed thoughts
  - expand outward across explicit references and typed relations
  - rerank or group expanded context for delivery
- Support depth-limited expansion and relation-kind-aware weighting.

### Design

- Keep seed retrieval and graph expansion separate.
- Use lexical search to find initial candidates.
- Expand via:
  - `refs`
  - typed `relations`
- Track:
  - graph distance
  - relation kinds traversed
  - number of distinct seed paths reaching a thought

### Ranking

- Base score starts from lexical score.
- Expansion score adjusts by:
  - shorter graph distance
  - multiple supporting paths
  - relation-type boosts
  - importance and confidence
- Offer two output modes:
  - ranked flat list
  - grouped context bundles anchored on the seed thought

### Deliverables

- `search/graph.rs` or equivalent module
- a context-bundle result type for crate consumers
- tests for:
  - expansion depth limits
  - duplicate suppression
  - refs plus relations traversal
  - reranking with graph distance

### Parallel Slice

- Worker 3 owns graph traversal and expansion scoring.
- Worker 4 owns bundle rendering and tests.

## Phase 3: Optional Embeddings And Vector Sidecar

Status: complete in the core crate on `master` as of March 28, 2026.

### Objective

Add true semantic retrieval without weakening append-only durability or making remote AI dependencies mandatory.

### Scope

- Add an optional embedding pipeline and sidecar persistence layer.
- Support offline-disabled mode with no vector dependencies.
- Support rebuilds when the embedding model changes.

### Design

- Store embeddings outside the canonical chain as derived artifacts.
- Key vector records by:
  - `chain_key`
  - `thought_id`
  - `thought_hash`
  - `model_id`
  - embedding dimension
  - embedding version
- Mark embedding indexes as invalidated when:
  - a new thought is appended and not yet embedded
  - the configured model changes
  - a sidecar integrity check fails
- Support sidecar adapters:
  - in-process simple vector index first
  - pluggable external adapter later if needed

### Safety And Integrity

- The sidecar must be rebuildable from the durable chain.
- The sidecar must never become the only copy of any meaning-bearing data.
- Search results should surface whether a vector hit was produced from stale or fresh embeddings.

### Acceptance Criteria

- A chain with Phase 3 disabled behaves normally with lexical and graph retrieval only; no embedding dependency becomes mandatory.
- Vector sidecar entries are separated by `chain_key`, `thought_id`, `thought_hash`, `model_id`, embedding dimension, and embedding version so incompatible embeddings cannot silently mix.
- Changing `model_id` or embedding version invalidates old vector state and forces explicit rebuild rather than silently reusing stale vectors.
- Rebuilding from the canonical chain recreates the sidecar without mutating durable thoughts or requiring any meaning-bearing data outside the chain.
- Search results can tell callers whether a vector hit came from fresh or stale embeddings.
- Sidecar corruption or deletion degrades semantic/vector retrieval only; it does not compromise append-only durability or non-vector search correctness.

### Deliverables

- embedding job abstraction
- vector sidecar storage format
- optional indexing and rebuild commands
- tests for:
  - model/version separation
  - stale index detection
  - rebuild from canonical chain
  - sidecar corruption recovery

### Parallel Slice

- Worker 5 owns the sidecar persistence model and rebuild path.
- Worker 6 owns the embedding provider abstraction and tests.

### Landed Shape

- `src/search/vector.rs` provides the provider abstraction, vector validation, cosine ranking, and deterministic in-process vector index.
- `src/search/sidecar.rs` provides rebuildable sidecar persistence with integrity checks, freshness states, and model/version separation.
- `MentisDb` exposes explicit additive methods to:
  - compute deterministic sidecar paths
  - rebuild sidecars from the canonical chain
  - load sidecars
  - report sidecar freshness
  - run vector search without altering default `ThoughtQuery` behavior

## Phase 4: Hybrid Ranking API And MCP/REST Exposure

### Objective

Expose one high-quality retrieval surface that can blend filters, lexical ranking, graph expansion, and vectors.

### Scope

- Add explicit search modes:
  - `filter_only`
  - `lexical`
  - `lexical_graph`
  - `vector`
  - `hybrid`
- Add score-bearing response types.
- Expose hybrid retrieval consistently through crate, MCP, REST, and CLI surfaces.

### API Design

- Keep the existing plain search path for backward compatibility.
- Add a new ranked search API rather than silently changing the semantics of every current search response.
- New request fields should include:
  - `mode`
  - `text`
  - structured filters
  - `seed_limit`
  - `expand_depth`
  - `rerank_limit`
  - `embedding_model`
- New response fields should include:
  - `score`
  - `score_breakdown`
  - `seed_matches`
  - `graph_distance`
  - `match_sources`

### Exposure

- Core crate:
  - ranked search result types and query builders
- MCP:
  - `mentisdb_ranked_search` for flat ranked retrieval
  - `mentisdb_context_bundles` for seed-anchored grouped context
- REST:
  - `POST /v1/ranked-search`
  - `POST /v1/context-bundles`
- CLI:
  - add search inspection commands for operators and local testing

### Deliverables

- new ranked search APIs
- MCP and REST transport types
- compatibility tests proving existing endpoints remain stable
- docs for query semantics and ranking fields
- transport acceptance tests:
  - `tests/search_transport_contract_tests.rs`
  - contract checks for ranked response fields (`backend`, `score`, `matched_terms`, `match_sources`, `graph_distance`, `graph_seed_paths`, `graph_relation_kinds`, `graph_path`)
  - contract checks for grouped bundle response fields (`seed`, `support`, `relation_kinds`, `seed_path_count`, `path`, `consumed_hits`)
  - compatibility check that plain `POST /v1/search` remains stable

### Parallel Slice

- Worker 7 owns MCP and REST contracts.
- Worker 8 owns CLI and docs for the new retrieval surfaces.

## Phase 5: Dashboard Integration

Status: complete on `master` as of March 28, 2026.

### Objective

Make search visible and useful to operators inside the current chain explorer workflow.

### Scope

- Integrate search into the existing dashboard chain view.
- Add two primary controls:
  - text input
  - agent-id dropdown populated from the currently browsed chain
- Reuse the existing table, thought modal, and pagination patterns.
- Keep plain author/type filtering in chronological order when no search text is present.
- Surface ranked-search metadata and grouped context bundles when search text is present.

### UX Design

- Default state remains chronological browsing.
- When either search field is active:
  - query the chain search endpoint
  - show ranked or filtered results depending on the current backend phase
- Agent dropdown should show:
  - `agent_id`
  - display name when available
  - optional thought count

### Backend Support

- Add a dashboard-oriented chain search endpoint with pagination metadata.
- Add a chain-scoped agent list endpoint suitable for the dropdown.
- Ensure the dropdown reflects live authors in the chain, not only registry entries.
- Return canonical ranked-search fields additively so the dashboard does not drift from MCP/REST vocabulary.
- Expose a dashboard bundle endpoint for explicit grouped-context inspection.

### States

- browsing-empty
- search-empty
- search-error
- search-loading

### Deliverables

- dashboard API handlers
- dashboard HTML and JavaScript changes
- tests for:
  - chain-scoped agent dropdown data
  - search pagination
  - ranked result metadata
  - grouped context bundles
  - non-text legacy filtering behavior
  - empty and error states

### Parallel Slice

- Worker 9 owns dashboard backend handlers.
- Worker 10 owns dashboard frontend integration.

## Execution Order

1. Land phase 1 first. This is the highest-leverage improvement and reduces pressure to jump straight to vectors.
2. Land phase 2 next. MentisDB already has graph structure, so seed-plus-expansion is a natural force multiplier.
3. Land phase 4 crate and transport contracts in a minimal lexical-first form.
4. Land phase 5 dashboard integration once the ranked search response shape is stable enough.
5. Land phase 3 vector sidecar after lexical and graph retrieval are already strong.
6. Extend phase 4 hybrid ranking after phase 3 exists.

## Recommended First Implementation Milestones

### Milestone A

- lexical tokenization
- postings lists
- BM25 scoring
- ranked crate API
- unit and fixture tests

### Milestone B

- lexical search plus graph expansion
- context bundles
- rerank logic
- operator-facing search evaluation fixtures

### Milestone C

- ranked REST and MCP endpoints
- CLI inspection
- dashboard search UI

### Milestone D

- embedding provider abstraction
- vector sidecar storage
- rebuild command
- hybrid reranking

## Evaluation Criteria

- exact keyword retrieval improves over current substring search
- multi-term ranking is deterministic and visibly better than recency-only tail selection
- graph expansion brings in the right supporting context without flooding the result set
- vector hits are optional, attributable, and rebuildable
- dashboard search is fast enough for interactive operator use
- dashboard operators can inspect why a result surfaced without leaving the UI

## Concurrency Plan

- Keep worker ownership disjoint by module and surface.
- Do not let multiple workers edit the same search core files at once.
- Run backend and frontend dashboard work only after the response contract is stable.
- Require every worker to load the MentisDB skill, load recent context from the `mentisdb` chain, and save checkpoints and lessons back to the same chain.
