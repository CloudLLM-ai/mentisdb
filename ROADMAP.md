# MentisDB Roadmap

## Shipped (0.8.2 -> 0.10.4.49)

### 0.10.4.49 - Dashboard Login Fix, Session Lifetime Fix, HNSW Unconditional
- **Dashboard login fix** — the session cookie's `Secure` attribute is now conditional on `X-Forwarded-Proto`, fixing login behind TLS-terminating reverse proxies that talk plain HTTP to the daemon.
- **Session lifetime fix** — replaced `RATE_LIMIT_WINDOW_SECS * 60` (5 hours, due to a unit mismatch) with an explicit `SESSION_TIMEOUT_SECS` (8 hours), independent of the brute-force rate-limit window.
- **HNSW unconditional** — the `hnsw-backend` Cargo feature has been removed; HNSW is always compiled in. The `HnswBackendNotEnabled` error variant and its test have been deleted.
- **`local-embeddings` is now a default feature** — `cargo install` and the self-updater no longer need `--features local-embeddings`. The Makefile install target, `build_cargo_install_args`, and the non-interactive update messages have been simplified.

### 0.10.3.48 - HNSW Approximate Vector Search, Background Builds, and HNSW Runtime Tuning
- **HNSW approximate-nearest-neighbor vector backend** — unconditionally compiled since 0.10.4.49. The exact f32 cosine scan remains the fallback for small sidecars; once a managed sidecar exceeds `MENTISDB_HNSW_THRESHOLD` (default 50,000 vectors) MentisDB switches to a pure-Rust HNSW graph automatically. The public API and hybrid/ranked search semantics are unchanged.
- **HNSW graph persistence** — built graphs are serialized to disk and reloaded on startup when the vector count matches; stale graphs are deleted during a from-scratch rebuild. Writes are atomic (temp file + rename) so readers never see partial state.
- **Background HNSW construction** — `MENTISDB_HNSW_BACKGROUND_BUILD` (default true) starts an Exact placeholder immediately and builds the graph off-thread, then swaps it into the cached sidecar when ready. Dashboard `backend_kind` and `backend_building` expose the current state.
- **HNSW runtime knobs** — `MENTISDB_HNSW_THRESHOLD`, `MENTISDB_HNSW_EF_CONSTRUCTION`, `MENTISDB_HNSW_EF_SEARCH`, and `MENTISDB_HNSW_BACKGROUND_BUILD` are surfaced in Dashboard Settings and read on demand.
- **Scale benchmark** — `benches/hnsw_scale.rs` measures build time, recall@10, and per-query latency at 50k/100k/1M vectors (100k/1M opt-in via env var).
- **Removed experimental quantized HNSW** — raw f32 HNSW outperformed the quantized prototype on both recall and latency, so the quantized path was deleted.

