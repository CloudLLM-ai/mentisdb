# MentisDB White Paper

**Author:** Angel Leon

## 1. Abstract

Modern agent frameworks treat long-term memory as an afterthought. In practice, memory is reduced to ad hoc prompt stuffing, fragile `MEMORY.md` files, or proprietary session state that is hard to inspect, hard to transfer, and easy to lose or tamper with. MentisDB is a durable, semantically typed memory engine that replaces these ad hoc approaches with an append-only, hash-chained ledger designed for agents and multi-agent teams.

MentisDB stores thoughts -- structured, timestamped, typed, attributable records -- in an append-only hash chain. The chain model is storage-agnostic through a `StorageAdapter` interface, with binary (length-prefixed bincode) as the default backend. A dedicated ranked-retrieval layer combines BM25 lexical scoring, optional vector-semantic similarity, graph-aware expansion from typed relation edges, session cohesion signals, and importance weighting. Temporal edge validity (`valid_at`/`invalid_at`) enables point-in-time queries. Automatic deduplication via Jaccard similarity prevents near-duplicate pollution. Memory scopes (User/Session/Agent) provide visibility isolation without physical chain partitioning.

Benchmarks on standard long-term memory evaluations confirm the retrieval quality: LoCoMo 2-persona R@10 = 88.7%, LoCoMo 10-persona R@10 = 74.6% (0.8.5), LongMemEval R@5 = 67.6% / R@10 = 73.2%.

The system ships as a single Rust crate with an optional daemon (`mentisdbd`) exposing MCP, REST, and HTTPS dashboard surfaces. It requires no external databases, no LLM API keys for core operation, and no cloud dependencies.

## 2. Architecture

### 2.1 Core Data Model

The fundamental unit of memory is a `Thought`:

```rust
pub struct Thought {
    pub schema_version: u32,
    pub id: Uuid,
    pub index: usize,
    pub timestamp: DateTime<Utc>,
    pub agent_id: String,
    pub signing_key_id: Option<String>,
    pub thought_signature: Option<Vec<u8>>,
    pub thought_type: ThoughtType,
    pub role: ThoughtRole,
    pub content: String,
    pub tags: Vec<String>,
    pub concepts: Vec<String>,
    pub confidence: Option<f64>,
    pub importance: Option<f64>,
    pub scope: Option<MemoryScope>,
    pub refs: Vec<usize>,
    pub relations: Vec<ThoughtRelation>,
    pub prev_hash: String,
    pub hash: String,
}
```

A `ThoughtInput` is the caller-authored memory proposal. It contains the semantic payload but omits chain-managed fields (`index`, `timestamp`, `hash`, `prev_hash`). This prevents agents from forging chain mechanics.

### 2.2 Hash Chain

Each thought includes `prev_hash` (the hash of the preceding thought) and its own `hash`. The hash is computed over the canonical bincode serialization of the thought record. This creates a blockchain-style integrity ledger: modifying or removing any thought breaks the chain from that point forward.

This is a practical integrity mechanism for agent memory, not a public cryptocurrency system. The hash chain makes offline tampering detectable. Optional Ed25519 signing fields (`signing_key_id`, `thought_signature`) provide stronger provenance controls for agents that register public keys in the agent registry.

### 2.3 ThoughtRelation

Typed edges connect thoughts into a navigable graph:

```rust
pub struct ThoughtRelation {
    pub kind: ThoughtRelationKind,
    pub target_id: Uuid,
    pub chain_key: Option<String>,
    pub valid_at: Option<DateTime<Utc>>,
    pub invalid_at: Option<DateTime<Utc>>,
}
```

`chain_key` enables cross-chain references. `valid_at` and `invalid_at` define temporal validity windows for point-in-time queries.

### 2.4 Agent Registry

Agent profile metadata lives in a per-chain `AgentRegistry` sidecar rather than being duplicated inside every thought record. The registry stores:

- `display_name`, `agent_owner`, `description`
- `aliases` (historical or alternate names)
- `status` (active / revoked)
- `public_keys` (Ed25519 verification keys)
- per-chain activity counters (`thought_count`, `first_seen_index`, `last_seen_index`)

Thoughts carry only the stable `agent_id`. The registry resolves identity metadata at read time, keeping thought records small and the identity model consistent. The registry is administrable directly through library calls, MCP tools, and REST endpoints -- agents can be pre-registered, documented, disabled, or provisioned with keys before writing any thoughts.

## 3. Schema Evolution & Migration

Append-only memory still evolves. MentisDB versions its schemas and provides migration paths.

### 3.1 Schema Versions

| Version | Constant | Changes |
|---------|----------|---------|
| V0 | `MENTISDB_SCHEMA_V0` | Original format; no explicit `schema_version` field |
| V1 | `MENTISDB_SCHEMA_V1` | Adds explicit `schema_version`, optional `signing_key_id` and `thought_signature`, agent registry sidecar |
| V2 | `MENTISDB_SCHEMA_V2` | Adds `ThoughtType::Reframe`, `ThoughtRelationKind::Supersedes`, optional cross-chain `ThoughtRelation::chain_key` |
| V3 | `MENTISDB_SCHEMA_V3` | Adds temporal validity fields `ThoughtRelation::valid_at` and `ThoughtRelation::invalid_at` |

The current version constant is `MENTISDB_CURRENT_VERSION = MENTISDB_SCHEMA_V3`.

### 3.2 Per-Thought Version Detection

When loading a binary chain, MentisDB peeks at the first thought's `schema_version` field to determine the chain's version before deserializing the full record set. A bincode empty-Vec fast-path guard ensures that V0 chains (which lack the `schema_version` field entirely) are detected correctly: if deserialization succeeds but the resulting `schema_version` is 0 and the binary data is non-empty, the system recognizes it as a legacy chain and applies the appropriate migration path.

### 3.3 Migration Paths

- **V0 to V1**: `migrate_legacy_chain_v0()` detects the actual `from_version` and rewrites the chain with explicit `schema_version`, hash chain rebuild, and agent registry sidecar creation.
- **V1 to V2**: `migrate_v1_thoughts()` adds the `Reframe` and `Supersedes` enum variants and the `chain_key` field on `ThoughtRelation`. `LegacyThoughtRelation` (2-field: `kind`, `target_id`) is deserialized and upgraded to 3-field `ThoughtRelation` with `chain_key = None`.
- **V2 to V3**: `migrate_v2_thoughts()` adds `valid_at` and `invalid_at` fields. `LegacyThoughtRelationV2` (3-field) is deserialized and upgraded to 5-field `ThoughtRelation` with both temporal fields defaulting to `None`. Hash chain is rebuilt after migration.

Migrated chains are persisted on disk so subsequent opens use the native format. All migrations are idempotent -- safe to run repeatedly.

### 3.4 Mixed-Schema Chain Handling

The daemon startup sequence:

1. Scans discovered chains for their schema version
2. Migrates legacy chains to the current schema
3. Reconciles older active files into the configured default storage adapter
4. Attempts repair when the expected active file is missing or invalid but another valid local source exists
5. Migrates the skill registry from V1 to V2 format if needed (idempotent)
6. Migrates chain relations from V2 to V3 format to add temporal edge validity (idempotent)

After migration, a chain registry records each chain's schema version, storage adapter, thought count, and agent count.

## 4. Semantic Memory Model

### 4.1 ThoughtType (30 Variants)

`ThoughtType` classifies what a memory means:

| Category | Variants |
|----------|----------|
| User/relationship | `PreferenceUpdate`, `UserTrait`, `RelationshipUpdate` |
| Observation | `Finding`, `Insight`, `FactLearned`, `PatternDetected`, `Hypothesis`, `Surprise` |
| Error/correction | `Mistake`, `Correction`, `LessonLearned`, `AssumptionInvalidated`, `Reframe` |
| Planning | `Constraint`, `Plan`, `Subgoal`, `Goal`, `Decision`, `StrategyShift` |
| Exploration | `Wonder`, `Question`, `Idea`, `Experiment` |
| Execution | `ActionTaken`, `TaskComplete` |
| State | `Checkpoint`, `StateSnapshot`, `Handoff`, `Summary` |

