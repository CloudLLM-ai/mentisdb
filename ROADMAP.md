# MentisDB Roadmap

## Shipped (0.8.2 → 0.9.3.39)

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
- stdio smart daemon detection — `mentisdbd --mode stdio` auto-detects a running daemon and proxies to it, or launches one in the background when none is found; falls back to local mode only if launch fails
- smart stdio mode — Claude Desktop / Cursor users no longer need to pre-start `mentisdbd &` before launching the MCP client; the stdio subprocess handles daemon lifecycle transparently
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

### Enterprise
- **MentisDB Cloud** — managed service, zero-infrastructure deployment
- **Token tracking** — per-agent usage metrics
- **Compliance exports** — SOC2, GDPR audit trails

### Developer Experience
- **Browser extension** — read/write memories from any webpage
- **Self-improving agent primitives** — agents that update their own skill files

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
- **Cognee v1.0** — 15k stars, Hermes integration
- **LangMem** — default in LangGraph Platform deployments; massive distribution advantage

The next battle is ecosystem and distribution, not features.