### 0.10.1.46 - Built-in Bearer Token Auth, TLS Cert CLI, and Search-First Discipline
- **Built-in bearer-token authentication** — `MENTISDB_BEARER_TOKEN_ACCESS=true` protects Streamable HTTP MCP and legacy HTTP MCP routes with `Authorization: Bearer ...`; tokens stored as SHA-256 hashes only. Prerequisite for any non-loopback HTTPS MCP/REST deployment.
- **Scoped bearer-token registry and CLI** — operators issue global tokens (`mentisdb bearertoken create --global <alias>`) or chain-scoped tokens (`--chain <chain>...`); chain-scoped tokens restricted to their chain set; server-wide tools require global token.
- **Dashboard Bearer Tokens page** — dedicated nav page with feature toggle, alias input, Global/Chains radio controls, multi-chain selector, one-time token display with copy-to-clipboard, token table, and revoke action.
- **Settings page enhancements** — edits `MENTISDB_BEARER_TOKEN_ACCESS` and adds Restart Daemon button for environment changes requiring restart.
- **`mentisdb cert` CLI** — mints self-signed TLS certificates with custom SAN sets, writes `MENTISDB_TLS_CERT`/`MENTISDB_TLS_KEY` to `.env`, supports `--force`, `--reset`, `--out-dir`, `--env-file`, `--no-env-update`; prints SAN list and SHA-256 fingerprint for `openssl` cross-check.
- **Comprehensive `--help`** — `mentisdb --help` prints daemon + all subcommand help in one view; subcommand-specific help works as expected.
- **Search-first discipline in MENTISDB_SKILL.md** — prominent "🔎 SEARCH BEFORE YOU WRITE" section with routine (`recent_context` → `ranked_search` → tighten with filters) and three decisions per append; "Blind appends" anti-pattern documented.
- **Library TLS cert API** — `ensure_tls_cert_with_sans(cert, key, extra_sans, overwrite) -> TlsCertArtifacts` exposed for library consumers and CLI; `ensure_tls_cert` is now a one-line wrapper.
- **`--headless` top-level flag** — `mentisdb --headless` starts HTTP/MCP/REST servers without TUI; listed in `--help`.
- **TUI auto-headless fix** — daemon auto-promotes to headless HTTP mode when stdin/stdout not a TTY, eliminating 100% CPU spin on `docker run` without `-t`, `nohup`, `systemd` without `StandardInput=tty`, `cron`, and SSH disconnect.
- **Restore idempotency** — local registries preserved during cross-instance restores; verified same-chain suffixes appended; divergent same-name chains imported under renamed keys; `--overwrite` only for same-path chain file replacement when safe suffix merge not possible.
- **Token prefix cleanup** — generated token secrets use `mentisdb_` prefix instead of `mdb_live_`.
- **Refactored cert CLI** — collapsed duplicated `writeln!` blocks into named helpers; help text single source of truth consumed by both `mentisdb cert --help` and `mentisdb --help`.
- **Docs: TLS Certificates section** — new README section with six worked examples, options table, restart note, `openssl s_client` cross-check; docs.mentisdb.com user_docs.rs and agent_docs.rs updated.
- **Full release engineering pipeline followed** — Phases 1–5 with benchmarks skipped per operator request, Phase 4 code review, granular docs commits.

### 0.9.9 - Automatic Thesaurus + Validated Retrieval Quality
- **Thesaurus now applies automatically by default** to all ranked search (REST, MCP, dashboard, CLI, benchmarks) — no client changes required
- Static thesaurus (~900 headwords + 300+ lemmas) + irregular verb lemmatization now fires transparently on every text query via `apply_thesaurus_if_text` in the server
- Real benchmark validation with full embeddings: LoCoMo-10P R@10 reached the target **72.6%**; LongMemEval R@5 at 66.8% (thesaurus delivered clear gains on vocabulary-mismatch and multi-hop cases)
- WHITEPAPER, README, skill file, blog post, and docs.mentisdb.com all updated for the new default behavior
- Full release engineering pipeline followed (Phases 1–5): benchmarks via sub-agents, Phase 4 code review, granular docs commits

### 0.9.8.44 - Binary Rename, Dashboard Settings, and Dotenvy
- **Binary rename** — `mentisdbd` → `mentisdb`; all docs, tests, CI, install scripts, and TUI references updated
- **Dashboard chain sizes** — chains table shows human-readable on-disk size per chain; summary bar shows total size, thoughts, agents, and per-index totals
- **Dashboard Settings tab** — edit all 18 `MENTISDB_*` env vars with type-aware controls, reset-to-default per row, apply with `.env` persistence and hot-reload for `auto_flush`; restart-required flag for network/storage changes
- **Dotenvy support** — loads environment variables from `.env` in the working directory at startup; shell env takes precedence; silently ignored when absent
- **TUI improvements** — Storage Location column shows chain file size in brackets; running status includes daemon PID

### 0.9.7.43 - Dashboard Skill Editing and Stdio Reliability
- **Dashboard skill editing** - operators can edit skills from the Skills table or a skill detail page; saving creates a new immutable version through the existing upload path and preserves uploader identity/source format
- **Version-correct detail edits** - the skill detail page defaults to the latest version and edits the version currently being viewed; missing version UUIDs return 404 instead of 500
- **Rendered skill Markdown hardening** - dashboard link rendering escapes labels/hrefs, allowlists http/https/mailto, and renders unsafe links as plain text
- **Stdio reliability** - strict MCP clients no longer receive synthetic notification acknowledgements, and background daemon launch uses headless HTTP mode
- **Primer consistency** - the agent primer now uses `use mentisdb as your memory system` across TUI, mentisdb.com, and docs.mentisdb.com

