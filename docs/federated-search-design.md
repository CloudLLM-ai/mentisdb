# Federated Search API Design — MentisDB 0.9.0

## Status

Draft. This document specifies the cross-chain federated search feature for MentisDB 0.9.0.

---

## 1. Overview

Federated search lets callers issue a single ranked-search query over **multiple chains simultaneously** and receive a single merged, ranked result list. This is useful for:

- Multi-agent hubs that maintain separate chains per agent but need unified retrieval
- Cross-organizational memory aggregation
- Backup/restore verification across chains

### Design Goals

1. **Unified result set** — one ranked list across all requested chains
2. **Chain provenance** — every hit carries the `chain_key` it came from
3. **Duplicate elimination** — same thought (by UUID) never appears twice, even if a branch and its ancestor both contain it
4. **Transparent ancestor inclusion** — searching a branch chain automatically includes its ancestors (consistent with existing single-chain behavior)
5. **No performance regression** — concurrent fetching where possible

---

## 2. API Shapes

### 2.1 REST API — `POST /v1/federated-search`

#### Request

```json
{
  "chain_keys": ["agent-brain", "project-copilot", "experiment-1"],
  "text": "distributed cache consistency",
  "limit": 20,
  "offset": 0,
  "graph": {
    "max_depth": 2,
    "max_visited": 128,
    "include_seeds": true,
    "mode": "bidirectional"
  },
  "filter": {
    "thought_types": ["Finding", "Insight"],
    "min_importance": 0.5
  },
  "as_of": "2025-01-01T00:00:00Z",
  "scope": "user",
  "enable_reranking": true,
  "rerank_k": 50
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `chain_keys` | `Vec<String>` | **Yes** | List of chain keys to search. |
| `text` | `String` | No | Lexical query text. |
| `limit` | `usize` | No | Max hits returned (default 10). |
| `offset` | `usize` | No | Result offset for paging (default 0). |
| `graph` | `RankedSearchGraphRequest` | No | Graph expansion config. |
| `filter` | `ThoughtQuery` | No | Deterministic pre-filter. |
| `as_of` | `DateTime<Utc>` | No | Point-in-time query. |
| `scope` | `String` | No | Memory scope filter (`user`, `session`, `agent`). |
| `enable_reranking` | `bool` | No | Enable RRF reranking (default false). |
| `rerank_k` | `usize` | No | RRF candidates window (default 50). |
| `entity_type` | `String` | No | Entity type filter. |

#### Response

```json
{
  "backend": "lexical_graph",
  "total": 847,
  "results": [
    {
      "chain_key": "experiment-1",
      "thought": { ... },
      "score": {
        "lexical": 8.4,
        "vector": 0.0,
        "graph": 2.1,
        "relation": 0.5,
        "seed_support": 2.0,
        "importance": 0.7,
        "confidence": 0.9,
        "recency": 0.12,
        "session_cohesion": 0.0,
        "rrf": 0.0,
        "total": 13.82
      },
      "matched_terms": ["cache", "consistency", "distributed"],
      "match_sources": ["content"],
      "graph_distance": 1,
      "graph_seed_paths": 1,
      "graph_relation_kinds": ["Supports"],
      "graph_path": { ... }
    }
  ]
}
```

**Shape is identical to `RankedSearchResponse`** — callers can treat federated and single-chain ranked search interchangeably.

---

### 2.2 MCP Tool — `mentisdb_federated_search`

```json
{
  "name": "mentisdb_federated_search",
  "description": "Run federated ranked retrieval over multiple chains simultaneously and return a single merged, ranked result list. Useful for multi-agent hubs or cross-organizational memory aggregation.",
  "input": {
    "type": "object",
    "properties": {
      "chain_keys": {
        "type": "array",
        "items": { "type": "string" },
        "description": "List of chain keys to search."
      },
      "text": {
        "type": "string",
        "description": "Optional lexical query text."
      },
      "limit": {
        "type": "integer",
        "description": "Maximum hits to return."
      },
      "offset": {
        "type": "integer",
        "description": "Result offset for paging."
      },
      "graph": {
        "type": "object",
        "description": "Graph expansion config: max_depth, max_visited, include_seeds, mode."
      },
      "thought_types": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional ThoughtType filter."
      },
      "tags_any": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional tags filter."
      },
      "concepts_any": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional concepts filter."
      },
      "agent_ids": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional agent IDs filter."
      },
      "min_importance": {
        "type": "number",
        "description": "Minimum importance threshold."
      },
      "min_confidence": {
        "type": "number",
        "description": "Minimum confidence threshold."
      },
      "since": {
        "type": "string",
        "description": "RFC 3339 lower timestamp bound."
      },
      "until": {
        "type": "string",
        "description": "RFC 3339 upper timestamp bound."
      },
      "as_of": {
        "type": "string",
        "description": "Point-in-time query timestamp."
      },
      "scope": {
        "type": "string",
        "description": "Memory scope: user, session, or agent."
      },
      "enable_reranking": {
        "type": "boolean",
        "description": "Enable RRF reranking."
      },
      "rerank_k": {
        "type": "integer",
        "description": "RRF candidates window size."
      },
      "entity_type": {
        "type": "string",
        "description": "Entity type label filter."
      }
    },
    "required": ["chain_keys"]
  }
}
```

**Output**: Same JSON shape as `RankedSearchResponse` serialized into a `ToolResult`.

---

### 2.3 Rust API — `MentisDb::query_federated()`

```rust
/// Query multiple chains simultaneously and return a single merged ranked result.
///
/// This method launches concurrent ranked searches against each provided chain key,
/// deduplicates hits by thought UUID, merges the results using score-based ordering,
/// and applies optional RRF reranking over the combined candidate set.
///
/// # Parameters
///
/// * `requests` — One [`RankedSearchQuery`] per chain key. Each query carries its own
///   `filter`, `text`, `graph`, `as_of`, `scope`, and reranking configuration.
/// * `limit` — Global cap on merged results returned.
/// * `offset` — Offset into the merged sorted list for paging.
///
/// # Algorithm
///
/// 1. For each `chain_key` in `requests`, open the chain and call
///    `query_ranked()` concurrently using `tokio::task::spawn_blocking`.
/// 2. Collect all hits into a global list, tagging each with its source `chain_key`.
/// 3. Deduplicate by thought UUID — only the first occurrence (highest score)
///    is retained.
/// 4. Sort deduplicated hits by `total` score descending.
/// 5. If any request enabled reranking, apply RRF over the merged list.
pub fn query_federated(
    requests: &BTreeMap<String, RankedSearchQuery>,
    limit: usize,
    offset: usize,
) -> FederatedSearchResult<'_>
```

---

## 3. Behavioral Specification

### 3.1 Chain Expansion

For **each** `chain_key` in the request, the server:

1. Opens the chain (lazy — uses the existing `get_chain` mechanism)
2. **Automatically includes ancestor chains** (via `discover_ancestor_chain_keys`) before running `query_ranked`
3. Runs `query_ranked` with the per-chain query

> **Note**: Ancestor inclusion is per-chain. If the caller explicitly passes both a branch and its ancestor, duplicates can occur. The global deduplication step (step 3) eliminates these.

### 3.2 Duplicate Elimination

- A `BTreeSet<Uuid>` tracks seen thought IDs during result collection
- Only the **first occurrence** (by the sort order below) is retained
- Sort order for tiebreaking: `total` score desc → `thought.index` asc

### 3.3 Result Merging

After collecting all per-chain hits:

1. **Sort** all hits by `total` score descending, then by `thought.index` ascending
2. **Deduplicate** by UUID, keeping the higher-scoring occurrence
3. **Apply offset + limit** for paging
4. **Record `chain_key`** on every retained hit for response construction

### 3.4 Cross-Chain Concerns

| Concern | Resolution |
|---------|------------|
| Caller passes both branch + ancestor | Deduplication handles it |
| Empty chain in list | Silently skipped; no error |
| Chain key not found | Silently skipped; no error |
| Same thought UUID in multiple chains (cross-chain relation) | Treated as duplicate — first occurrence wins |
| Ancestor chain has more relevant hits than the requested branch | Included transparently |

---

## 4. Edge Cases

| Case | Behavior |
|------|----------|
| `chain_keys` is empty | Return empty results with `total: 0` |
| All chains are empty | Return empty results with `total: 0` |
| `chain_keys` contains unknown key | Silently skip; include only found chains |
| `offset >= total` | Return empty `results` array |
| `limit == 0` | Return empty `results` array |
| Same thought ID appears in branch + ancestor | Deduplicate — keep the higher-scoring occurrence |
| All hits filtered out by `as_of` | Return empty results with `total: 0` |

---

## 5. Performance

### 5.1 Concurrent Fetching

- Each per-chain `query_ranked` call is executed in a separate `tokio::task::spawn_blocking`
- All searches run concurrently; the slowest chain determines latency
- The `DashMap` of chains means different chain keys don't contend

### 5.2 No New Caching Layer

- Results are computed fresh on each call (consistent with existing ranked search semantics)
- Future work may add a federated search cache layer keyed on `(chain_keys, query_signature)`

---

## 6. File Changes

### 6.1 `src/lib.rs`

- Add `FederatedSearchResult` struct (mirrors `RankedSearchResult` but with `chain_key` on each hit)
- Add `query_federated` method on `MentisDb`

### 6.2 `src/server.rs`

- Add `FederatedSearchRequest` deserialization struct
- Add `FederatedSearchResponse` serialization struct  
- Add `MentisDbService::federated_search` async method
- Add `POST /v1/federated-search` route to `rest_router` and `rest_router_with_service`
- Add `mentisdb_federated_search` branch in `MentisDbMcpProtocol::execute`
- Add `mentisdb_federated_search` entry to `mcp_tool_metadata`

### 6.3 New test file: `tests/federated_search_tests.rs`

- Test federated search over 2 chains with distinct thoughts
- Test that ancestor chains are included transparently
- Test duplicate elimination when branch + ancestor both have the same thought
- Test empty chain list → empty results
- Test unknown chain key → silently skipped
- Test offset/limit paging

---

## 7. Data Structures

### New Types

```rust
/// Federated search result — mirrors RankedSearchResult but carries chain_key
/// on each hit so callers know provenance.
#[derive(Debug, Clone)]
pub struct FederatedSearchResult<'a> {
    pub backend: RankedSearchBackend,
    pub total_candidates: usize,
    pub hits: Vec<FederatedSearchHit<'a>>,
}

