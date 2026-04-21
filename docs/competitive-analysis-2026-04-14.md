# MentisDB Competitive Update — April 14, 2026

*Follow-up to the April 10, 2026 competitive analysis*

## Executive Summary

Since April 10, MentisDB has shipped 11 releases (0.8.2 → 0.9.1), closing most of the feature gaps identified in the original analysis. The competitive landscape has also shifted: Hindsight (9.2k stars) has emerged as a strong new entrant, Cognee crossed 15k stars, and MCP-first memory systems have proliferated. This update re-benchmarks all systems and identifies what MentisDB has gained and what remains.

---

## New Entrants: Systems Researched

### 1. Hindsight (`vectorize-io/hindsight`)

**9.2k GitHub stars · Python (70%) + TypeScript (17%) + Rust (3.5%)**

Hindsight is an agent memory system built by Vectorize around a biomimetic memory model. It claims state-of-the-art on LongMemEval benchmarks, with independently verified scores from Virginia Tech and The Washington Post.

**Architecture:**
- Server-mode (Docker): external PostgreSQL + separate services
- Embedded mode (`hindsight-all`): Python-only, no external DB, LLM still required
- Memory banks (equivalent to MentisDB chains)
- Three memory pathways: World (facts), Experiences (agent's own), Mental Models (reflected understanding)

**Memory Model:**
- LLM-powered `retain()` to extract entities, relationships, time series from raw input
- `recall()` merges 4 retrieval strategies: semantic (vector), keyword (BM25), graph (entity/temporal/causal), temporal
- `reflect()` generates new observations/insights from existing memories using LLM

**Retrieval:** Hybrid of 4 signals merged via reciprocal rank fusion + cross-encoder reranking

**Unique Features:**
- Mental Models: learned understanding formed by reflecting on raw memories (higher-order synthesis)
- Disposition-aware responses via `reflect()`
- Benchmarks independently verified by academic collaborators
- Per-user memory isolation via metadata filtering

**Weaknesses:**
- LLM required for all core operations (retain, reflect)
- External database required for production (PostgreSQL)
- No cryptographic integrity
- Not local-first (server mode requires Docker + external DB)
- No agent registry or multi-agent coordination primitives

---

### 2. Cognee (`topoteretes/cognee`)

**15.3k GitHub stars · Python (86%) · Apache 2.0**

Cognee is a knowledge engine that combines vector search, graph databases, and cognitive science approaches. Already covered in the April analysis, but worth noting it crossed 15k stars and released v1.0.0 on April 11, 2026.

**Notable v1.0 additions:**
- `cognee-mcp` package for MCP server integration
- Cognee Cloud managed service
- Hermes Agent native integration as memory provider
- Expanded deployment targets (Modal, Railway, Fly.io, Render, Daytona)

**Key characteristics (unchanged):**
- LLM required for cognify pipeline
- External DBs (Neo4j, etc.) required
- `remember/recall/forget/improve` API
- Session memory + permanent graph layers
- No cryptographic integrity

---

### 3. LangMem (`langchain-ai/langmem`)

**1.4k GitHub stars · Python · MIT**

LangMem is LangChain's memory primitives library, integrated natively into LangGraph Platform. It provides functional memory primitives and memory management tools for agents.

**Architecture:**
- Storage-backend agnostic (works with any LangGraph `BaseStore`)
- Default: `InMemoryStore` for dev, `AsyncPostgresStore` for production
- Requires vector embedding provider for search

**Memory Model:**
- `create_manage_memory_tool()` — agents decide what to store
- `create_search_memory_tool()` — agents search their own memory
- Background memory manager for automatic consolidation
- Hot-path and background modes

**Unique Features:**
- Native LangGraph integration (default in LangGraph Platform deployments)
- Pluggable storage (in-memory → Postgres → any backend)
- Agents own their memory management decisions

**Weaknesses:**
- LLM required for core operations
- No cryptographic integrity
- External database for production
- Not local-first
- No native graph traversal or temporal facts
- No agent registry

---

### 4. Anthropic Memory / Model Context Protocol

Anthropic's approach to agent memory is primarily delivered through the **Model Context Protocol (MCP)** — an open standard for connecting AI applications to external data and tools. Rather than a monolithic memory product, Anthropic provides:

**MCP Servers:**
- File system, Git, databases, knowledge bases
- Any tool or data source can be an MCP server
- Standardized interface: resources, tools, prompts

**Memory Integration Pattern:**
Anthropic agents access memory through MCP servers. Examples:
- Third-party memory servers (mentisdb-mcp, others)
- Custom MCP servers for specific memory needs
- Claude Desktop has native MCP support

**Key Properties:**
- No native memory storage — delegated to MCP servers
- Standardized protocol enables any memory system to connect
- Agents use `read` tool to access MCP resources

**Weaknesses:**
- Memory capability entirely dependent on MCP server implementation
- No built-in memory system — must be provided by external service
- No standardized memory schema or retrieval strategies in MCP itself

---

### 5. Obsidian as Agent Memory

Obsidian is a general-purpose knowledge management tool that has been adopted by AI agent users as a memory layer. This is not a product feature but a community pattern.

**How it works:**
- Agents write notes to Obsidian vault (markdown files)
- Obsidian's graph view shows connections between notes
- Plugins like Dataview enable querying
- Local file storage, no cloud required
- Community plugins for AI integration (e.g., Copilot, AI commands)

**For AI Agents:**
- Claude Code, Cursor, and others can read/write Obsidian vaults
- Cross-agent sharing via shared vault
- Rich linking between memories
- MCP servers exist for Obsidian access

**Weaknesses:**
- No native AI memory semantics — just files
- No hybrid retrieval (BM25, vector, graph)
- No temporal fact management
- No cryptographic integrity
- No agent registry
- Requires external tool integration to be useful as memory
- Manual organization required

---

### 6. Fast.io (`fast.io`)

**Cloud storage platform with MCP support**

Fast.io is a cloud content workspace with MCP server integration for AI agents. 50GB free storage, 5,000 monthly AI credits.

**For AI Agents:**
- MCP server at `mcp.fast.io/mcp` with 14+ tools
- File storage and retrieval
- Built-in AI summarization and RAG
- Agent-human handoff patterns
- Semantic search over documents

**Architecture:**
- Cloud-only (no self-hosted option)
- External database (their infrastructure)
- Requires API key/account

**Weaknesses:**
- Not a memory system per se — file storage with AI features
- No hybrid retrieval over agent memories
- No cryptographic integrity
- No agent registry
- Cloud-dependent (not local-first)
- Not designed for agent memory use cases

---

### 7. Hermes Memory Ecosystem

"Hermes" refers to multiple related projects:

#### Hermes Agent (`NousResearch/hermes-agent`)
- Open-source agent framework with MCP support
- Modular memory architecture (plug in any memory provider)
- Native Cognee integration available
- Python-based

#### Hermes Workspace (`outsourc-e/hermes-workspace`)
- Web UI for Hermes Agent
- Memory browser, skills explorer, terminal
- 1.4k stars · TypeScript · MIT

#### Memory Plugins for Hermes:
- **agentmemory** (1.6k stars) — TypeScript, SQLite-based, hooks + MCP
- **ClawMem** (112 stars) — Bun/TypeScript, SQLite, hybrid RAG
- Both work as Hermes memory providers via MCP or native plugins

**Key observation:** Hermes has become a hub for third-party memory integrations. The memory battle is happening at the plugin/extension level.

---

## Updated Feature Comparison Table

| Feature | MentisDB | Hindsight | Cognee | LangMem | Mem0 | Graphiti/Zep | Letta | Neo4j KB | Fast.io | Obsidian | Hermes Plugins |
|---------|----------|-----------|--------|--------|------|--------------|-------|----------|---------|----------|---------------|
| **Language** | Rust | Python | Python | Python | Python | Python | Python/TS | Python | TypeScript | Markdown | TypeScript |
| **Storage** | Embedded (sled) | External (Postgres) | External (Neo4j) | External (Postgres) | External DB | External DB | External DB | Neo4j | Cloud | Local files | SQLite/External |
| **LLM Required** | **No** (opt-in) | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | No | No |
| **Local-First** | **Yes** | No | No | No | Partial | No | No | No | No | **Yes** | Partial |
| **Cryptographic Integrity** | **Yes** (hash chain) | No | No | No | No | No | No | No | No | No | No |
| **Hybrid Retrieval** | **BM25+vec+graph** | BM25+vec+graph+causal | vec+graph | vec | vec+keyword | semantic+kw+graph | No | multi-mode | RAG | No | BM25+vec |
| **MCP Support** | **Built-in** | No | MCP client | No | No | Yes | No | No | **Yes** | Via plugin | Via plugin |
| **Agent Registry** | **Yes** | No | No | No | No | No | Yes | No | No | No | No |
| **Federated/Cross-Chain** | **Yes** (0.9.1) | No | No | No | No | No | No | No | No | No | No |
| **Skills/Extensions** | **Yes** (skill registry) | No | No | No | No | No | No | No | No | Plugins | Via plugin |
| **Temporal Facts** | **Yes** (0.8.2) | Via metadata | No | No | Updates | **Yes** | No | No | No | No | No |
| **Memory Dedup** | **Yes** (0.8.2) | No | Partial | No | Yes | Merge | No | No | No | No | Partial |
| **Custom Ontology** | **Yes** (0.8.7) | Via metadata | Yes | No | No | Pydantic | No | Schema | No | No | No |
| **Episode Provenance** | **Yes** (0.8.8) | No | Partial | No | No | Yes | No | Partial | No | No | Partial |
| **Memory Branching** | **Yes** (0.8.6) | No | No | No | No | No | No | No | No | No | No |
| **RRF Reranking** | **Yes** (0.8.6) | Yes | No | No | No | No | No | No | No | No | Via reranker |
| **Webhooks** | **Yes** (0.9.1) | No | No | No | No | No | No | No | No | No | No |
| **LLM Extraction** | **Opt-in** (0.9.1) | Yes (core) | Yes (core) | Yes (core) | Yes | Yes | Yes | Yes | Yes | No | No |
| **Benchmarks** | 72% LoCoMo R@10 | SOTA (indep. verified) | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | 95% R@5 (agentmemory) |

---

## What MentisDB Gained Since April 10

### ✅ Closed Gaps

| Feature | April 10 Status | April 14 Status | Delta |
|---------|-----------------|------------------|-------|
| **Temporal Facts** | Planned for 0.8.2 | ✅ Shipped in 0.8.2 | Done |
| **Memory Dedup** | Planned for 0.8.2 | ✅ Shipped in 0.8.2 (Jaccard) | Done |
| **Multi-Level Scopes** | Planned for 0.8.2 | ✅ Shipped in 0.8.2 (tag-based) | Done |
| **Custom Ontology** | Planned for 0.8.4 | ✅ Shipped in 0.8.7 (entity_type + registry) | Done |
| **Episode Provenance** | Planned for 0.8.4 | ✅ Shipped in 0.8.8 (source_episode field) | Done |
| **CLI Tool** | No | ✅ Shipped in 0.8.2 (add/search/agents) | Done |
| **RRF Reranking** | No | ✅ Shipped in 0.8.6 | Done |
| **Memory Branching** | No | ✅ Shipped in 0.8.6 (BranchesFrom) | Done |
| **Per-Field BM25 DF Cutoffs** | No | ✅ Shipped in 0.8.6 | Done |
| **Irregular Verb Lemma Expansion** | No | ✅ Shipped in 0.9.1 (query time) | Done |
| **Federated Cross-Chain Search** | No | ✅ Shipped in 0.9.1 | Done |
| **Webhooks** | No | ✅ Shipped in 0.9.1 | Done |
| **Opt-in LLM Extraction** | No | ✅ Shipped in 0.9.1 | Done |
| **Python Client** | No | ✅ Shipped in 0.9.1 (pymentisdb) | Done |
| **Reciprocal Rank Fusion** | No | ✅ Shipped in 0.8.6 | Done |
| **LLM Reranking** | No | ✅ Shipped in 0.8.8 (opt-in) | Done |

### 📈 New Unique Advantages

1. **Federated Cross-Chain Search (0.9.1):** Walk BranchesFrom relations to discover parent chains; ranked search on branch chains transparently includes ancestor results. No competitor has this.

2. **Opt-in LLM Extraction:** Keep the no-LLM core for pure structural retrieval, add LLM-powered extraction only when needed. All competitors require LLM for core operations.

3. **Skill Registry:** First-class skill upload, versioning, deprecation, and revocation with signed thought support. Unique among all systems.

4. **Webhooks:** HTTP callbacks on thought append — register/list/get/delete webhooks. No competitor has this.

5. **Python Client (pymentisdb):** Full MentisDbClient with LangChain integration, complete enum coverage, typed relations. Enables Python ecosystem adoption.

6. **Rust + Embedded:** Still the only Rust-based, embedded-storage, no-LLM-required memory system with cryptographic integrity.

---

## What MentisDB Is Still Missing

### 1. Ecosystem Integrations (High Priority)

| Gap | Who Has It | MentisDB Status |
|-----|-----------|-----------------|
| LangChain native integration | LangMem | Python client only, not native LangChain store |
| LlamaIndex integration | Cognee | Python client only |
| Auto-capture hooks | Hindsight, agentmemory | Manual append only |
| Per-agent automatic memory capture | Hindsight | Manual append only |

**Fix:** Build native LangChain `BaseStore` implementation and LlamaIndex memory connector.

### 2. Benchmark Verification

Hindsight's benchmark claims are independently verified by Virginia Tech. MentisDB's benchmarks (72% LoCoMo R@10, 66.8% LongMemEval R@5) are self-reported.

**Fix:** Partner with an academic group to independently verify scores, as Hindsight did.

### 3. Managed Cloud Service

Mem0, Cognee, Hindsight, and Fast.io all offer hosted versions. This lowers the barrier to entry significantly.

**Fix:** Offer MentisDB Cloud for users who want managed service without infrastructure.

### 4. Memory Lifecycle Management

- **agentmemory:** 4-tier consolidation (working → episodic → semantic → procedural) with Ebbinghaus decay curve
- **Hindsight:** Mental Models reflection
- **LangMem:** Background memory manager

MentisDB has dedup and temporal validity but lacks automatic memory consolidation/evolution.

**Fix:** Implement automatic memory consolidation tiers.

### 5. Enterprise Features

- Audit trails beyond hash chain (token tracking, access logs)
- Role-based access control
- Compliance exports (SOC2, GDPR)

### 6. Broader Agent Support

Competitors have explicit integrations with Claude Code, Cursor, OpenClaw, Hermes, etc. MentisDB's agent support is primarily via MCP which is generic.

**Fix:** Build explicit plugins/integrations for major agent platforms.

---

## Competitive Position Shift

### April 10 Position:
> "MentisDB is the only local-first, zero-dependency, cryptographically-integrity-verified semantic memory with built-in hybrid retrieval — in Rust."

### April 14 Position:

**Still unique:** The combination of Rust + embedded + no-LLM-required + cryptographic integrity remains unique.

**New differentiation:**
- **Federated cross-chain search** — unique to MentisDB
- **Skill registry with revocation** — unique to MentisDB  
- **Webhooks** — unique to MentisDB
- **Opt-in LLM extraction** (keep no-LLM core) — unique positioning

**Competitive landscape has tightened:**
- Hindsight is a credible SOTA contender on benchmarks (independently verified)
- Cognee crossed 15k stars and has native Hermes integration
- agentmemory shows 95% R@5 on LongMemEval (self-reported)
- LangMem is the default in LangGraph Platform (massive distribution advantage)

---

## Recommendations

### Immediate (0.10.x)

1. **LangChain native store** — pip install langchain-mentisdb
2. **Academic benchmark verification** — contact Virginia Tech Sanghani Center
3. **Claude Code auto-capture plugin** — match agentmemory's 12 hooks
4. **Memory consolidation tiers** — implement 4-tier lifecycle

### Near-term (0.11.x)

1. **Managed cloud service** — MentisDB Cloud
2. **LlamaIndex connector** — complete Python ecosystem
3. **Enterprise audit logging** — token tracking, access logs
4. **Plugin ecosystem docs** — explicit integration guides for major agents

### Future (1.0+)

1. **Self-improving agent primitives** (on roadmap)
2. **Browser extension** (on roadmap)
3. **Cross-platform CLI** — one binary for all platforms
4. **Compliance exports** — SOC2, GDPR

---

## Conclusion

MentisDB has shipped an extraordinary amount since April 10 — 11 releases, 30+ features — closing most of the gaps identified in the original analysis. The hash chain, embedded storage, and no-LLM-required properties remain unique differentiators. The addition of federated cross-chain search, webhooks, opt-in LLM extraction, and the Python client expand the addressable market significantly.

The competitive threat is now Hindsight (benchmarks, managed service) and the LangMem/LangGraph ecosystem (distribution). The opportunity is Rust performance, cryptographic integrity, and the skill registry — properties that matter for enterprise and audit-critical applications.

MentisDB is well-positioned. The next battle is ecosystem and distribution, not features.