### 0.9.6.42 — SSE, bind_host, Agent-Scoped Context
- **SSE on MCP servers** — `start_mcp_server` and `start_https_mcp_server` return `(ServerHandle, SseBroadcaster)`; GET endpoint serves `text/event-stream` with live MCP events; POST auto-broadcasts JSON-RPC results/errors as SSE events; cloudllm_mcp 0.3.0
- **bind_host parameter** — `standard_mcp_router` accepts `bind_host` (via `MENTISDB_BIND_HOST` env var, default `127.0.0.1`); binds to `0.0.0.0` for Docker containers and multi-homed machines
- **Agent-scoped recent context** — `mentisdb_recent_context` accepts optional `agent_id` parameter; when set, returns the last N thoughts from that agent, not the chain's last N overall

### 0.9.5.41 — MCP 2025-11-25 + Hermes Native Integration
- **MCP 2025-11-25 protocol support** — cloudllm_mcp upgraded to 0.2.1; initialize handshake negotiates "2025-11-25"; conditional Origin validation on streamable HTTP servers
- **Hermes native MentisDB MemoryProvider** — Nous Research's Hermes agent ships a direct memory plugin against the MentisDB MCP tools; first third-party agent integration without an MCP bridge
- **Dashboard fix** — "+Bootstrap New Chain" and "↺ Refresh" buttons restored after a one-shot guard bug dropped their click handlers during page transitions

### 0.9.4.40 — Search Speed-Up, Dashboard Perf, Read Sounds
- **Incremental LexicalIndex** — ranked search queries no longer rebuild the full BM25 index on every call; index is built once at chain open and updated incrementally on append. `query_ranked_lexical_content` drops from ~35 ms to ~237 µs (99.3% faster) on a 5K-thought chain.
- **Per-read-operation audio feedback** — when `MENTISDB_THOUGHT_SOUNDS=true`, every logged read command (search, list_chains, get_thought, etc.) plays a unique 60–150 ms sine-wave chime in the 2.5–4.5 kHz range. Write sounds remain square-wave at 250 Hz–1 kHz. You can tell read from write by ear.
- **Dashboard Agents page loads instantly** — two-phase loading: skeleton renders from the fast chain registry, then parallel per-chain agent fetches populate each section independently. Eliminates the serial O(chains × thoughts) blocking that made the page unusable with many chains.
- **Dashboard table alignment fix** — Recent Thoughts table on the agent detail page had misaligned columns (date under Content, Branch button under Date). Fixed.

### 0.9.3.39 — Ratatui TUI + Clipboard + Streamable HTTP Passthrough
- ratatui 0.30.0 TUI — live three-pane dashboard (server info, endpoints & TLS, tabbed tables for Chains/Agents/Skills, scrollable event log) with Tab/Shift+Tab pane cycling, vim-style contextual hint bar, and RAII terminal cleanup
- drag-to-select copies any text — drag the mouse across any content in any pane; on mouse release the selected rectangle is read directly from ratatui's render buffer and written to clipboard via arboard + OSC 52 (enables native Cmd+C in iTerm2, Terminal.app, kitty, WezTerm); real-time selection highlight via REVERSED cell style overlay
- 'c' key explicit copy — copies the focused item (chain key, agent ID, skill name, primer paste line, visible log lines) with a 2-second green toast confirmation
- seamless single-TUI lifecycle — one TUI for the entire daemon lifetime; no flash between startup progress overlay and running state
- startup crash overlay — startup failures shown as a red full-screen overlay with scrollable log, keeping TUI alive until the user quits
- agent primer panel — "Prime your agent" panel with single paste line for AI chat clients
- Streamable HTTP passthrough — stdio proxy forwards all JSON-RPC to `POST /` on the daemon's Streamable HTTP endpoint; full MCP protocol transparent to all MCP clients
- log panel newest-first display — most recent entries always visible at top; correct auto-scroll pinned to newest, not oldest
- chain key and skill name columns auto-sized to longest entry — no truncated names
- update dialog — centered modal when a newer GitHub release is available

