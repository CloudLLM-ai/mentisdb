# MentisDB: Durable Semantic Memory for Software Agents

**Author:** Angel Leon
**Version:** 0.10.6.51
**Date:** 2026-08-15

---

## Abstract

Most agent frameworks treat long-term memory as an afterthought — ad-hoc prompt stuffing,
unstructured Markdown files, or opaque session state that's non-portable and easily lost.
MentisDB is a durable, semantically typed memory engine built in Rust that treats agent
memory as an append-only, hash-chained ledger of structured records called *thoughts*.

Every thought is cryptographically linked to its predecessor via SHA-256, making the chain
tamper-evident. On top of this ledger, MentisDB provides a retrieval pipeline that combines
BM25 lexical scoring, vector similarity, typed graph expansion, implicit edge inference,
and Reciprocal Rank Fusion (RRF) — all without requiring an external database or LLM service
in the core path.

On standard long-term memory benchmarks, MentisDB achieves 72.6% Recall@10 on LoCoMo-10P
and 66.8% Recall@5 on LongMemEval. The implementation ships as a single Rust crate with an
optional daemon exposing MCP, REST, and HTTPS surfaces.

---

## 1. System Overview

### 1.1 The Problem

LLM agents need memory that is:

- **Durable** — survives process restarts and context window resets
- **Queryable** — not just a flat file, but searchable by text, semantics, and graph relationships
- **Tamper-evident** — you can detect if someone modified the memory log
- **Portable** — works with Claude Code, Codex, Copilot, Cursor, or any MCP client
- **Attributable** — every record carries the agent ID that wrote it

Existing solutions fail one or more of these. Markdown files aren't queryable. Provider
session state isn't portable. Vector databases don't have integrity guarantees. LLM-based
memory extraction adds latency and cost to every write.

### 1.2 The Approach

MentisDB models agent memory as a hash-chained append-only log — similar to a blockchain,
but without consensus or distributed systems complexity. Each entry (a "thought") is a
typed, structured record with semantic metadata, signed by the agent that produced it.

Retrieval is a multi-signal pipeline: lexical search (BM25), vector similarity (cosine
over ONNX embeddings), graph traversal (typed edges between thoughts), and rank fusion
(RRF). All of this runs locally — no network calls, no LLM API bills for the core path.

### 1.3 Architecture at a Glance

```
┌─────────────────────────────────────────────────────────┐
│                     Agent (Claude, Codex, ...)          │
│                    via MCP / REST / CLI                  │
└────────────────────────┬────────────────────────────────┘
                         │
              ┌──────────▼──────────┐
              │   mentisdb daemon   │
              │  (single binary)    │
              ├─────────────────────┤
              │  MCP server (:9471) │
              │  REST API (:9472)   │
              │  Dashboard (:9473)  │
              ├─────────────────────┤
              │  MentisDb core      │
              │  ├─ Hash-chained    │
              │  │  thought ledger  │
              │  ├─ BM25 + vector   │
              │  │  + graph search  │
              │  ├─ Dedup (Jaccard) │
              │  └─ Agent registry  │
              ├─────────────────────┤
              │  Storage adapters   │
              │  ├─ Binary (.tcbin) │
              │  └─ JSON-L (legacy) │
              └──────────┬──────────┘
                         │
              ┌──────────▼──────────┐
              │   ~/.cloudllm/      │
              │   mentisdb/         │
              │   ├─ *.tcbin        │
              │   ├─ *.agents.json  │
              │   ├─ *.vectors.bin  │
              │   └─ *.auto_edges   │
              └─────────────────────┘
```

The daemon is a single static binary with no external dependencies. Storage is embedded
files on disk. No database server, no Redis, no Neo4j.

---

## 2. The Thought Data Model

### 2.1 The Thought Record

The core unit of memory is a *thought* — a structured, typed, hash-chained record. Here's
a simplified view of the Rust struct:

```rust
struct Thought {
    // Schema version for forward-compatible migrations
    schema_version: u32,

    // Identity (assigned by the chain on commit)
    thought_id: Uuid,           // stable UUID
    index: u64,                 // append-order position (0, 1, 2, ...)
    timestamp: DateTime<Utc>,   // commit time

    // Attribution
    agent_id: String,           // who wrote this (stable ID, resolved via agent registry)
    signing_key_id: Option<String>,    // optional Ed25519 key ID
    thought_signature: Option<Vec<u8>>, // optional detached signature

    // Semantics
    thought_type: ThoughtType,  // what kind of memory (Decision, Mistake, Insight, ...)
    thought_role: ThoughtRole,  // how the system uses it (Memory, Checkpoint, Handoff, ...)
    content: String,            // the actual memory text
    tags: Vec<String>,          // free-form tags
    concepts: Vec<String>,      // semantic concept labels

    // Scoring metadata
    confidence: Option<f64>,    // 0.0–1.0
    importance: Option<f64>,    // 0.0–1.0

    // Relationships
    refs: Vec<u64>,             // positional back-references (append-order indices)
    relations: Vec<ThoughtRelation>, // typed edges to other thoughts

    // Chain integrity (assigned by the chain on commit)
    prev_hash: String,          // SHA-256 of the previous thought
    hash: String,               // SHA-256 of this thought (minus the hash field itself)
}
```

The key design decision: the agent authors the *content* fields (type, role, content,
tags, concepts, relations) but cannot forge the *chain* fields (id, index, timestamp,
hashes). Those are assigned by the chain on commit.