#[derive(Debug, Clone)]
pub struct FederatedSearchHit<'a> {
    /// Chain key of the source chain for this hit.
    pub chain_key: &'a str,
    pub thought: &'a Thought,
    pub score: RankedSearchScore,
    pub graph_distance: Option<usize>,
    pub graph_seed_paths: usize,
    pub graph_relation_kinds: Vec<ThoughtRelationKind>,
    pub graph_path: Option<crate::search::GraphExpansionPath>,
    pub matched_terms: Vec<String>,
    pub match_sources: Vec<crate::search::lexical::LexicalMatchSource>,
}
```

### Request Struct (REST/MCP)

```rust
#[derive(Debug, Deserialize)]
struct FederatedSearchRequest {
    chain_keys: Vec<String>,
    text: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    graph: Option<RankedSearchGraphRequest>,
    thought_types: Option<Vec<String>>,
    roles: Option<Vec<String>>,
    tags_any: Option<Vec<String>>,
    concepts_any: Option<Vec<String>>,
    agent_ids: Option<Vec<String>>,
    agent_names: Option<Vec<String>>,
    agent_owners: Option<Vec<String>>,
    min_importance: Option<f32>,
    min_confidence: Option<f32>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    as_of: Option<DateTime<Utc>>,
    scope: Option<String>,
    enable_reranking: Option<bool>,
    rerank_k: Option<usize>,
    entity_type: Option<String>,
}
```

---

## 8. Acceptance Criteria

1. `POST /v1/federated-search` with `{"chain_keys": ["a", "b"], "text": "query"}` returns a merged ranked list with `chain_key` on each hit
2. `mentisdb_federated_search` MCP tool produces the same response shape
3. When the same thought UUID appears in multiple chains, only one hit appears in results
4. Empty `chain_keys` array returns `{"total": 0, "results": []}`
5. A chain key that doesn't exist is silently ignored (no 404)
6. Concurrent execution: searching 3 chains takes no longer than the slowest chain
7. Ancestor chains are included transparently when a branch chain is in the list
8. All existing tests pass (`cargo test --all-features`)
9. `cargo clippy --all-targets --all-features -- -D warnings` passes with no warnings
10. `cargo fmt` produces no format changes to modified files