### 0.9.2.38 — Smart Stdio Mode
- stdio smart daemon detection — `mentisdb --mode stdio` auto-detects a running daemon and proxies to it, or launches one in the background when none is found; falls back to local mode only if launch fails
- smart stdio mode — Claude Desktop / Cursor users no longer need to pre-start `mentisdb &` before launching the MCP client; the stdio subprocess handles daemon lifecycle transparently
- MCP-REST split brain fix — stdio proxy forwards `tools/list` and `tools/call` to the daemon's HTTP MCP endpoints so all clients share the same live in-memory chain cache
- `start_servers` shared service — single `MentisDbService` instance is shared across REST, HTTP-MCP, and stdio surfaces instead of each transport instantiating its own

### 0.9.1 — The Full-Feature Release
- Federated cross-chain search — `BranchesFrom` walks ancestor chains; ranked search transparently queries branch + ancestors
- Opt-in LLM extraction — GPT-4o (or any OpenAI-compatible endpoint) extracts structured `ThoughtInput` from raw text; review-before-append workflow
- pymentisdb Python client — full `MentisDbClient` on PyPI; LangChain `MentisDbMemory`; typed enums and relations
- Webhooks — HTTP POST callbacks on thought append with exponential backoff retries
- Wizard brew-first setup — interactive setup detects Homebrew `mcp-remote` and writes Claude Desktop config automatically

### 0.8.9 — Webhooks + Benchmark Stability
- Webhook delivery for thought append events (async HTTP callbacks with retry)
- Irregular verb lemma expansion in lexical search

### 0.8.8 — Episode Provenance + LLM Reranking
- `source_episode` field — full lineage from derived fact to source
- `DerivedFrom` relation kind
- Optional LLM reranking — pluggable cross-encoder reranker interface

### 0.8.7 — Custom Ontology
- `entity_type` field on thoughts (e.g. "bug_report", "architecture_decision")
- Per-chain entity type registry persisted in a sidecar
- Dashboard entity_type display and filter

### 0.8.6 — Search Quality + Branching
- Reciprocal Rank Fusion (RRF) — opt-in reranking merging lexical + vector + graph signals
- Memory branching — `BranchesFrom` relation; `POST /v1/chains/branch`
- Per-field BM25 DF cutoffs — document-frequency-based field weighting
- Irregular verb lemma expansion (~170 mappings, query-time only)

### 0.8.2 — Temporal, Dedup, Scopes, CLI
- Temporal facts — `valid_at`/`invalid_at` on thoughts; `as_of` query parameter
- Memory deduplication — Jaccard similarity threshold; auto-`Supersedes` relation
- Multi-level memory scopes — `MemoryScope` enum (`User`, `Session`, `Agent`)
- CLI tool — `mentisdb add`, `search`, `list`, `agents`, `chain` subcommands

---

## Benchmarks (0.9.1)

| Benchmark | Score |
|-----------|-------|
| **LoCoMo 10-persona R@10** | **74.0%** (1462/1977) |
| LoCoMo 10-persona R@20 | 80.8% |
| LoCoMo 10-persona R@50 | 88.5% |
| Single-hop | 78.0% |
| Multi-hop | 59.1% |
| Evaluation time | 94s (20.9 q/s) |

**Near-miss analysis:** 44.3% of misses don't appear in top-50 — lexical coverage gap, not ranking. Vector scores on misses near zero. Multi-hop is 19pp behind single-hop.

Reference scores (MemPalace BENCHMARKS.md):
- Hybrid v5, top-10, no rerank: 88.9% R@10
- Hybrid + Sonnet rerank, top-50: 100.0% R@5

---

## Competitive Position (April 2026)