New variants are always appended at the end of the enum because bincode encodes variants by integer index; inserting mid-enum would corrupt persisted data.

### 4.2 ThoughtRole (8 Values)

`ThoughtRole` classifies how the system uses a memory:

| Role | Semantics |
|------|-----------|
| `Memory` | Durable long-term memory (default) |
| `WorkingMemory` | Shorter-lived or speculative working memory |
| `Summary` | Synthesized summary |
| `Compression` | Emitted during context compression |
| `Checkpoint` | Resumption checkpoint |
| `Handoff` | Context handed to another actor |
| `Audit` | Traceability or audit log |
| `Retrospective` | Deliberate post-incident reflection |

The type/role separation avoids mixing semantics with workflow mechanics. A hard-won fix might be stored as `Mistake` / `Correction` / `LessonLearned` (type) with role `Retrospective`, letting future agents retrieve not just what happened, but what they should do differently next time.

### 4.3 ThoughtRelationKind (11 Values)

| Kind | Semantics |
|------|-----------|
| `References` | General back-reference |
| `Summarizes` | Source summarizes target |
| `Corrects` | Source corrects target's factual error |
| `Invalidates` | Source invalidates target (correct but stale) |
| `CausedBy` | Source was caused by target |
| `Supports` | Source supports target's claim |
| `Contradicts` | Source contradicts target |
| `DerivedFrom` | Source was derived from target |
| `ContinuesFrom` | Source continues work from target |
| `RelatedTo` | Generic semantic connection |
| `Supersedes` | Source replaces target's framing (not an error; use `Reframe` as type) |

`Supersedes` is particularly important for deduplication and temporal fact management: it marks a thought as replacing a prior thought without the prior being a clear error.

### 4.4 Temporal Edge Validity

Schema V3 adds `valid_at` and `invalid_at` to `ThoughtRelation`:

- `valid_at`: when the relation became valid (auto-set to `now` on append if not provided)
- `invalid_at`: when the relation stopped being valid

Edges without temporal bounds are always included. The `as_of` parameter on `RankedSearchQuery` restricts graph expansion to only edges whose validity window covers the given timestamp, enabling queries like "what did the agent know at the start of the sprint?"

### 4.5 Invalidated Thought IDs

At chain open time, MentisDB builds `invalidated_thought_ids: HashSet<Uuid>` from all `Supersedes`, `Corrects`, and `Invalidates` relations. This provides O(1) superseded detection during retrieval -- ranked search skips superseded thoughts in constant time without walking the full relation graph.

## 5. Storage Layer

### 5.1 StorageAdapter Trait

```rust
pub trait StorageAdapter: Send + Sync {
    fn load_thoughts(&self) -> io::Result<Vec<Thought>>;
    fn append_thought(&self, thought: &Thought) -> io::Result<()>;
    fn flush(&self) -> io::Result<()>;
    fn set_auto_flush(&self, auto_flush: bool) -> io::Result<()>;
    fn storage_location(&self) -> String;
    fn storage_kind(&self) -> StorageAdapterKind;
    fn storage_path(&self) -> Option<&Path>;
}
```

### 5.2 BinaryStorageAdapter

The default and only supported backend for new chains. Each record is a length-prefixed bincode-serialized `Thought`:

```
[4-byte LE length][bincode-encoded Thought][4-byte LE length][bincode-encoded Thought]...
```

File extension: `.tcbin`.

Write buffering modes:

- **Strict** (`auto_flush = true`, default): appends are queued to a dedicated background writer; the caller blocks until the writer flushes to the OS. Concurrent requests share a short group-commit window (configurable via `MENTISDB_GROUP_COMMIT_MS`, default 2ms), preserving durable-ack semantics while reducing contention.
- **Buffered** (`auto_flush = false`): appends are handed to a bounded background-writer queue. The worker batches records and flushes every 16 entries (`FLUSH_THRESHOLD`). Up to 15 thoughts may be lost on a hard crash, but write throughput increases significantly for multi-agent hubs with many concurrent writers.

### 5.3 Legacy JSONL Adapter

`LegacyJsonlReadAdapter` is a read-only adapter for migrating legacy `.jsonl` chains. It cannot be used for new chains. The `StorageAdapterKind::Jsonl` variant is retained in the registry schema for backward compatibility.

### 5.4 File Layout

```
~/.mentisdb/
  mentisdb-registry.json          # chain registry (keys, versions, counts)
  mentisdb-skills.bin             # skill registry (binary)
  <chain-key>.tcbin               # binary thought chain
  <chain-key>.agents.json         # per-chain agent registry sidecar
  <chain-key>.vectors.bin         # vector sidecar (optional, per embedding provider)
  tls/
    cert.pem
    key.pem
```

## 6. Query & Retrieval

MentisDB separates filter-first baseline search from scored ranked retrieval.

### 6.1 Baseline Search (ThoughtQuery)

`ThoughtQuery` / `POST /v1/search` / `mentisdb_search`:

- Indexed filters narrow candidates by `thought_type`, `role`, `agent_id`, tags, and concepts
- `text` is a case-insensitive substring match over content, agent metadata, tags, and concepts
- Results return in append order
- `limit` keeps the newest matching tail after filtering (not ranked)

This path is deterministic and explainable. It is not BM25, hybrid, or vector retrieval.

### 6.2 Ranked Search (RankedSearchQuery)

`RankedSearchQuery` / `POST /v1/ranked-search` / `mentisdb_ranked_search`:

```rust
let ranked = RankedSearchQuery::new()
    .with_filter(ThoughtQuery::new().with_types(vec![ThoughtType::Decision]))
    .with_text("latency ranking")
    .with_graph(RankedSearchGraph::new().with_max_depth(1))
    .with_as_of("2025-06-01T00:00:00Z")
    .with_scope(MemoryScope::Session)
    .with_limit(10);
```

Backend selection:

| Condition | Backend |
|-----------|---------|
| Non-empty `text`, no vector sidecar | `Lexical` |
| Non-empty `text`, vector sidecar active | `Hybrid` |
| Non-empty `text`, graph enabled, no vector | `LexicalGraph` |
| Non-empty `text`, graph enabled, vector active | `HybridGraph` |
| Absent/blank `text` | `Heuristic` |

### 6.3 BM25 Lexical Scoring

BM25 (Best Matching 25) is a probabilistic ranking function that scores documents by term frequency and document frequency. It rewards terms that appear frequently in a document (high TF) while penalizing terms that appear in many documents across the corpus (low IDF), and normalizes for document length. MentisDB applies BM25 per indexed field (content, tags, concepts, agent id, agent registry) with configurable field weights, then sums the field scores into a single lexical score.

The lexical tokenizer applies Porter stemming before indexing and querying so word variants share a common root (`prefers`/`preferred`/`preferences` all map to `prefer`). An irregular verb lemma map (~170 entries) expands query-time tokens: `went` also matches `go`-stemmed documents, `saw` also matches `see`, and so on.

Per-field document-frequency cutoffs prevent non-discriminative terms from dominating scores. Each field has its own DF ratio threshold:

| Field | Cutoff | Rationale |
|-------|--------|-----------|
| Content | 30% | Default; content is large and stop-word prone |
| Tags | 50% | Tag vocabularies are smaller and more uniform |
| Concepts | 40% | Concepts are curated, less noisy |
| Agent ID | 70% | Agent IDs are inherently repetitive |
| Agent Registry | 60% | Registry text is short and repetitive |