### 2.2 The Hash Chain

Every thought is linked to its predecessor via SHA-256. Think of it as a git-like
history, but for memory records instead of source code:

```rust
// Simplified: how the chain computes a thought's hash
fn compute_thought_hash(thought: &Thought) -> String {
    // Serialize everything EXCEPT the hash field itself
    // using a canonical binary format (bincode)
    let canonical_bytes = bincode::serialize(&CanonicalThought {
        schema_version: thought.schema_version,
        thought_id: thought.thought_id,
        index: thought.index,
        // ... all fields except `hash`
        prev_hash: thought.prev_hash.clone(),
        // hash is intentionally excluded
    });

    // SHA-256 of the canonical bytes
    sha256(&canonical_bytes)
}
```

The integrity invariant is simple:

```rust
// Every thought must satisfy:
//   thought.hash == sha256(canonical_serialize(thought_without_hash))
//   thought.prev_hash == previous_thought.hash
```

If someone modifies any field of any thought, the hash breaks. If they recompute the
hash to fix it, the *next* thought's `prev_hash` no longer matches — so the tamper
cascades forward through the entire chain. Detecting tampering is O(1): just check
that each record's `prev_hash` matches the previous record's `hash`.

This is tamper *evidence*, not tamper *prevention*. An attacker with write access to
the file can rebuild the whole chain. For stronger provenance, individual thoughts can
be Ed25519-signed by the producing agent using a registered public key.

### 2.3 Typed Relations

Thoughts can link to each other via typed edges. There are 12 relation kinds:

```rust
enum ThoughtRelationKind {
    References,      // generic link
    Summarizes,      // this thought summarizes another
    Corrects,        // factual correction of a prior thought
    Invalidates,     // marks a prior thought as no longer valid
    CausedBy,        // causal link
    Supports,        // evidence for
    Contradicts,     // evidence against
    DerivedFrom,     // computed from
    ContinuesFrom,   // sequential continuation
    BranchesFrom,    // chain branch point
    RelatedTo,       // semantic similarity (used by implicit edges)
    Supersedes,      // replaces a prior framing (non-error, e.g. updated plan)
}
```

Each relation carries an optional validity interval (`valid_at` / `invalid_at`), enabling
point-in-time queries: "what did the agent know at timestamp T?"

```rust
struct ThoughtRelation {
    kind: ThoughtRelationKind,
    target_thought_id: Uuid,         // which thought this edge points to
    target_chain_key: Option<String>, // for cross-chain relations
    valid_at: Option<DateTime<Utc>>,  // when this edge became true
    invalid_at: Option<DateTime<Utc>>, // when this edge stopped being true
}
```

### 2.4 The Invalidation Set

When a thought is superseded, corrected, or invalidated, we don't delete it — we mark it.
At chain open time, we precompute a set of all invalidated thought IDs:

```rust
// Pseudocode: build the invalidation set at chain open
fn build_invalidation_set(chain: &[Thought]) -> HashSet<Uuid> {
    let mut invalidated = HashSet::new();
    for thought in chain {
        for relation in &thought.relations {
            match relation.kind {
                ThoughtRelationKind::Supersedes
                | ThoughtRelationKind::Corrects
                | ThoughtRelationKind::Invalidates => {
                    invalidated.insert(relation.target_thought_id);
                }
                _ => {}
            }
        }
    }
    invalidated
}
```

During retrieval, checking "is this thought superseded?" is an O(1) HashSet lookup.
Default search, ranked search, context bundles, and related read paths exclude
those IDs unless the caller sets `include_invalidated=true`. Point-in-time
queries (`as_of`) apply the same set with temporal edge validity so a thought
that was still current at that timestamp remains visible.

### 2.5 Agent Registry

To avoid duplicating identity metadata in every thought, MentisDB maintains a per-chain
agent registry. Thoughts carry only the stable `agent_id` string; display names, owners,
descriptions, aliases, public keys, and thought counters live in the registry:

```rust
struct AgentRecord {
    agent_id: String,
    display_name: Option<String>,
    owner: Option<String>,
    description: Option<String>,
    aliases: Vec<String>,
    status: AgentStatus,           // Active or Revoked
    public_keys: Vec<PublicKey>,
    thought_count: u64,            // updated as thoughts are appended
}
```

The registry is administrable through library calls, MCP tools, and REST endpoints,
allowing pre-registration before any thoughts are written.

---

## 3. Semantic Typing

### 3.1 ThoughtType — What Kind of Memory

Every thought carries a semantic type from a 30-variant enum, organized into seven
categories:

```rust
enum ThoughtType {
    // User / relationship
    PreferenceUpdate, UserTrait, RelationshipUpdate,

    // Observation
    Finding, Insight, FactLearned, PatternDetected,
    Hypothesis, Surprise,

    // Error / correction
    Mistake, Correction, LessonLearned, AssumptionInvalidated, Reframe,

    // Planning
    Constraint, Plan, Subgoal, Goal, Decision, StrategyShift,

    // Exploration
    Wonder, Question, Idea, Experiment,

    // Execution
    ActionTaken, TaskComplete,

    // State
    Checkpoint, StateSnapshot, Handoff, Summary,
}
```

This lets agents and retrieval code distinguish "the user changed their preference" from
"the agent made a mistake" from "this is a checkpoint for context compaction."

### 3.2 ThoughtRole — How the System Uses It