| Feature | MentisDB | Hindsight | Cognee | LangMem | Mem0 | Graphiti |
|---------|----------|-----------|--------|---------|------|----------|
| Language | **Rust** | Python | Python | Python | Python | Python |
| Storage | **Embedded (sled)** | External (PG) | External | External | External | External |
| LLM Required | **No (opt-in)** | Yes | Yes | Yes | Yes | Yes |
| Local-First | **Yes** | No | No | No | Partial | No |
| Crypto Integrity | **Hash chain** | No | No | No | No | No |
| Hybrid Retrieval | **BM25+vec+graph** | 4-signal RRF | vec+graph | vec only | vec+kw | sem+kw+graph |
| Federated Search | **Yes** | No | No | No | No | No |
| Skills/Extensions | **Yes** | No | No | No | No | No |
| Webhooks | **Yes** | No | No | No | No | No |
| Benchmark | 74.0% (self) | SOTA (indep. verified) | N/A | N/A | N/A | N/A |

**MentisDB is the only local-first, zero-dependency, cryptographically-integrity-verified semantic memory with built-in hybrid retrieval — in Rust.**

---

## 1.0.0 — Production Stability

The next phase closes the remaining competitive gaps and ships what enterprise users need:

### Retrieval Quality (High Priority)
- **Multi-hop recall** — 19pp gap (59.1% vs 78.0% single-hop); entity coreference, deeper graph traversal, query expansion
- **Vector sidecar debugging** — near-zero vector scores on misses; FastEmbed loading issues
- **Auto-capture hooks** — agentmemory-style hooks for automatic memory capture (Hindsight retain pattern)

### Ecosystem Distribution
- **Native LangChain store** — `langchain-mentisdb` pip package with `BaseStore` implementation
- **LlamaIndex connector** — complete Python ecosystem coverage
- **Claude Code / Cursor plugins** — explicit integrations for major agent platforms

### Academic Benchmark Verification
- **Partner with an academic group** — Virginia Tech Sanghani Center or similar; independently verify LoCoMo and LongMemEval scores like Hindsight did

### Enterprise (Dual-Track Strategy)

**Track A: World-Class Library (Primary — Now)**
- **Crates.io excellence** — complete rustdoc, examples, cookbook, migration guides
- **Agent Memory Cookbook** — 10+ runnable patterns for RAG, multi-agent handoff, long-running tasks, episodic memory, semantic compression
- **Ecosystem connectors** — native LangChain store, LlamaIndex connector, Claude Code/Cursor plugins
- **Academic benchmark verification** — independent LoCoMo/LongMemEval validation (target: ≥85% R@10)
- **Zero-dep local-first** — no LLM required, embedded sled storage, hash-chain integrity

**Track B: Managed Cloud Service (Secondary — After Library Maturity)**
- **Multi-tenancy** — org/workspace/chains isolation, per-tenant API keys, RBAC
- **Managed embeddings** — pre-warmed FastEmbed/OpenAI-compatible model server, no cold starts
- **Managed HNSW and cloud optimizations** — hosted HNSW with persistence sharding, horizontal replication, and optional quantization for sub-100ms p95 at 10M+ vectors in the managed service
- **Usage metering & billing** — per-request, per-GB, per-seat tiers
- **SLA/observability** — health endpoints, latency percentiles, backup/restore API, audit logs
- **Import/Export UX** — one-click JSONL/CSV/Parquet, schema migration
- **Compliance** — SOC2, GDPR audit trails, VPC deployment option

**Pricing Target (When Track B Ships)**
| Tier | Monthly | Includes |
|------|---------|----------|
| Hobby | $0 | 10k vectors, 100 MB, 1k req/day, shared CPU |
| Starter | $49 | 1M vectors, 2 GB, 100k req/day, dedicated CPU, API keys |
| Pro | $199 | 10M vectors, 20 GB, 1M req/day, HNSW, custom embeddings, SSO |
| Enterprise | Custom | Unlimited, VPC, SLA, dedicated infra, audit logs, support |

**Go/No-Go Gate for Track B**: Library reaches ≥85% LoCoMo R@10, 3+ design partners committed, cookbook published, crates.io downloads >1k/mo.