A term that exceeds the cutoff in one field is still scored in other fields where it remains discriminative — e.g., a term at 35% content DF is filtered from content scoring but still contributes via tags if its tag DF is below 50%. The cutoff only activates when the corpus has 20+ documents.

### 6.4 Vector-Lexical Fusion

When a managed vector sidecar (e.g., `fastembed-minilm` via ONNX) is active, ranked search blends lexical and semantic signals using smooth exponential decay:

```
vector_score * (1 + 35 * exp(-lexical_score / 3.0))
```

Pure-semantic matches receive ~36x amplification. By `lexical = 3.0` the boost has decayed to ~12x. At `lexical = 6.0` it is additive. This eliminates the discontinuities of earlier step-function boost tiers.

### 6.5 Graph-Aware Expansion

When `graph` is enabled, expansion starts from lexical seed hits and walks `refs` and typed `relations` bidirectionally. Graph-expanded hits expose:

- `graph_distance` (hops from seed)
- `graph_seed_paths` (which seeds led here)
- `graph_relation_kinds` (which relation types were traversed)
- `graph_path` (full traversal provenance)

`MAX_GRAPH_SEEDS = 20` bounds BFS cost. Relation-kind boosts: `ContinuesFrom` = 0.30, `Corrects`/`Invalidates` = 0.25, `Supersedes` = 0.22, `DerivedFrom` = 0.20. Graph proximity score = 1.0 / depth.

### 6.6 Session Cohesion Scoring

Thoughts within +/-8 positions of a high-scoring lexical seed (score >= 3.0) receive a proximity boost up to 0.8, decaying linearly with distance. This surfaces evidence turns adjacent to the matching turn but sharing no lexical terms. Seeds with `lexical >= 5.0` are excluded from the boost (strong enough to stand alone).

### 6.7 Importance Weighting

Replaces flat multipliers with differential boost proportional to lexical score:

```
lexical_score * (importance - 0.5) * 0.3
```

User-originated thoughts (`importance` ~0.8) outrank verbose assistant responses (`importance` ~0.2) in close BM25 races.

### 6.8 Reciprocal Rank Fusion (RRF)

When `enable_reranking` is set on a `RankedSearchQuery`, ranked search runs a second scoring pass over the top `rerank_k` candidates (default 50). Three single-signal rankings are produced:

1. **Lexical-only** — sorted by BM25 score, vector set to 0
2. **Vector-only** — sorted by cosine similarity, lexical set to 0
3. **Graph-only** — sorted by `graph + relation + seed_support`

These three rank lists are merged via Reciprocal Rank Fusion:

```
score(d) = Σ 1 / (k + rank_i(d))    where k = 60
```

Documents appearing in multiple lists accumulate contributions from each. The RRF total replaces the original additive total. Non-rankable signals (importance, confidence, recency, session cohesion, graph) are added back as small additive adjustments on top of the RRF score so they can still break ties without overriding the rank-based fusion.

RRF is pure arithmetic — no LLM, no model, no external service. It is opt-in via `with_reranking(rerank_k)` on the query builder or `enable_reranking` / `rerank_k` in the REST/MCP request.

### 6.9 As-Of Point-in-Time Queries

`RankedSearchQuery::with_as_of(rfc3339)` restricts graph expansion to only relation edges whose `valid_at`/`invalid_at` window covers the given timestamp. Thoughts appended after the `as_of` timestamp are excluded. Thoughts superseded by thoughts appended at or before the timestamp are also excluded (via `invalidated_thought_ids`).

### 6.10 Memory Scopes

Three visibility levels stored as `scope:{variant}` tags:

| Scope | Tag | Visibility |
|-------|-----|------------|
| `User` (default) | `scope:user` | All agents sharing the chain |
| `Session` | `scope:session` | Current session only |
| `Agent` | `scope:agent` | Creating agent only |

`RankedSearchQuery::with_scope(MemoryScope)` filters results by scope. Omitting scope returns thoughts from all scopes.

### 6.11 Context Bundles