Orthogonal to type, every thought has a role that tells the system how to handle it:

```rust
enum ThoughtRole {
    Memory,         // standard durable memory
    WorkingMemory,  // short-term, may be pruned
    Summary,        // compressed representation of prior thoughts
    Compression,    // compression artifact
    Checkpoint,     // save point for context restoration
    Handoff,        // handoff to another agent
    Audit,          // audit trail entry
    Retrospective,  // post-hoc lesson
}
```

The combination of type × role gives 240 distinguishable semantic positions. A
retrospective lesson is `(LessonLearned, Retrospective)`. A context-compression checkpoint
is `(Summary, Checkpoint)`.

---

## 4. Storage Layer

### 4.1 Storage Adapter Trait

Persistence is abstracted behind a trait, allowing the chain semantics to stay the same
regardless of backend:

```rust
trait StorageAdapter {
    fn load_thoughts(&self) -> io::Result<Vec<Thought>>;
    fn append_thought(&self, thought: &Thought) -> io::Result<()>;
    fn flush(&self) -> io::Result<()>;
    fn set_auto_flush(&self, auto: bool);
}
```

### 4.2 Binary Storage (Default)

The default backend (`BinaryStorageAdapter`) writes each thought as a length-prefixed
bincode record:

```
[4-byte LE length] [bincode(thought_0)]
[4-byte LE length] [bincode(thought_1)]
[4-byte LE length] [bincode(thought_2)]
...
```

File extension: `.tcbin`. This format is compact (typically 40–60% smaller than JSON) and
deserializes faster than text-based formats.

Two durability modes:

| Mode | Behavior | Tradeoff |
|------|----------|----------|
| **Strict** (`auto_flush = true`) | Each append blocks until the writer thread flushes to disk. Group-commit window of 2ms amortizes cost across concurrent writers. | Maximum durability; slight throughput cost. |
| **Buffered** (`auto_flush = false`) | Records are queued; the writer flushes every 16 records. | Higher throughput; up to 15 records may be lost on a hard crash. |

### 4.3 File Layout

Each chain's files share a stem derived from the chain key and a hash of its genesis
thought, preventing accidental cross-chain file sharing:

```
~/.cloudllm/mentisdb/
  mentisdb-registry.json          # chain registry
  mentisdb-skills.bin             # skill registry
  mentisdb-webhooks.json          # webhook registrations
  <chain-key>-<hash8>.tcbin       # the thought chain (hash-chained ledger)
  <chain-key>-<hash8>.agents.json # agent registry for this chain
  <chain-key>-<hash8>.entity-types.json
  <chain-key>-<hash8>.vectors.<model>-<dim>-<ver>.bin  # vector sidecar
  <chain-key>-<hash8>.vectors.managed.json             # managed sidecar config
  <chain-key>-<hash8>.auto_edges.bin                   # implicit edge overlay
  tls/{cert.pem, key.pem}                              # self-signed TLS cert
```

The `.auto_edges.bin` file is a bincode-serialized implicit edge overlay (Section 6.7).
It's rebuildable from the vector sidecar and is not part of the hash chain.

---

## 5. Schema Evolution

### 5.1 Version Lattice

MentisDB has a linear schema version history:

| Version | What was added |
|---------|---------------|
| V0 | Original format; no version field |
| V1 | Explicit version field, optional Ed25519 signatures, agent registry sidecar |
| V2 | New ThoughtType variant (`Reframe`), new relation kind (`Supersedes`), cross-chain relations |
| V3 | Edge validity fields (`valid_at`, `invalid_at`) |

### 5.2 Idempotent Migrations

Each migration is an idempotent transformation — running it twice produces the same
result as running it once. Migrations compose: a V0 chain is upgraded by applying
V0→V1, then V1→V2, then V2→V3:

```rust
// Pseudocode: migration pipeline
fn migrate_chain(chain: Vec<Thought>) -> Vec<Thought> {
    let chain = migrate_v0_to_v1(chain);
    let chain = migrate_v1_to_v2(chain);
    let chain = migrate_v2_to_v3(chain);
    chain // now at V3 (current)
}
```

A critical constraint: because bincode encodes enum variants by integer tag, new variants
must be appended to the end of an enum. Reordering or inserting mid-enum would silently
corrupt persisted data.

After migration, the hash chain is rebuilt under the current schema and persisted in
binary format, so subsequent opens pay no migration cost.

---

## 6. Retrieval Pipeline

Retrieval is the heart of MentisDB. There are two paths: a deterministic filter (baseline
search) and a scored ranked pipeline.

### 6.1 Baseline Filter Search

The baseline path narrows candidates by indexed fields (type, role, agent_id, tags,
concepts) and applies a case-insensitive substring predicate over content and metadata.
Results return in append order. No BM25, no vectors, no graph — just fast filtering:

```rust
// Simplified: baseline filter
fn baseline_search(chain: &[Thought], query: &ThoughtQuery) -> Vec<&Thought> {
    chain
        .iter()
        .filter(|t| matches_type(t, query))
        .filter(|t| matches_agent(t, query))
        .filter(|t| matches_tags(t, query))
        .filter(|t| text_contains(t, query.text.as_deref()))
        .collect() // append order preserved
}
```

### 6.2 Ranked Search — Backend Selection

Ranked search selects a backend based on what signals are available:

| Query has... | Backend |
|---|---|
| Text, no vectors | Lexical (BM25 only) |
| Text + vectors | Hybrid (BM25 + vector fusion) |
| Text + graph, no vectors | LexicalGraph |
| Text + graph + vectors | HybridGraph (full pipeline) |
| No text | Heuristic (importance/recency scoring) |

### 6.3 BM25 with Per-Field DF Gating

The lexical score uses BM25 — the standard ranking function from information retrieval —
applied across five fields: content, tags, concepts, agent_id, and agent_registry. Each
field has its own weight and document-frequency gate.

```rust
// Simplified: BM25 score for a single term in a single field
fn bm25_field_score(
    term_freq: f64,      // how many times the term appears in this field
    doc_field_len: f64,  // length of this field in the document
    avg_field_len: f64,  // average field length across the corpus
    doc_freq: usize,     // how many documents contain this term in this field
    corpus_size: usize,  // total number of documents
) -> f64 {
    let k1 = 1.2;
    let b = 0.75;

    // Inverse Document Frequency: rare terms score higher
    let idf = ((corpus_size as f64 - doc_freq as f64 + 0.5)
        / (doc_freq as f64 + 0.5)
        + 1.0).ln();

    // BM25 term frequency saturation
    let tf_norm = (term_freq * (k1 + 1.0))
        / (term_freq + k1 * (1.0 - b + b * (doc_field_len / avg_field_len)));

    idf * tf_norm
}
```

**Per-field DF gating** prevents common terms from dominating. If a term appears in more
than 30% of documents (in the content field), it's filtered out of that field's scoring —
it's too common to be a useful signal. Each field has its own cutoff:

| Field | DF cutoff | Weight |
|-------|-----------|--------|
| content | 30% | 1.0 |
| tags | 30% | 1.6 |
| concepts | 30% | 1.4 |
| agent_id | 70% | 1.5 |
| agent_registry | 60% | 1.1 |

A term that's too common in one field can still contribute through another field whose
cutoff it respects.

Text normalization applies Porter stemming (e.g., "running" → "run") plus an irregular
verb lemma table (~170 entries: "went" → "go", "saw" → "see") since stemming can't
handle suppletive forms.

### 6.4 Smooth Vector-Lexical Fusion

When a vector sidecar is available, each thought has an embedding vector. The query text
is also embedded, and cosine similarity provides a semantic score. The fusion function
combines the lexical and vector scores:

```rust
// Simplified: smooth exponential fusion
fn fuse_scores(lexical_score: f64, vector_similarity: f64) -> f64 {
    let alpha = 35.0;
    let beta = 3.0;

    // When lexical score is 0 (no text overlap), the vector signal gets
    // amplified ~36x. As lexical score increases, the amplification decays
    // exponentially, so strong lexical matches aren't drowned out.
    vector_similarity * (1.0 + alpha * (-lexical_score / beta).exp())
}
```

This means: a thought that shares no words with the query but is semantically similar
gets a strong boost. A thought that matches both lexically and semantically gets both
signals additively. The exponential decay eliminates the discontinuities that step-function
boost tiers introduce at bin boundaries.

### 6.5 Graph-Aware Expansion

Thoughts linked by typed relations form a directed graph. When the ranked search has
seeds (top lexical/vector hits), it expands outward via BFS to find related thoughts:

```rust
// Simplified: bounded BFS graph expansion
fn expand_graph(
    seeds: &[&Thought],
    adjacency: &AdjacencyIndex,
    implicit_edges: &ImplicitEdgeOverlay,
    max_depth: usize,
    max_visits: usize,
    mode: TraversalMode, // Out, In, or Bidirectional
) -> Vec<GraphHit> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();

    // Seed the BFS with the lexical/vector hits
    for seed in seeds {
        queue.push_back((seed, 0)); // (thought, depth)
    }

    while let Some((thought, depth)) = queue.pop_front() {
        if depth > max_depth || results.len() >= max_visits {
            break;
        }
        if !visited.insert(thought.thought_id) {
            continue; // already seen
        }

        // Score by graph proximity: 1/depth
        results.push(GraphHit {
            thought_id: thought.thought_id,
            depth,
            graph_score: 1.0 / (depth as f64),
        });

        // Traverse explicit edges (typed relations)
        for neighbor in adjacency.neighbors(thought, mode) {
            queue.push_back((neighbor, depth + 1));
        }

        // Traverse implicit edges (cosine-inferred RelatedTo)
        for neighbor in implicit_edges.neighbors(thought) {
            queue.push_back((neighbor, depth + 1));
        }
    }

    results
}
```

Each typed relation carries a weight — a `ContinuesFrom` edge (weight 0.60) is stronger
than a `References` edge (weight 0.06):

| Relation kind | Weight |
|---|---|
| ContinuesFrom | 0.60 |
| BranchesFrom | 0.55 |
| Corrects, Invalidates | 0.50 |
| Supersedes | 0.45 |
| DerivedFrom | 0.40 |
| Summarizes | 0.20 |
| CausedBy | 0.20 |
| Supports, Contradicts | 0.15 |
| RelatedTo (implicit) | 0.08 |
| References | 0.06 |

Graph proximity decays with depth: a hit at depth 1 gets a score of 1.0, depth 2 gets 0.5,
depth 3 gets 0.33, etc.

### 6.6 Session Cohesion