### Developer Experience
- **Browser extension** — read/write memories from any webpage
- **Self-improving agent primitives** — agents that update their own skill files
- **Agent Memory Cookbook** — deployed at docs.mentisdb.com/cookbook; 10+ patterns with runnable code

---

## What Is HNSW? (And Why MentisDB Needs It)

**HNSW = Hierarchical Navigable Small World graphs**

It is the dominant algorithm for *approximate nearest neighbor search* (ANNS) at scale. Pinecone, Qdrant, Weaviate, Milvus, Vespa, and OpenSearch all use HNSW or variants.

### The Problem
Exact cosine search over 10M vectors of dimension 1536 requires ~60 GB RAM and scans take seconds. Linear scan doesn't scale.

### The HNSW Idea
Build a multi-layer graph where:
- **Layer 0** = all vectors, connected to ~16-64 nearest neighbors (dense connectivity)
- **Layer 1** = subset (~1/4), longer-range connections
- **Layer 2** = subset of layer 1, even longer range
- ...
- **Top layer** = few entry points, global navigation

Search starts at the top layer, greedily walks toward the query vector, drops down a layer, repeats. Typical hops: 50-200 total distance computations vs 10M for brute force.

### Key Parameters
- `M` = max connections per node (default 16-48). Higher = better recall, more memory.
- `efConstruction` = search width during index build (default 100-400). Higher = better graph quality, slower build.
- `efSearch` = search width at query time (default 50-200). Higher = better recall, slower query.

### Why MentisDB Needs It
| Current | With HNSW |
|---------|-----------|
| Linear scan, exact cosine | Sublinear graph search, ~95-99% recall |
| ~50k vectors practical | 10M+ vectors practical |
| p95 ~200ms at 50k | p95 ~10-50ms at 1M+ |
| Memory: 4 bytes × dim × N | Memory: ~1.1× vectors + graph edges |