`query_context_bundles` / `mentisdb_context_bundles` returns seed-anchored grouped support context instead of a flat list. Each bundle contains one lexical seed and its supporting graph-expanded neighbors in deterministic order, making it easy for agents to understand why supporting thoughts surfaced.

### 6.12 Vector Sidecars

Vector state lives in rebuildable per-chain sidecars, never in the canonical chain. Sidecars are separated by `chain_key`, `thought_id`, `thought_hash`, `model_id`, dimension, and embedding version. Changing the model or version invalidates old vector state instead of silently mixing incompatible embeddings.

Managed sidecars (registered via `manage_vector_sidecar`) stay synchronized on append. The daemon defaults to the built-in `fastembed-minilm` provider (ONNX, local, no cloud dependencies).

### 6.13 Score Breakdown

Each ranked hit includes decomposed scores:

```json
{
  "score": {
    "lexical": 2.91,
    "vector": 0.27,
    "graph": 0.18,
    "relation": 0.05,
    "seed_support": 0.0,
    "importance": 0.0,
    "confidence": 0.0,
    "recency": 0.0,
    "session_cohesion": 0.4,
    "total": 3.14
  },
  "matched_terms": ["latency", "ranking"],
  "match_sources": ["content", "tags", "agent_registry"]
}
```

## 7. Memory Deduplication

When `MENTISDB_DEDUP_THRESHOLD` is set (0.0-1.0), each new thought's normalized lexical tokens are compared against recent thoughts using Jaccard similarity.

### 7.1 Algorithm

1. Tokenize and normalize the new thought's content
2. Scan the last `MENTISDB_DEDUP_SCAN_WINDOW` thoughts (default: 64)
3. Compute Jaccard similarity: `|A intersection B| / |A union B|`
4. If the best match exceeds the threshold, auto-create a `Supersedes` relation pointing to the most similar prior thought
5. Update `invalidated_thought_ids` for O(1) exclusion in future ranked search

### 7.2 Configuration

```bash
MENTISDB_DEDUP_THRESHOLD=0.85     # similarity threshold (0.0-1.0)
MENTISDB_DEDUP_SCAN_WINDOW=64     # how many recent thoughts to scan
```

Library API: `MentisDb::with_dedup_threshold()` and `with_dedup_scan_window()`.

The superseded thought is retained for audit. Ranked search simply deprioritizes it. No content is deleted or overwritten.

## 8. CLI Subcommands

`mentisdbd` provides three subcommands that talk to a running daemon via its REST endpoint (default `http://127.0.0.1:9472`):

### 8.1 add

```bash
mentisdbd add "The cache uses LRU eviction" \
  --type decision \
  --scope session \
  --tag caching \
  --agent planner \
  --chain my-project \
  --url http://127.0.0.1:9472
```

Appends a thought to the specified chain. Defaults to `FactLearned` type and `user` scope. Uses `ureq` for synchronous HTTP (no async runtime needed).

### 8.2 search

```bash
mentisdbd search "cache invalidation" \
  --limit 5 \
  --scope session \
  --chain my-project \
  --url http://127.0.0.1:9472
```

Invokes the REST ranked-search endpoint and prints results with score breakdowns, matched terms, and match sources.

### 8.3 agents

```bash
mentisdbd agents --chain my-project --url http://127.0.0.1:9472
```

Lists the distinct agent identities writing to the specified chain.

## 9. MCP Integration

`mentisdbd` exposes a standard streamable HTTP MCP endpoint at `POST /` (default port 9471) plus legacy compatibility endpoints at `POST /tools/list` and `POST /tools/execute`.

### 9.1 Tool Catalog

34 MCP tools are currently exposed, covering:

- **Bootstrap & append**: `mentisdb_bootstrap`, `mentisdb_append`, `mentisdb_append_retrospective`
- **Search**: `mentisdb_search`, `mentisdb_lexical_search`, `mentisdb_ranked_search`, `mentisdb_context_bundles`
- **Read**: `mentisdb_get_thought`, `mentisdb_get_genesis_thought`, `mentisdb_traverse_thoughts`, `mentisdb_recent_context`, `mentisdb_head`
- **Export/import**: `mentisdb_memory_markdown`, `mentisdb_import_memory_markdown`, `mentisdb_skill_md`
- **Agent registry**: `mentisdb_list_agents`, `mentisdb_get_agent`, `mentisdb_list_agent_registry`, `mentisdb_upsert_agent`, `mentisdb_set_agent_description`, `mentisdb_add_agent_alias`, `mentisdb_add_agent_key`, `mentisdb_revoke_agent_key`, `mentisdb_disable_agent`
- **Chain management**: `mentisdb_list_chains`, `mentisdb_merge_chains`
- **Skill registry**: `mentisdb_list_skills`, `mentisdb_skill_manifest`, `mentisdb_upload_skill`, `mentisdb_search_skill`, `mentisdb_read_skill`, `mentisdb_skill_versions`, `mentisdb_deprecate_skill`, `mentisdb_revoke_skill`

### 9.2 Skill Registry

The skill registry is a git-like immutable version store for agent instruction bundles. Skills are authored in Markdown or JSON and uploaded by agents with a stable `skill_id`. Every upload to an existing `skill_id` creates a new immutable version. The first version stores full content; subsequent versions store unified diff patches. Version reconstruction replays patches from v0 forward. Content hashes are computed over reconstructed content, making integrity checks independent of storage representation.

Agents with registered Ed25519 public keys must cryptographically sign uploads. Signature verification is enforced server-side before acceptance. Agents without keys may upload without signatures for backward compatibility.

### 9.3 MCP Bootstrap Flow

Modern MCP clients bootstrap from the MCP handshake:

1. `initialize.instructions` tells the agent to read `mentisdb://skill/core`
2. `resources/read(mentisdb://skill/core)` delivers the embedded operating skill
3. `mentisdb_bootstrap` creates or opens the chain and writes a checkpoint if empty
4. `mentisdb_recent_context` loads prior state for session resumption

## 10. Benchmarks

### 10.1 Standard Evaluation Results

| Benchmark | Metric | 0.8.1 | 0.8.5 |
|-----------|--------|-------|-------|
| LoCoMo 2-persona | R@10 | 88.7% | — |
| LoCoMo 2-persona single-hop | R@10 | 90.7% | — |
| LoCoMo 10-persona (1977 queries) | R@10 | 74.2% | **74.6%** |
| LoCoMo 10-persona single-hop | R@10 | — | **79.0%** |
| LoCoMo 10-persona multi-hop | R@10 | — | **58.4%** |
| LongMemEval | R@5 | 67.6% | — |
| LongMemEval | R@10 | 73.2% | — |

The 0.8.5 LoCoMo 10-persona improvement comes from three tuning changes:

1. **Session cohesion** — radius 8→12, boost 0.8→1.2. Adjacent thoughts to a lexical
   match get a stronger signal, pushing near-misses into the top-10.
2. **Graph relation scores** — doubled across all `ThoughtRelationKind` variants.
   `ContinuesFrom` 0.30→0.60, `Corrects`/`Invalidates` 0.25→0.50, etc.
3. **FastEmbed sentence embeddings** — the benchmark now uses `fastembed-minilm`
   vectors instead of text-only hashing when the `local-embeddings` feature is compiled.

### 10.2 Near-Miss Analysis (LoCoMo 10-persona, 0.8.5)

Of 503 misses (correct answer not in top-10):

| Bucket | Count | % | Interpretation |
|--------|-------|---|----------------|
| In top-20 | 130 | 25.8% | Ranking problem — close to fixable |
| In top-50 | 285 | 56.7% | Moderate gap — needs more signal |
| Not in top-50 | 218 | 43.3% | Lexical gap — query terms absent from evidence |