When a seed thought has a moderate lexical score (not strong enough to stand alone, but
not noise), thoughts adjacent to it in append order get a proximity boost. This surfaces
"evidence turns" — thoughts that happened right after a match and share context even if
they don't share vocabulary:

```rust
// Simplified: session cohesion boost
fn cohesion_score(
    candidate_index: u64,
    seed_index: u64,
    seed_lexical_score: f64,
) -> f64 {
    let radius = 8;       // max append-order distance
    let boost = 0.8;      // max cohesion boost
    let seed_threshold_min = 3.0;
    let seed_threshold_max = 5.0;

    // Only apply when the seed is moderately strong (not solo-strong, not noise)
    if seed_lexical_score < seed_threshold_min
        || seed_lexical_score >= seed_threshold_max
    {
        return 0.0;
    }

    let distance = (candidate_index as f64 - seed_index as f64).abs();
    if distance > radius as f64 {
        return 0.0;
    }

    boost * (1.0 - distance / radius as f64)
}
```

### 6.7 Implicit Edge Overlay

In practice, most agents append thoughts without authoring explicit relation edges. For
such chains, the graph is sparse and graph expansion contributes nothing. The implicit
edge overlay closes this gap by deriving `RelatedTo` edges automatically from vector
cosine similarity.

```rust
// Simplified: building the implicit edge overlay
fn build_implicit_edges(
    vectors: &HashMap<Uuid, Vec<f32>>, // thought_id → embedding
    threshold: f64,                     // cosine similarity cutoff (default 0.85)
    max_neighbors: usize,               // K: max edges per node (default 5)
) -> HashMap<Uuid, Vec<ImplicitNeighbor>> {
    let mut overlay = HashMap::new();
    let ids: Vec<_> = vectors.keys().collect();

    for &source_id in &ids {
        let source_vec = &vectors[source_id];
        let mut neighbors: Vec<(Uuid, f64)> = Vec::new();

        for &target_id in &ids {
            if source_id == target_id {
                continue;
            }
            let similarity = cosine_similarity(source_vec, &vectors[target_id]);
            if similarity >= threshold {
                neighbors.push((*target_id, similarity));
            }
        }

        // Keep only the top-K most similar neighbors
        neighbors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        neighbors.truncate(max_neighbors);

        overlay.insert(*source_id, neighbors.into_iter()
            .map(|(id, score)| ImplicitNeighbor { id, score })
            .collect());
    }

    overlay
}
```

The full build is O(N²) (all pairwise cosines), done once at chain open for N ≤ 50,000.
On each new append, an O(N) incremental update computes cosines between the new thought's
vector and all existing entries, adds forward and back edges, and re-sorts/truncates
affected neighbor lists.

The overlay is persisted to `<chain>.auto_edges.bin` via bincode with atomic rename. If
the threshold or K parameters change, the stored file is discarded and a full rebuild is
triggered.

### 6.8 HNSW Approximate Vector Backend

For vector sidecars above a configurable size threshold (default 50,000 vectors),
MentisDB switches from exact linear-scan cosine to an HNSW (Hierarchical Navigable Small
World) graph for approximate nearest-neighbor search:

```rust
// Simplified: backend selection
fn select_backend(vector_count: usize, threshold: usize) -> VectorBackendKind {
    if vector_count >= threshold {
        VectorBackendKind::Hnsw
    } else {
        VectorBackendKind::Exact
    }
}
```

Both backends implement the same `VectorSearchBackend` trait, so the surrounding
retrieval pipeline is unchanged. The HNSW graph is persisted to disk and reloaded on
startup when the stored thought count matches the live chain. For large sidecars, the
graph can be built in a background thread while the daemon continues serving queries
with the exact backend as a placeholder.

At 10k vectors, HNSW delivers a 10.9× query speedup with 91.4% recall. At 50k vectors,
it achieves 100% recall at 4.23ms per query.

### 6.9 Reciprocal Rank Fusion

When reranking is enabled, the top K candidates (default 50) are re-ranked using
Reciprocal Rank Fusion — a simple, parameter-light method that combines multiple ranked
lists:

```rust
// Simplified: Reciprocal Rank Fusion
fn rrf_score(
    candidate: &Thought,
    ranked_lists: &[Vec<Uuid>], // multiple ranked lists of thought IDs
    k: u64,                     // damping constant (default 60)
) -> f64 {
    let mut total = 0.0;

    for list in ranked_lists {
        // Find the 1-indexed position of this candidate in the list
        if let Some(pos) = list.iter().position(|id| *id == candidate.thought_id) {
            let rank = (pos + 1) as u64; // 1-indexed
            total += 1.0 / (k + rank) as f64;
        }
        // If absent from a list, contributes 0 (no penalty beyond missing the signal)
    }

    total
}
```

MentisDB produces three single-signal rankings — lexical-only, vector-only, and
graph-only — and fuses them via RRF. The RRF score replaces the additive blend. Other
signals (importance, cohesion, confidence, recency) are then added as small
tie-breaking adjustments.

RRF is pure arithmetic: no LLM, no external service, no network round trip.

### 6.10 Importance Weighting

User-originated thoughts (importance ~0.8) should outrank verbose assistant responses
(importance ~0.2) in close BM25 races. The importance boost is differential, not a flat
multiplier:

```rust
// Simplified: importance boost
fn importance_boost(base_score: f64, importance: f64) -> f64 {
    // importance is 0.0–1.0; 0.5 is neutral
    base_score * (importance - 0.5) * 0.3
}
```