### Integration Path for MentisDB
1. **Keep exact f32 as reference** — never lose deterministic, auditable search
2. **HNSW as a `VectorSearchBackend` implementation** — same trait, swapped implementation; shipped in 0.10.3.48 and enabled by default
3. **Quantized HNSW** — deferred; raw f32 HNSW outperformed the quantized prototype on both recall and latency
4. **Filter-aware search** — HNSW with bitmap/pre-filter support (MentisDB's filter-first model maps well)
5. **Incremental build** — HNSW supports online inserts; deletes via tombstone + periodic rebuild

### Rust Crates Evaluated
- `hnsw` — selected for the 0.10.3.48 backend; pure Rust, no BLAS, no_std-friendly core, serde support for persistence
- `arroy` — used by Meilisearch, supports mmap, incremental
- `vectorsearch` — newer, SIMD-optimized
- Or wrap `faiss`/`hnswlib` via FFI (heavier deps)

**Decision**: Prototype `hnsw` crate behind feature flag after cookbook v1. Benchmark against exact f32 at 100k, 1M, 10M. Target: ≥95% recall@10 with <50ms p95 at 1M vectors.

---

## Agent Memory Cookbook (docs.mentisdb.com/cookbook)

**Status**: In progress — see `/docs/cookbook/` for source files.

### Table of Contents

#### Part 0: Foundations
0.1  **Why Agent Memory Matters** — the problem space, LLM context limits, hallucination reduction
0.2  **MentisDB Mental Model** — chains, thoughts, agents, embeddings, retrieval modes
0.3  **Quickstart: Your First Memory** — 5-minute tutorial from `cargo add mentisdb` to first search
0.4  **Search-First Discipline** — the `recent_context → ranked_search → append` loop

#### Part 1: Core Patterns (Library Users)
1.1  **Episodic Task Memory** — remember what you did, why, and what failed across sessions
1.2  **Semantic Fact Extraction** — LLM-extracted `Decision`, `Constraint`, `Insight` with review workflow
1.3  **Multi-Agent Handoff** — `Summary` + `role: Checkpoint` + `BranchesFrom` for seamless context transfer
1.4  **Long-Running Project Memory** — entity types, temporal validity, dedup, cross-chain federation
1.5  **RAG Over Agent History** — hybrid lexical + vector + graph retrieval for "what did we decide about X?"
1.6  **Preference Learning** — `PreferenceUpdate` thoughts, confidence decay, conflict resolution
1.7  **Error/Mistake Memory** — `Mistake` + `Correction` pairs, automatic `LessonLearned` synthesis

#### Part 2: Advanced Patterns
2.1  **Semantic Compression** — `Summary` checkpoints, sliding window, importance-weighted retention
2.2  **Cross-Session Continuity** — agent-scoped context, session scopes, identity persistence
2.3  **Dynamic Skill Loading** — upload skill → version → `mentisdb_read_skill` → execute
2.4  **Federated Team Memory** — branch per feature/tenant, merge with `BranchesFrom` provenance
2.5  **Webhook-Driven Workflows** — append → HTTP callback → downstream processing → write back

#### Part 3: Production Hardening
3.1  **Embedding Provider Selection** — local (256d) vs FastEmbed (384d) vs OpenAI (1536d/3072d) tradeoffs
3.2  **Vector Sidecar Management** — rebuild strategies, freshness, incremental sync, corruption recovery
3.3  **Retrieval Tuning** — BM25 weights, RRF parameters, vector thresholds, graph expansion depth
3.4  **Benchmarking Your Memory** — LoCoMo/LongMemEval methodology, custom eval sets, CI integration
3.5  **Deployment Patterns** — stdio MCP, HTTP MCP, REST, library-only, systemd, Docker, Railway

#### Part 4: Using MentisDB with Agentic Harnesses (User On-Ramp)
4.1  **OpenCode** — current harness, local MCP + skill store primer
4.2  **Claude Code (CLI)** — `claude mcp add` setup, skill priming, working session
4.3  **Claude Desktop** — stdio MCP config, one-click memory for desktop Claude
4.4  **Codex (OpenAI CLI)** — `~/.codex/config.toml` MCP setup
4.5  **Hermes (Nous Research)** — native `MemoryProvider` integration
4.6  **Cursor** — IDE MCP server config, `.cursorrules` priming
4.7  **Continue.dev** — open-source IDE assistant with MentisDB provider
4.8  **Cline & Aider** — VS Code extension and CLI tool integrations
4.9  **Zed, Windsurf, Other MCP Clients** — generic config patterns

#### Part 5: Custom Agent Recipes (Copy-Paste Code)
5.1  **Rust: Minimal Agent with Memory** — 50-line complete example
5.2  **Python: LangChain + MentisDB** — `MentisDbMemory` conversation buffer
5.3  **Python: Custom Agent with pymentisdb** — direct client, no framework
5.4  **TypeScript: MCP Client Integration** — stdio + HTTP transport patterns
5.5  **CLI: Daily Standup Memory** — `mentisdb add --type Decision --tags standup`
5.6  **Dashboard: Memory Archaeology** — search, filter, graph traverse, export

---

## Next Actions (This Week)
1. Create `/docs/cookbook/` directory structure
2. Write 0.1–0.4 (Foundations) as markdown → HTML pipeline
3. Write 1.1 (Episodic Task Memory) as first complete pattern
4. Add cookbook nav to docs.mentisdb.com index.html
5. Publish to GitHub Pages / Railway static site

---

## What's Changed Since April 10

MentisDB closed 15+ feature gaps in 11 releases (0.8.2 → 0.9.1). The original competitive analysis identified temporal facts, memory dedup, multi-level scopes, CLI, and episode provenance as major gaps. All shipped.

New unique advantages since April 10:
- Federated cross-chain search (no competitor has this)
- Skill registry with versioning and revocation
- Webhooks
- Opt-in LLM extraction (keeps no-LLM core as differentiator)

New competitive threats:
- **Hindsight** — independently verified SOTA benchmarks; managed service
- **Cognee v1.0** — 15k stars, graph-based memory
- **Hermes** — open-source agent by Nous Research; now natively integrated with MentisDB via MemoryProvider
- **LangMem** — default in LangGraph Platform deployments; massive distribution advantage

The next battle is ecosystem and distribution, not features.