The 43.3% lexical gap represents the hard ceiling for BM25-only retrieval on this
benchmark. Addressing it requires stronger stemming (irregular verb lemmas), larger
embedding models, or query expansion via LLM.

### 10.3 Scoring Evolution

| Version | Change | LongMemEval R@5 | LoCoMo 10p R@10 |
|---------|--------|-----------------|-----------------|
| 0.8.0 baseline | — | 57.2% | — |
| 0.8.0 + Porter stemming | Token normalization | 61.6% | — |
| 0.8.0 + tiered fusion + importance | Vector/lexical balance | 65.0% | — |
| 0.8.1 + session cohesion + smooth fusion + DF cutoff | Retrieval quality | 67.6% | 74.2% |
| 0.8.5 + cohesion tuning + graph scores + fastembed | Session/graph boost | — | **74.6%** |

### 10.3 Criterion Micro-Benchmarks

- `benches/thought_chain.rs` -- 10 benchmarks: append throughput, query latency, traversal
- `benches/search_baseline.rs` -- 4 benchmarks: lexical/filter-first baselines
- `benches/search_ranked.rs` -- 4 benchmarks: ranked retrieval, heuristic fallback
- `benches/skill_registry.rs` -- 12 benchmarks: skill upload, search, delta reconstruction, lifecycle
- `benches/http_concurrency.rs` -- write/read throughput at 100/1k/10k concurrent Tokio tasks (p50/p95/p99)

The `DashMap` concurrent chain lookup refactor delivers 750-930 read req/s at 10k concurrent tasks, compared to the sequential bottleneck on the previous `RwLock<HashMap>`.

## 11. Competitive Landscape

| Feature | MentisDB | Mem0 | Graphiti/Zep | Letta/MemGPT |
|---------|----------|------|--------------|--------------|
| Language | Rust | Python | Python | Python/TS |
| Storage | Embedded (file) | External DB | External DB (Neo4j/FalkorDB) | External DB |
| LLM required for core | No | Yes | Yes | Yes |
| Cryptographic integrity | Hash chain | No | No | No |
| Hybrid retrieval | BM25+vec+graph | vec+keyword | semantic+kw+graph | No |
| Temporal facts | valid_at/invalid_at (0.8.2) | Updates only | valid_at/invalid_at | No |
| Memory dedup | Jaccard similarity | LLM-based | Merge | No |
| Agent registry | Yes | No | No | Yes |
| MCP server | Built-in | No | Yes | No |
| CLI tool | add/search/agents | Yes | No | Yes |

**MentisDB's differentiators**: the only system that combines embedded storage, no LLM dependency, cryptographic integrity, and hybrid BM25+vector+graph retrieval in a single static binary. Competitors require external databases and LLM API keys for core ingestion. MentisDB works offline, in air-gapped environments, and at scale without external infrastructure.

**Gaps**: custom entity/relation ontologies (Graphiti's Pydantic models), LLM-extracted memories (Mem0/Graphiti), browser extension (Mem0), and token tracking.

## 12. Future Direction

### 0.8.3 -- Retrieval Quality

- **Irregular verb lemmas**: extend the Porter stemmer with a lookup table for common irregular verbs (`ran`/`run`, `went`/`go`) to improve recall on conversational phrasing
- **RRF (Reciprocal Rank Fusion) reranking**: blend lexical and vector rank positions using RRF instead of the current score-level fusion, providing more robust cross-signal combination
- **Per-field BM25 DF cutoffs**: apply document-frequency filtering independently per indexed field (content vs. tags vs. concepts vs. agent metadata) instead of a single corpus-wide threshold

### 0.8.4 -- Ontology & Provenance

- Custom entity/relation types per chain
- Episode provenance tracking from derived facts back to source conversations

### 0.9.0 -- Ecosystem

- Cross-chain queries
- Optional LLM-extracted memories
- LangChain integration
- Webhooks

### 1.0.0 -- Production Stable

- Browser extension
- Self-improving agent primitives
- Token tracking
- API stability guarantees

---

**Angel Leon**