The differential structure prevents flat multipliers from overwhelming the lexical signal.

### 6.11 Decomposed Scores

Each ranked hit exposes its full score breakdown for auditability:

```rust
struct RankedSearchHit {
    thought_id: Uuid,
    // Individual signal scores
    lexical_score: f64,
    vector_score: f64,
    graph_score: f64,
    relation_boost: f64,
    seed_score: f64,
    importance_boost: f64,
    confidence: f64,
    recency_score: f64,
    cohesion_score: f64,
    // The final fused total
    total_score: f64,
    // Which query terms matched and in which fields
    matched_terms: Vec<String>,
    match_sources: Vec<String>,
}
```

This lets you debug *why* a thought ranked where it did — essential for tuning.

### 6.12 Context Bundles

Context bundles group retrieval results by their seed thought, pairing each lexical seed
with its graph-expanded neighbors. This gives agents context in deterministic provenance
order, so they can interpret *why* supporting thoughts surfaced:

```rust
// Simplified: context bundle structure
struct ContextBundle {
    seed: RankedSearchHit,              // the lexical seed
    neighbors: Vec<RankedSearchHit>,    // graph-expanded related thoughts
}
```

### 6.13 Vector Sidecars

Vector state lives in rebuildable per-chain sidecars, partitioned by chain key, model ID,
dimension, and version. Model or version changes invalidate old sidecars rather than
silently mixing incompatible embeddings. Managed sidecars stay synchronized on append;
the daemon defaults to local ONNX inference via `fastembed-minilm`.

At chain open, the sidecar is deserialized once into an in-memory index. All subsequent
ranked-search calls read from memory without touching disk.

### 6.14 Memory Scopes

Thoughts can be scoped to `User`, `Session`, or `Agent` level. A query with a scope
filter returns only thoughts at that scope; absence of a scope filter returns all. Scopes
are stored as tag markers (`scope:user`, `scope:session`, `scope:agent`).

---

## 7. Deduplication

### 7.1 Jaccard-Supersedes Algorithm

When a new thought is appended, MentisDB checks the last N thoughts (default 64) for
near-duplicates using Jaccard similarity over normalized token sets:

```rust
// Simplified: dedup check on append
fn check_dedup(
    new_thought: &Thought,
    recent_thoughts: &[&Thought], // last N thoughts
    threshold: f64,                // default 0.85
) -> Option<Uuid> {
    let new_tokens = normalize_tokens(&new_thought.content); // stemmed + lemma-expanded
    if new_tokens.is_empty() {
        return None;
    }

    let mut best_match = None;
    let mut best_similarity = 0.0;

    for prior in recent_thoughts {
        let prior_tokens = normalize_tokens(&prior.content);
        let similarity = jaccard(&new_tokens, &prior_tokens);

        if similarity > best_similarity {
            best_similarity = similarity;
            best_match = Some(prior.thought_id);
        }
    }

    // If the best match exceeds the threshold, mark it as superseded
    if best_similarity >= threshold {
        best_match
    } else {
        None
    }
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 { 0.0 } else { intersection / union }
}
```

If a duplicate is found, a `Supersedes` relation edge is auto-emitted, and the
superseded thought's ID is added to the invalidation set for O(1) skipping during
retrieval. The original thought is retained for audit — no content is deleted.

### 7.2 Configuration

```bash
MENTISDB_DEDUP_THRESHOLD=0.85    # Jaccard threshold (0.0–1.0)
MENTISDB_DEDUP_SCAN_WINDOW=64    # how many recent thoughts to scan
```

---

## 8. Operational Surfaces

### 8.1 CLI

The `mentisdb` binary exposes subcommands that RPC over REST to a running daemon:

```bash
mentisdb add --content "Decision: use Rust for the API" --type Decision
mentisdb search "what database did we choose"
mentisdb agents --list
```

Client commands use synchronous HTTP (`ureq`) to avoid pulling in an async runtime.

### 8.2 MCP Server

MentisDB exposes a streamable HTTP MCP endpoint with 35+ tools covering bootstrap, append,
search, read, export/import, agent registry, chain management, and a skill registry:

```
POST /  (port 9471)
Content-Type: application/json

# Tools include:
#   mentisdb_bootstrap, mentisdb_append, mentisdb_search,
#   mentisdb_ranked_search, mentisdb_context_bundles,
#   mentisdb_traverse_thoughts, mentisdb_branch_from,
#   mentisdb_merge_chains, mentisdb_upload_skill, ...
```

### 8.3 Skill Registry

The skill registry is a git-like immutable version store for agent instruction bundles.
An upload to an existing skill ID creates a new immutable version: the first is stored as
full content, subsequent versions as unified diff patches. Version reconstruction replays
patches from v0 forward. Skills may be deprecated or revoked without losing history.
Permanent delete (`delete_skill`, REST `POST /v1/skills/delete`, MCP
`mentisdb_delete_skill`) is the explicit exception: it removes the skill and all
versions so the same identifier can be reused.

Agents with registered Ed25519 keys must cryptographically sign uploads; signature
verification is server-side before acceptance.

### 8.4 Bootstrap Protocol

Modern MCP clients bootstrap from a handshake:

1. `initialize.instructions` directs the agent to read `mentisdb://skill/core`
2. `resources/read` returns the embedded operating skill
3. `mentisdb_bootstrap` opens or creates the chain; if empty, writes a genesis checkpoint
4. `mentisdb_recent_context` loads prior state for resumption

### 8.5 REST API

A versioned REST API (`/v1/...`) mirrors the MCP tools for non-MCP clients, with bearer
token authorization supporting chain-scoped tokens.

---

## 9. Empirical Evaluation

### 9.1 Benchmarks

MentisDB is evaluated on two standard long-term memory benchmarks:

| Benchmark | Metric | v0.8.1 | v0.8.5 | v0.8.9 |
|---|---|---|---|---|
| LoCoMo-2P | R@10 | **88.7%** | — | — |
| LoCoMo-2P single-hop | R@10 | 90.7% | — | — |
| LoCoMo-10P (1977 queries) | R@10 | 74.2% | **74.6%** | **71.9%** |
| LoCoMo-10P single-hop | R@10 | — | 79.0% | 75.8% |
| LoCoMo-10P multi-hop | R@10 | — | 58.4% | 57.4% |
| LoCoMo-10P | R@20 | — | — | 79.1% |
| LongMemEval (fresh chain) | R@5 | 67.6% | — | **66.8%** |
| LongMemEval (fresh chain) | R@10 | 73.2% | — | **72.2%** |
| LongMemEval (fresh chain) | R@20 | — | — | 78.0% |

All v0.8.9 numbers were reproduced deterministically across independent full-scale
benchmark runs on 2026-04-14 and 2026-04-17 against fresh chains with the default
retrieval configuration (`fastembed-minilm` vector sidecar, graph expansion enabled, RRF
reranking disabled).

### 9.2 Scoring Evolution

| Version | Change | LongMemEval R@5 | LoCoMo-10P R@10 |
|---|---|---|---|
| 0.8.0 baseline | — | 57.2% | — |
| 0.8.0 + Porter stemming | token normalization | 61.6% | — |
| 0.8.0 + tiered fusion + importance | vector/lexical balance | 65.0% | — |
| 0.8.1 + cohesion + smooth fusion + DF cutoff | retrieval quality | 67.6% | 74.2% |
| 0.8.5 + cohesion tuning + doubled edge weights + fastembed | session/graph boost | — | 74.6% |
| 0.8.9 + irregular lemmas + webhooks | lemma expansion + events | 66.8% | 71.9% |
| 0.9.8 + implicit edge overlay + in-memory vector index | graph density + latency | 66.8% | — |
| 0.9.9 + thesaurus auto-by-default + full embeddings | automatic synonym expansion | 66.8% | **72.6%** |

The 0.9.9 release makes the static thesaurus apply automatically by default to all ranked
search paths. With real `fastembed-minilm` vectors this delivers the target LoCoMo-10P
R@10 = 72.6%. LongMemEval remains at 66.8% R@5 (lexical gaps still dominate many misses).

### 9.3 Near-Miss Analysis (LoCoMo-10P, v0.8.5)

Of 503 misses (gold answer absent from top-10):

| Bucket | Count | Fraction | Interpretation |
|---|---|---|---|
| R@20 hit | 130 | 25.8% | close ranking error |
| R@50 hit | 285 | 56.7% | moderate signal gap |
| R > 50 | 218 | 43.3% | lexical gap (query terms absent from evidence) |

The 43.3% figure represents a hard ceiling for BM25-only retrieval on this benchmark.
Closing it requires larger embedding models, LLM-driven query expansion, or external
knowledge retrieval. The 25.8% ranking-error bucket is the target for graph-density
improvements; the implicit edge overlay is designed to address it.

### 9.4 Near-Miss Analysis (LongMemEval, v0.9.8)

Running v0.9.8 against the LongMemEval 500-question evaluation set (10,866-thought chain,
`fastembed-minilm`, default settings, threshold 0.85) produces R@5 = 66.8%, R@10 ≈ 72.0%,
R@20 ≈ 77.8%. Of 166 misses:

| Bucket | Count | Fraction | Interpretation |
|---|---|---|---|
| R@10 hit | 27 | 16.3% | close ranking error |
| R@20 hit | 57 | 34.3% | moderate signal gap |
| not in R@20 | 109 | 65.7% | lexical gap |

The dominant miss signal is lexical: 65.7% of misses are not recoverable at R@20
regardless of graph topology, because the evidence thoughts share no vocabulary with the
query. Graph expansion is the correct tool for LoCoMo multi-hop; query expansion or a
larger encoder is the correct tool for LongMemEval's lexical gap.

### 9.5 HNSW Performance

| Corpus | Backend | Query Latency (100 queries) | Recall@10 | Speedup |
|---|---|---|---|---|
| 10k × 128d | Exact (linear scan) | 95.1 ms | 100.0% | — |
| 10k × 128d | **HNSW** | **8.75 ms** | 91.4% | **10.9×** |
| 50k × 128d | **HNSW** | **4.23 ms/query** | **100.0%** | — |

The 50k result: 100% recall at 4.23ms per query. Build time was 232s (one-time,
background). HNSW graph persistence means the graph is serialized to disk and reloaded
on daemon startup — no rebuild needed after restart.

### 9.6 Micro-Benchmarks

Criterion micro-benchmarks span five domains: append throughput (`thought_chain`),
baseline search (`search_baseline`), ranked retrieval (`search_ranked`), skill registry
lifecycle (`skill_registry`), and HTTP concurrency at {100, 1k, 10k} concurrent Tokio
tasks with p50/p95/p99 reporting (`http_concurrency`). A `DashMap`-based concurrent chain
lookup delivers 750–930 read req/s at 10k concurrent tasks.

---

## 10. Related Work and Positioning

| Feature | **MentisDB** | Mem0 | Graphiti / Zep | Letta / MemGPT |
|---|---|---|---|---|
| Implementation language | Rust | Python | Python | Python / TS |
| Storage | embedded file | external DB | Neo4j / FalkorDB | external DB |
| LLM required for core | **No** | Yes | Yes | Yes |
| Cryptographic integrity | **SHA-256 hash chain** | — | — | — |
| Hybrid retrieval | BM25 + vector + graph | vector + keyword | semantic + keyword + graph | — |
| Approximate NN (HNSW) | **Yes (default)** | — | — | — |
| Implicit graph inference | **cosine-inferred edges** | — | LLM-extracted | — |
| Temporal facts | valid_at / invalid_at | update-only | valid_at / invalid_at | — |
| Deduplication | **Jaccard + Supersedes** | LLM-based | merge | — |
| Agent registry | Yes | — | — | Yes |
| MCP server | **Built-in** | — | Yes | — |

MentisDB is, to our knowledge, the only system combining (i) embedded storage, (ii) zero
LLM dependency in the core path, (iii) cryptographic chain integrity, (iv) hybrid BM25 +
vector + graph retrieval with cosine-inferred implicit edges, and (v) a built-in MCP
server — all in a single static binary.

---

## 11. Limitations and Future Work

### 11.1 Limitations

- **Ceiling of sparse retrieval.** The near-miss analyses quantify irreducible lexical
  gaps: 43.3% on LoCoMo-10P and 65.7% on LongMemEval. Dense embeddings and implicit graph
  edges mitigate ranking errors but cannot recover evidence thoughts that share no
  vocabulary with the query.

- **Implicit edge quality depends on the encoder.** With `fastembed-minilm` (384-dim) and
  threshold 0.85, LongMemEval produces a sparse overlay (~5k edges for 10k thoughts)
  because MiniLM cosine similarities between topically diverse conversations rarely exceed
  0.85. Lowering the threshold increases density but introduces noise.

- **Local-only integrity.** The hash chain provides tamper evidence, not Byzantine fault
  tolerance or distributed consensus. Cross-chain consistency is not enforced
  cryptographically.

- **Schema churn discipline.** Because bincode tags enum variants by ordinal, schema
  evolution is append-only at the enum level — reordering or renaming variants would
  silently corrupt persisted data.

### 11.2 Future Work

- **LoCoMo multi-hop validation of implicit edge overlay.** The 21-point single-hop /
  multi-hop gap (79.0% vs 57.4% R@10) is the primary target for graph-density
  improvements.
- **Threshold sweep** across cosine thresholds {0.75, 0.80, 0.85, 0.90} and K {3, 5, 10}
  on LoCoMo multi-hop.
- **Per-chain entity/relation ontologies** enabling typed domain-specific facts beyond the
  fixed relation kinds.
- **Cross-chain federated retrieval** with result reconciliation across distributed
  ledgers.
- **Optional LLM-extracted memories** as a layered, auditable transform.
- **Self-improving skill registry**: agents committing updated skill versions as they
  learn, with signed provenance.

### 11.3 Conclusion

MentisDB formalizes agent memory as an append-only, hash-chained ledger of semantically
typed thoughts, and couples that ledger with a composable retrieval pipeline — BM25 with
per-field DF gating, smooth vector-lexical fusion, bounded typed-edge graph expansion
augmented by cosine-inferred implicit edges, RRF, session cohesion, and Jaccard-based
deduplication — in a single embedded Rust crate. The implicit edge overlay extends
graph-based retrieval to chains where agents author few explicit relations, deriving a
dense implicit graph from vector cosine proximity at O(N) incremental cost per append.
Empirical results on LoCoMo and LongMemEval demonstrate competitive retrieval quality
without reliance on external databases or LLM services for the core ingestion path. The
system is released as open source and exposes MCP, REST, and HTTPS surfaces for
interoperation with contemporary agentic harnesses.

---

## References

- Robertson, S. and Zaragoza, H. *The Probabilistic Relevance Framework: BM25 and Beyond*. Foundations and Trends in Information Retrieval, 3(4), 2009.
- Cormack, G. V., Clarke, C. L. A., and Büttcher, S. *Reciprocal Rank Fusion outperforms Condorcet and individual Rank Learning Methods*. SIGIR, 2009.
- Porter, M. F. *An algorithm for suffix stripping*. Program, 14(3), 1980.
- Jaccard, P. *Étude comparative de la distribution florale dans une portion des Alpes et des Jura*. Bulletin de la Société Vaudoise des Sciences Naturelles, 1901.
- Bernstein, D. J., Duif, N., Lange, T., Schwabe, P., and Yang, B.-Y. *High-speed high-security signatures*. Journal of Cryptographic Engineering, 2012.
- FIPS PUB 180-4. *Secure Hash Standard (SHS)*. NIST, 2015.
- Maharana, A. et al. *Evaluating Very Long-Term Conversational Memory of LLM Agents (LoCoMo)*. 2024.
- Wu, D. et al. *LongMemEval: Benchmarking Chat Assistants on Long-Term Interactive Memory*. 2024.
- Anthropic. *Model Context Protocol (MCP) Specification*. 2024.

---

**Angel Leon**
