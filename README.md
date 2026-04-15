<img src="logo.svg" alt="MentisDB logo" height="48" align="left" style="margin-right:12px" />

# MentisDB

MentisDB is a **durable semantic memory engine and versioned skill registry** for AI agents — a persistent, hash-chained brain that survives context resets, model swaps, and team turnover.

It stores semantically typed thoughts in an append-only, hash-chained memory log through a swappable storage adapter layer. The skill registry is a git-like immutable version store for agent instruction bundles — every upload is a new version, history is never overwritten, and every version is cryptographically signable.

---

## Why MentisDB

**Harness Swapping** — the same durable memory works across every AI coding environment. Connect Claude Code, OpenAI Codex, GitHub Copilot CLI, Qwen Code, Cursor, VS Code, or any MCP-capable host to the same `mentisdbd` daemon and your agents share one brain, regardless of which tool you picked up today.

**Zero Knowledge Loss Across Context Boundaries** — when an agent's context window fills, it writes a `Summary` checkpoint to MentisDB, compacts, reloads `mentisdb_recent_context`, and continues without losing a single decision. Chat history is ephemeral. MentisDB is permanent.

**Fleet Orchestration at Scale** — one project manager agent decomposes work, dispatches a parallel fleet of specialists, each pre-warmed with shared memory, and synthesizes results wave by wave. MentisDB is the coordination substrate: every agent reads from the same chain and writes its lessons back. The fleet's collective intelligence compounds.

**Versioned Skill Registry** — skills are not just stored, they are versioned like a git repository. Every upload to an existing `skill_id` creates a new immutable version (stored as a unified diff). Any historical version is reconstructable. Skills can be deprecated or revoked while full audit history is preserved. Uploading agents with registered Ed25519 keys must cryptographically sign their uploads — provenance is verifiable, not assumed.

**Session Resurrection** — any agent can call `mentisdb_recent_context` and immediately know exactly where the project stands, what decisions were made, what traps were already hit, and what comes next — without re-reading code, re-running exploratory searches, or asking the human to re-explain context that was earned through hours of work.

**Self-Improving Agent Fleets** — agents upload updated skill files after learning something new. A skill checked in at the start of a project is better by the end of it. Combine with Ed25519 signing to create a verifiable, tamper-evident record of which agent authored which version of institutional knowledge.

**Multi-Agent Shared Brain** — multiple agents, multiple roles, multiple owners can write to the same chain key simultaneously. Every thought carries a stable `agent_id`. Queries filter by agent identity, thought type, role, tags, concepts, importance, and time windows. The chain represents the full collective intelligence of an entire orchestration system, not just one session.

**Lessons That Outlive Models** — architectural decisions, hard constraints, non-obvious failure modes, and retrospectives written to MentisDB survive chat loss, model upgrades, and team changes. The knowledge compounds instead of evaporating. A new engineer or a new agent boots up, loads the chain, and inherits everything the team learned.

---

## Quick Start

Install the daemon:

```bash
cargo install mentisdb
```

Connect your local AI tools the fast way:

```bash
mentisdbd wizard
```

Or target one integration explicitly:

```bash
mentisdbd setup codex
mentisdbd setup all --dry-run
mentisdbd wizard
mentisdbd add "The sky is blue"
mentisdbd search "cache invalidation" --limit 5 --scope session
mentisdbd agents
```

Then start the daemon:

```bash
mentisdbd
```

On an interactive first run with no configured client integrations,
`mentisdbd` offers to launch the setup wizard immediately after startup so you
do not have to guess the next command.

Run persistently after closing your SSH session:

```bash
nohup mentisdbd &
```

Modern MCP clients bootstrap themselves from the MCP handshake:

- `initialize.instructions` tells the agent to read `mentisdb://skill/core`
- `resources/read(mentisdb://skill/core)` delivers the embedded operating skill
- `GET /mentisdb_skill_md` remains available only as a compatibility fallback

If you need to wire a tool manually, here are the raw MCP commands/configs:

```bash
# Claude Code
claude mcp add --transport http mentisdb http://127.0.0.1:9471

# OpenAI Codex
codex mcp add mentisdb --url http://127.0.0.1:9471

# Qwen Code
qwen mcp add --transport http mentisdb http://127.0.0.1:9471

# GitHub Copilot CLI — use /mcp add in interactive mode,
# or write ~/.copilot/mcp-config.json manually (see below)
```

---

## What Is In This Folder

`mentisdb/` contains:

- the standalone `mentisdb` library crate
- server support for HTTP MCP and REST, enabled by default
- the `mentisdbd` daemon binary
- dedicated tests under `mentisdb/tests`

---

## Makefile

A `Makefile` is included at the repository root. All common workflows have a target:

```bash
make build          # fmt + release build
make build-mentisdbd # build only the daemon binary
make release        # fmt, check, clippy, build, test, doc in sequence
make fmt            # cargo fmt
make check          # cargo check (lib + binary)
make clippy         # cargo fmt + clippy --all-targets -D warnings
make test           # cargo test
make bench          # Criterion benchmarks, output tee'd to /tmp/mentisdb_bench_results.txt
make doc            # cargo doc --all-features
make install        # cargo install --path . --locked
make publish        # cargo publish
make publish-dry-run
make clean
make help           # list all targets with descriptions
```

---

## Build

```bash
make build
```

Or directly with Cargo:

```bash
cargo build --release
```

Build only the library without the default daemon/server stack:

```bash
cargo build --no-default-features
```

---

## Test

```bash
make test
```

Or directly:

```bash
cargo test
```

Run tests for the library-only build:

```bash
cargo test --no-default-features
```

Run rustdoc tests:

```bash
cargo test --doc
```

---

## Benchmarks

MentisDB ships a Criterion benchmark suite and a harness-free HTTP concurrency benchmark:

```bash
make bench
```

Or directly:

```bash
cargo bench
```

Results are also written to `/tmp/mentisdb_bench_results.txt` so numbers persist across terminal sessions.

Benchmark coverage:

- `benches/thought_chain.rs` — 10 benchmarks: append throughput, query latency, traversal patterns
- `benches/search_baseline.rs` — 4 benchmarks: lexical/filter-first search baseline over content, registry text, indexed+text intersections, and newest-tail limits
- `benches/search_ranked.rs` — 4 benchmarks: additive ranked retrieval over lexical content, filtered ranked queries, and heuristic fallback, plus a baseline append-order comparison
- `benches/skill_registry.rs` — 12 benchmarks: skill upload, search, delta reconstruction, lifecycle
- `benches/http_concurrency.rs` — starts `mentisdbd` in-process on a random port; measures write and read throughput at 100 / 1k / 10k concurrent Tokio tasks with p50/p95/p99 latency reporting

Baseline numbers from the `DashMap` concurrent chain lookup refactor: **750–930 read req/s at 10k concurrent tasks**, compared to a sequential bottleneck on the previous `RwLock<HashMap>` implementation.

---

## Generate Docs

```bash
make doc
```

Or directly:

```bash
cargo doc --no-deps
```

Generate docs for the library-only build:

```bash
cargo doc --no-deps --no-default-features
```

---

## Run The Daemon

The standalone executable is `mentisdbd`.

Run it from source:

```bash
cargo run --bin mentisdbd
```

Install it from the crate directory:

```bash
make install
# or
cargo install --path . --locked
```

`mentisdbd` now owns both daemon startup and local integration setup:

```bash
mentisdbd setup codex
mentisdbd setup all --dry-run
mentisdbd wizard
mentisdbd add "The sky is blue"
mentisdbd search "cache invalidation" --limit 5 --scope session
mentisdbd agents
mentisdbd
```

When it starts, it serves:

- an MCP server
- a REST server
- an HTTPS web dashboard

Before serving traffic, it:

- migrates or reconciles discovered chains to the current schema and default storage adapter
- verifies chain integrity and attempts repair from valid local sources when possible
- migrates the skill registry from V1 to V2 format if needed (idempotent; safe to run repeatedly)
- migrates chain relations from V2 to V3 format to add temporal edge validity (`valid_at`/`invalid_at`) if needed (idempotent; safe to run repeatedly)

Once startup completes, it prints:

- the active chain directory, default chain key, and bound MCP/REST/dashboard addresses
- a catalog of all exposed HTTP endpoints with one-line descriptions
- a per-chain summary with version, adapter, thought count, and per-agent counts

---

## Daemon Configuration

`mentisdbd` is configured with environment variables:

- `MENTISDB_DIR`
  Directory where MentisDB storage adapters store chain files.
- `MENTISDB_DEFAULT_CHAIN_KEY`
  Default `chain_key` used when requests omit one. Default: `borganism-brain`.
  `MENTISDB_DEFAULT_KEY` is accepted as a deprecated alias.
- `MENTISDB_STORAGE_ADAPTER`
  Default storage backend for newly created chains. Only `binary` is supported for
  new chains (JSONL is deprecated — existing JSONL chains remain readable).
  Default: `binary`
- `MENTISDB_VERBOSE`
  When unset, verbose interaction logging defaults to `true`. Supported explicit values:
  `1`, `0`, `true`, `false`.
- `MENTISDB_LOG_FILE`
  Optional path for interaction logs. When set, MentisDB writes interaction logs to that file
  even if console verbosity is disabled. If `MENTISDB_VERBOSE=true`, the same lines are also
  mirrored to the console logger.
- `MENTISDB_BIND_HOST`
  Bind host for both HTTP servers. Default: `127.0.0.1`
- `MENTISDB_MCP_PORT`
  MCP server port. Default: `9471`
- `MENTISDB_REST_PORT`
  REST server port. Default: `9472`
- `MENTISDB_DASHBOARD_PORT`
  HTTPS dashboard port. Default: `9475`. Set to `0` to disable the web dashboard.
- `MENTISDB_DASHBOARD_PIN`
  Optional PIN required to access the dashboard. Leave unset only for trusted localhost use.
- `MENTISDB_AUTO_FLUSH`
  Controls per-write durability of the `binary` storage adapter.
  - `true` (default): every `append_thought` flushes to disk immediately. Full durability.
  - `false`: writes are batched and flushed every 16 appends (`FLUSH_THRESHOLD`). Up to 15
    thoughts may be lost on a hard crash or power failure, but write throughput increases
    significantly for multi-agent hubs with many concurrent writers.
  Supported values: `1`, `0`, `true`, `false`. Has no effect on the `jsonl` adapter.
- `MENTISDB_GROUP_COMMIT_MS`
  Group-commit window in milliseconds for the background binary writer.
  The writer batches appends within this window before flushing to disk.
  Lower values = lower latency; higher values = better throughput.
  Default: `2`
- `MENTISDB_UPDATE_CHECK`
  Background GitHub release check for `mentisdbd`. Enabled by default; set `0`, `false`, `no`,
  or `off` to disable update checks after startup. Default: `true`
- `MENTISDB_UPDATE_REPO`
  Optional GitHub `owner/repo` override used by the updater. Default: `CloudLLM-ai/mentisdb`
- `MENTISDB_HTTPS_MCP_PORT`
  HTTPS MCP server port. Default: `9473`. Set to `0` to disable HTTPS MCP.
- `MENTISDB_HTTPS_REST_PORT`
  HTTPS REST server port. Default: `9474`. Set to `0` to disable HTTPS REST.
- `MENTISDB_TLS_CERT`
  Path to a PEM-encoded TLS certificate for the HTTPS servers and dashboard.
  Default: `<MENTISDB_DIR>/tls/cert.pem`
- `MENTISDB_TLS_KEY`
  Path to a PEM-encoded TLS private key for the HTTPS servers and dashboard.
  Default: `<MENTISDB_DIR>/tls/key.pem`
- `MENTISDB_STARTUP_SOUND`
  Play the 4-note "men-tis-D-B" startup jingle. Default: `true`. Set `0`, `false`, `no`,
  or `off` to silence.
- `MENTISDB_THOUGHT_SOUNDS`
  Play a unique short sound for each thought type on append. Default: `false`. Set `1`,
  `true`, `yes`, or `on` to enable.
- `MENTISDB_DEDUP_THRESHOLD`
  Jaccard similarity threshold for automatic deduplication on append (0.0–1.0). When
  unset, auto-dedup is disabled. When set, a new thought whose content is sufficiently
  similar to a recent thought receives a `Supersedes` relation instead of being stored
  as a duplicate.
- `MENTISDB_DEDUP_SCAN_WINDOW`
  Number of recent thoughts to scan for dedup comparison. Default: `64`.

Example — full durability (production default):

```bash
MENTISDB_DIR=/tmp/mentisdb \
MENTISDB_DEFAULT_CHAIN_KEY=borganism-brain \
MENTISDB_STORAGE_ADAPTER=binary \
MENTISDB_VERBOSE=true \
MENTISDB_LOG_FILE=/tmp/mentisdb/mentisdbd.log \
MENTISDB_BIND_HOST=127.0.0.1 \
MENTISDB_MCP_PORT=9471 \
MENTISDB_REST_PORT=9472 \
MENTISDB_DASHBOARD_PIN=change-me \
MENTISDB_AUTO_FLUSH=true \
cargo run --bin mentisdbd
```

Example — high-throughput write mode (multi-agent hub):

```bash
MENTISDB_DIR=/var/lib/mentisdb \
MENTISDB_AUTO_FLUSH=false \
MENTISDB_BIND_HOST=0.0.0.0 \
mentisdbd
```

### Automatic Update Check

`mentisdbd` checks GitHub releases in the background after startup and can offer
to update itself with `cargo install`.

- checks are enabled by default
- version comparison uses only the first three numeric components, so a tag like
  `0.6.1.14` is treated as core version `0.6.1`
- interactive terminals get an ASCII prompt window with `Y` / `N`
- non-interactive terminals never block; they print the exact manual `cargo install` command instead

Disable the automatic check:

```bash
MENTISDB_UPDATE_CHECK=0 \
mentisdbd
```

---

## Server Surfaces

MCP endpoints:

- `GET /health`
- `POST /`
- `POST /tools/list`
- `POST /tools/execute`

REST endpoints:

- `GET /health`
- `GET /mentisdb_skill_md`
- `GET /v1/skills`
- `GET /v1/skills/manifest`
- `GET /v1/chains`
- `POST /v1/chains/merge`
- `POST /v1/chains/branch`
- `POST /v1/vectors/rebuild`
- `POST /v1/bootstrap`
- `POST /v1/agents`
- `POST /v1/agent`
- `POST /v1/agent-registry`
- `POST /v1/agents/upsert`
- `POST /v1/agents/description`
- `POST /v1/agents/aliases`
- `POST /v1/agents/keys`
- `POST /v1/agents/keys/revoke`
- `POST /v1/agents/disable`
- `POST /v1/thought`
- `POST /v1/thoughts`
- `POST /v1/thoughts/genesis`
- `POST /v1/thoughts/traverse`
- `POST /v1/retrospectives`
- `POST /v1/search`
- `POST /v1/lexical-search`
- `POST /v1/ranked-search`
- `POST /v1/context-bundles`
- `POST /v1/recent-context`
- `POST /v1/memory-markdown`
- `POST /v1/import-markdown`
- `POST /v1/skills/upload`
- `POST /v1/skills/search`
- `POST /v1/skills/read`
- `POST /v1/skills/versions`
- `POST /v1/skills/deprecate`
- `POST /v1/skills/revoke`
- `POST /v1/webhooks`
- `GET /v1/webhooks`
- `GET /v1/webhooks/{id}`
- `DELETE /v1/webhooks/{id}`
- `POST /v1/head`

### Append Thought — `POST /v1/thoughts`

```json
{
  "chain_key":    "my-chain",
  "agent_id":     "my-agent",
  "agent_name":   "My Agent",
  "thought_type": "LessonLearned",
  "role":         "Execution",
  "content":      "...",
  "scope":        "session",
  "tags":         ["tag1"],
  "concepts":     ["concept1"],
  "importance":   0.9,
  "confidence":   0.8,
  "refs":         [14, 22],
  "relations": [
    { "kind": "CausedBy",      "target_id": "<uuid>" },
    { "kind": "ContinuesFrom", "target_id": "<uuid>", "chain_key": "other-chain" },
    { "kind": "Supersedes",    "target_id": "<uuid>", "valid_at": "2025-01-01T00:00:00Z", "invalid_at": "2025-12-31T23:59:59Z" }
  ]
}
```

`chain_key`, `role`, `scope`, `tags`, `concepts`, `importance`, `confidence`, `refs`, and `relations` are optional.  
`relations[].kind` accepts: `References`, `Summarizes`, `Corrects`, `Invalidates`, `CausedBy`, `Supports`, `Contradicts`, `DerivedFrom`, `ContinuesFrom`, `RelatedTo`, `Supersedes`.  
`relations[].chain_key` is optional — omit for intra-chain edges, set for cross-chain references.  
`relations[].valid_at` and `relations[].invalid_at` are optional RFC 3339 timestamps that define when a relation edge is temporally valid, enabling point-in-time queries with `as_of`.

---

## Search Semantics

MentisDB keeps its baseline thought search surface **filter-first and append-order**. Ranked, graph-aware, and vector retrieval are additive surfaces layered on top of that stable baseline.

Today, the main search APIs are:

- `MentisDb::query(&ThoughtQuery)`
- `POST /v1/search`
- `mentisdb_search`

Current behavior:

- indexed filters narrow the candidate set for `thought_type`, `role`, `agent_id`, tags, and concepts
- `text` is a case-insensitive substring match over:
  - thought `content`
  - `agent_id`
  - tags
  - concepts
  - agent-registry display name, aliases, owner, and description
- results are returned in **append order**
- `limit` keeps the **newest matching tail** after filtering rather than applying a ranking score

That means plain `ThoughtQuery` / `/v1/search` behavior is deterministic and explainable, but that baseline path is **not** BM25, hybrid, or vector retrieval. Additive ranked and graph-aware retrieval now exist on separate crate, REST, and MCP surfaces.

Examples:

```rust,no_run
use mentisdb::{MentisDb, ThoughtQuery, ThoughtType};
use std::path::PathBuf;

# fn main() -> std::io::Result<()> {
let chain = MentisDb::open(&PathBuf::from("/tmp/tc_query"), "agent1", "Agent", None, None)?;

let lexical = ThoughtQuery::new()
    .with_types(vec![ThoughtType::Decision])
    .with_tags_any(["search"])
    .with_text("latency");

let results = chain.query(&lexical);
# let _ = results;
# Ok(())
# }
```

```json
{
  "chain_key": "mentisdb",
  "thought_types": ["Decision"],
  "tags_any": ["search"],
  "text": "latency",
  "limit": 20
}
```

Design note:

- treat this lexical/filter-first behavior as the baseline
- keep ranked, vector, and hybrid retrieval as additive, explicitly documented surfaces on top of that baseline
- do not silently change the semantics of `ThoughtQuery` or `/v1/search` from append-order filtering to score-ranked retrieval

The dedicated benchmark `benches/search_baseline.rs` and evaluation tests in `tests/search_eval_tests.rs` are intended to preserve that baseline while world-class search evolves.

### Ranked Search

MentisDB now also exposes an additive ranked-search surface for direct crate use:

- `RankedSearchQuery`
- `RankedSearchGraph`
- `MentisDb::query_context_bundles(&RankedSearchQuery)`
- `MentisDb::query_ranked(&RankedSearchQuery)`
- `RankedSearchBackend::{Lexical, Hybrid, LexicalGraph, HybridGraph, Heuristic}`

This surface is intentionally separate from `ThoughtQuery`.

`ThoughtQuery` still decides **which** thoughts are eligible. Ranked search then decides **how those eligible thoughts are ordered**.

Current ranked-search behavior:

- `RankedSearchQuery.filter` uses the same deterministic semantics as `MentisDb::query`
- when `text` normalizes to a non-empty query, the backend is `lexical` or `hybrid` depending on whether a managed vector sidecar is active for the current handle
- lexical ranking scores indexed thought text plus agent metadata from the filtered candidate set
- when a managed vector sidecar is active for the current handle, ranked search blends lexical scoring with vector similarity and the backend becomes `hybrid`
- when `graph` is enabled alongside non-empty `text`, the backend becomes `lexical_graph` or `hybrid_graph` depending on whether vector scoring is available
- graph expansion starts from lexical seed hits, walks `refs` and typed `relations`, and can surface supporting context that did not lexically match
- `as_of` (RFC 3339 timestamp) filters to only relation edges that were valid at the given point in time, using each edge's `valid_at`/`invalid_at` temporal window — edges without temporal bounds are always included
- `scope` narrows results to thoughts tagged with a matching memory scope (`user`, `session`, or `agent`); omitted scope returns all
- when `text` is absent or blank, the backend falls back to `heuristic`
- heuristic ordering uses lightweight importance, confidence, and recency signals
- `total_candidates` counts the hits after filter application and ranked-signal gating, before final `limit` truncation
- each ranked hit includes `matched_terms` plus `match_sources` such as `content`, `tags`, `concepts`, `agent_id`, and `agent_registry`
- each ranked hit also includes a `vector` score component when semantic sidecars contribute to the ranking
- graph-expanded hits also expose `graph_distance`, `graph_seed_paths`, `graph_relation_kinds`, and `graph_path` provenance so callers can explain why a supporting thought surfaced
- grouped context delivery is available through `query_context_bundles`, which anchors supporting graph hits beneath lexical seeds in deterministic order

Lexical ranked example:

```rust,no_run
use mentisdb::{MentisDb, RankedSearchQuery, ThoughtQuery, ThoughtType};
use std::path::PathBuf;

# fn main() -> std::io::Result<()> {
let chain = MentisDb::open(&PathBuf::from("/tmp/tc_ranked"), "agent1", "Agent", None, None)?;

let ranked = RankedSearchQuery::new()
    .with_filter(
        ThoughtQuery::new()
            .with_types(vec![ThoughtType::Decision])
            .with_tags_any(["search"]),
    )
    .with_text("latency ranking")
    .with_limit(10);

let results = chain.query_ranked(&ranked);
assert!(matches!(
    results.backend,
    mentisdb::RankedSearchBackend::Lexical | mentisdb::RankedSearchBackend::Hybrid
));
# let _ = results;
# Ok(())
# }
```

Lexical + graph ranked example:

```rust,no_run
use mentisdb::{MentisDb, RankedSearchGraph, RankedSearchQuery, ThoughtQuery, ThoughtType};
use mentisdb::search::GraphExpansionMode;
use std::path::PathBuf;

# fn main() -> std::io::Result<()> {
let chain = MentisDb::open(&PathBuf::from("/tmp/tc_ranked"), "agent1", "Agent", None, None)?;

let ranked = RankedSearchQuery::new()
    .with_filter(
        ThoughtQuery::new()
            .with_types(vec![ThoughtType::Decision])
            .with_tags_any(["search"]),
    )
    .with_text("latency ranking")
    .with_graph(
        RankedSearchGraph::new()
            .with_max_depth(1)
            .with_mode(GraphExpansionMode::Bidirectional),
    )
    .with_limit(10);

let results = chain.query_ranked(&ranked);
# let _ = results;
# Ok(())
# }
```

Temporal and scoped ranked example:

```rust,no_run
use mentisdb::{MentisDb, MemoryScope, RankedSearchQuery, ThoughtQuery};
use std::path::PathBuf;

# fn main() -> std::io::Result<()> {
let chain = MentisDb::open(&PathBuf::from("/tmp/tc_ranked"), "agent1", "Agent", None, None)?;

let ranked = RankedSearchQuery::new()
    .with_filter(ThoughtQuery::new())
    .with_text("cache invalidation")
    .with_as_of("2025-06-01T00:00:00Z")
    .with_scope(MemoryScope::Session)
    .with_limit(10);

let results = chain.query_ranked(&ranked);
# let _ = results;
# Ok(())
# }
```

Vector-backed ranked example:

```rust,no_run
use mentisdb::{MentisDb, RankedSearchBackend, RankedSearchQuery};
use mentisdb::search::LocalTextEmbeddingProvider;
use std::path::PathBuf;

# fn main() -> std::io::Result<()> {
let mut chain = MentisDb::open_with_key(&PathBuf::from("/tmp/tc_ranked_vectors"), "semantic-ranked")?;
chain.append("planner", mentisdb::ThoughtType::Decision, "Tail latency ceiling for the Europe rollout.")?;

// Register the built-in local-text-v1 embedding sidecar so ranked search can
// blend lexical and semantic signals for the current chain handle.
chain.manage_vector_sidecar(LocalTextEmbeddingProvider::new())?;

let ranked = chain.query_ranked(&RankedSearchQuery::new().with_text("performance budget"));

assert_eq!(ranked.backend, RankedSearchBackend::Hybrid);
assert!(ranked.hits.iter().any(|hit| hit.score.vector > 0.0));
# Ok(())
# }
```

Grouped context example:

```rust,no_run
use mentisdb::{MentisDb, RankedSearchGraph, RankedSearchQuery, ThoughtQuery};
use mentisdb::search::GraphExpansionMode;
use std::path::PathBuf;

# fn main() -> std::io::Result<()> {
let chain = MentisDb::open(&PathBuf::from("/tmp/tc_ranked"), "agent1", "Agent", None, None)?;

let bundles = chain.query_context_bundles(
    &RankedSearchQuery::new()
        .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
        .with_text("latency ranking")
        .with_graph(
            RankedSearchGraph::new()
                .with_mode(GraphExpansionMode::Bidirectional)
                .with_max_depth(2),
        )
        .with_limit(5),
);
# let _ = bundles;
# Ok(())
# }
```

Product rule:

- keep `ThoughtQuery` stable and explainable for append-order filtering
- evolve ranked search as a separate surface with its own benchmarks, tests, and transport layers
- treat registry-aware filtering and future transport exposure as additive work on top of the current crate API
- use `query_ranked` for flat ranked retrieval and `query_context_bundles` when the caller wants seed-anchored support context instead of one mixed list

The ranked-search benchmark `benches/search_ranked.rs` and evaluation tests in `tests/search_ranked_eval_tests.rs` are the guardrails for that additive surface.

### Search Scoring (0.8.1)

Starting in 0.8.1, ranked search uses six key improvements:

- **Porter stemming** — the lexical tokenizer stems all tokens before indexing and querying so word variants share a common root (e.g. `prefers`/`preferred`/`preferences` → `prefer`).
- **Smooth exponential vector-lexical fusion** — replaces the 0.8.0 step-function boosts with a continuous decay curve: `vector × (1 + 35 × exp(-lexical / 3.0))`. Pure-semantic matches get ~36× amplification; by lexical=3.0 the boost has decayed to ~12×; at lexical=6.0 it's additive. This eliminates discontinuities between tiers.
- **Session cohesion scoring** — thoughts within ±8 positions of a high-scoring lexical seed (score ≥ 3.0) receive a proximity boost up to 0.8, decaying linearly with distance. This surfaces evidence turns adjacent to the matching turn but sharing no lexical terms.
- **BM25 document-frequency cutoff** — terms appearing in >30% of documents (corpus ≥ 20 docs) are skipped during scoring. This filters non-discriminative entity names without blanket stopword removal.
- **Importance-weighted scoring** — replaces flat multipliers with a differential boost proportional to lexical score: `lexical × (importance - 0.5) × 0.3`. User-originated thoughts (importance ≈ 0.8) outrank verbose assistant responses (importance ≈ 0.2) in close BM25 races.
- **Graph expansion limits and relation boosts** — `MAX_GRAPH_SEEDS=20` bounds BFS cost; `ContinuesFrom` boost raised to 0.30; graph proximity 1.0/depth.

These changes took LongMemEval R@5 from 57.2% to 67.6% and LoCoMo 2-persona R@10 from 55.8% to 88.7%.

### Search Scoring (0.8.2)

Starting in 0.8.2, ranked search adds temporal, scoped, and dedup-aware features:

- **Temporal `as_of` point-in-time filtering** — `RankedSearchQuery::with_as_of(rfc3339)` restricts graph expansion to only relation edges whose `valid_at`/`invalid_at` window covers the given timestamp. Edges without temporal bounds are always included. This enables queries like "what did the agent know at the start of the sprint?"
- **Memory scope filtering via `scope` tag** — thoughts carry a `scope` field (`user`, `session`, or `agent`) stored as `scope:<value>` tags. `RankedSearchQuery::with_scope(MemoryScope::Session)` narrows results to session-scoped memories only. Omitting scope returns thoughts from all scopes.
- **Auto-dedup with `Supersedes` relations** — when `MENTISDB_DEDUP_THRESHOLD` is set, appending a thought whose content is sufficiently similar (Jaccard ≥ threshold) to a recent thought automatically creates a `Supersedes` relation instead of storing a duplicate. The superseded thought's id is recorded for fast exclusion.
- **`invalidated_thought_ids` for O(1) superseded detection** — each thought tracks which earlier thoughts it supersedes. Ranked search uses this set to skip superseded thoughts in constant time without walking the full relation graph.

### Search Scoring (0.8.7)

Starting in 0.8.7, ranked search adds:

- **Entity type filtering** — thoughts can carry an optional `entity_type` label (e.g. "bug_report", "architecture_decision", "retrospective"). The `ThoughtQuery` accepts `entity_type` as a filter, and the dashboard exposes an entity_type text filter in the explorer. Entity types are auto-observed per chain and persisted in a `chain_key-entity-types.json` sidecar.

### Search Scoring (0.8.6)

Starting in 0.8.6, ranked search adds three features:

- **Irregular verb lemma expansion** — the query tokenizer maps irregular past-tense verbs to their base form (e.g. "went" → "go", "gave" → "give", "ran" → "run"). About 170 mappings. Indexed content is not modified — expansion is query-time only.
- **Reciprocal Rank Fusion (RRF)** — an opt-in reranking pass. When `enable_reranking=true` is set on a ranked search query, the engine produces separate lexical-only, vector-only, and graph-only rankings over the top `rerank_k` candidates (default 50), merges them via `1/(k + rank)` with `k=60`, and replaces the additive total with the RRF total plus non-rankable signal adjustments. Use when lexical and vector signals disagree on top candidates.
- **Per-field BM25 DF cutoffs** — `Bm25DfCutoffs` on `LexicalQuery` allows configuring separate document-frequency cutoff ratios per field (content, tags, concepts, agent_id, agent_registry). Terms whose global DF exceeds the cutoff for a given field are skipped for that field only. Default cutoffs match the original behavior (content=0.30, tags=0.30, concepts=0.30, agent_id=0.70, agent_registry=0.60).

### Vector Sidecars

MentisDB now exposes an additive Phase 3 vector sidecar surface for direct crate use:

- `search::EmbeddingProvider`
- `search::EmbeddingMetadata`
- `search::VectorSidecar`
- `VectorSearchQuery`
- `MentisDb::vector_sidecar_path(&EmbeddingMetadata)`
- `MentisDb::load_vector_sidecar(&EmbeddingMetadata)`
- `MentisDb::vector_sidecar_freshness(&VectorSidecar, &EmbeddingMetadata)`
- `MentisDb::rebuild_vector_sidecar(&provider)`
- `MentisDb::manage_vector_sidecar(provider)`
- `MentisDb::unmanage_vector_sidecar(&EmbeddingMetadata)`
- `MentisDb::managed_vector_sidecars()`
- `MentisDb::apply_persisted_managed_vector_sidecars()`
- `MentisDb::managed_vector_sidecar_statuses()`
- `MentisDb::set_managed_vector_sidecar_enabled(kind, enabled)`
- `MentisDb::sync_managed_vector_sidecar_now(kind)`
- `MentisDb::rebuild_managed_vector_sidecar_from_scratch(kind)`
- `MentisDb::query_vector(&provider, &VectorSearchQuery)`

Contract:

- embeddings remain optional, and MentisDB still works with no vector dependencies at all
- vector state lives in a rebuildable sidecar, never in the canonical append-only chain
- vector sidecars are separated by `chain_key`, `thought_id`, `thought_hash`, `model_id`, embedding dimension, and embedding version
- changing the embedding model or version invalidates old vector state instead of silently mixing incompatible embeddings
- callers can opt one embedding space into append-time synchronization on a live handle by registering a managed vector sidecar provider
- vector hits surface whether they came from a `Fresh` or stale sidecar
- deleting or corrupting the sidecar degrades only vector retrieval; plain chain reads, appends, and lexical/graph search still work

Operational flow:

- rebuild a sidecar explicitly for one provider and chain
- or register that provider as a managed vector sidecar and keep it fresh on future appends for that open handle
- load or query that sidecar later with the same embedding metadata
- if the chain head changes, the sidecar becomes stale and results report that freshness state until the sidecar is rebuilt

### `mentisdbd` Default Vector Sidecar

`mentisdbd` now applies a persisted managed-vector setting every time it opens a chain.

- by default each chain gets the built-in FastEmbed MiniLM embedding provider (`fastembed-minilm`), which runs locally via ONNX with no cloud dependencies. The legacy `local-text-v1` provider is also available.
- the daemon keeps that sidecar synchronized on append unless the user disables auto-sync for that chain
- ranked search in the daemon and dashboard now uses that managed sidecar transparently, blending lexical, graph, and vector signals whenever it is enabled and available
- the web dashboard exposes per-chain controls to:
  - enable or disable append-time auto-sync
  - sync the sidecar to the latest chain state without changing the enable/disable setting
  - rebuild the sidecar from scratch after an explicit confirmation that the previous file will be deleted and recreated
- if auto-sync is disabled, new thoughts can make the sidecar stale until the user syncs or rebuilds it

### REST Lexical Search

The daemon also exposes the Phase 1 ranked lexical surface over REST at `POST /v1/lexical-search`.

This is the right endpoint when you want lexicographical/lexical text ranking only, with simple score and match provenance fields.

Request shape:

```json
{
  "chain_key": "mentisdb",
  "text": "latency ranking",
  "agent_ids": ["planner"],
  "thought_types": ["Decision"],
  "offset": 0,
  "limit": 10
}
```

Example response:

```json
{
  "total": 1,
  "results": [
    {
      "thought": {
        "index": 42,
        "agent_id": "planner",
        "content": "Latency budget for the Europe rollout."
      },
      "score": 2.91,
      "matched_terms": ["latency", "ranking"],
      "match_sources": ["content", "tags", "agent_registry"]
    }
  ]
}
```

### Phase 4 Transport Contract (Ranked + Bundles)

Phase 4 transport work keeps plain `POST /v1/search` and `POST /v1/lexical-search`
compatibility and adds two additive endpoints:

- `POST /v1/ranked-search` for flat ranked retrieval
- `POST /v1/context-bundles` for seed-anchored grouped support context

Example `POST /v1/ranked-search` request:

```json
{
  "chain_key": "mentisdb",
  "text": "performance budget",
  "thought_types": ["Decision"],
  "limit": 10
}
```

When a managed vector sidecar such as the built-in `fastembed-minilm` provider is active for that chain, the ranked backend becomes `hybrid` or `hybrid_graph` and the response includes a non-zero `score.vector` component for semantic-only or semantic-boosted hits.

Ranked response contract fields:

- `backend`
- `results[].score.{lexical,vector,graph,relation,seed_support,importance,confidence,recency,session_cohesion,rrf,total}`
- `results[].matched_terms`
- `results[].match_sources`
- `results[].graph_distance`
- `results[].graph_seed_paths`
- `results[].graph_relation_kinds`
- `results[].graph_path`
- `results[].chain_key` (present when searching branch chains with ancestor results)

Context-bundle response contract fields:

- `total_bundles`
- `consumed_hits`
- `bundles[].seed.{locator,lexical_score,matched_terms,thought}`
- `bundles[].support[].{locator,thought,depth,seed_path_count,relation_kinds,path}`

MCP transport mirrors this split with additive tools:

- `mentisdb_ranked_search`
- `mentisdb_context_bundles`

Acceptance coverage for these transport contracts lives in:

- `tests/search_transport_contract_tests.rs`

Response shape:

```json
{
  "total": 2,
  "results": [
    {
      "thought": { "index": 42, "agent_id": "planner", "content": "..." },
      "score": {
        "lexical": 2.91,
        "vector": 0.27,
        "graph": 0.18,
        "relation": 0.05,
        "seed_support": 0.0,
        "importance": 0.0,
        "confidence": 0.0,
        "recency": 0.0,
        "total": 3.14
      },
      "matched_terms": ["latency", "ranking"],
      "match_sources": ["content", "tags", "agent_registry"]
    }
  ]
}
```

---

## Web Dashboard

The daemon includes an embedded browser UI at:

```text
https://127.0.0.1:9475/dashboard
```

The dashboard is served over HTTPS with the same self-signed certificate used by
the HTTPS MCP and REST surfaces.

Dashboard capabilities:

- live chain listing with thought and agent counts
- thought exploration with grouped ThoughtType filters, refs, and typed relations
- chain-scoped ranked search with text and live-agent filters
- grouped context bundles for seed-anchored supporting search context
- ranked result inspection in the thought modal, including score breakdowns, matched terms, graph distance, relation kinds, and bundle support preview
- per-chain vector sidecar inspection plus enable/disable, sync, and rebuild controls
- agent detail management for display name, description, owner, status, and signing keys
- latest agent-thought browsing without restarting the daemon after new thoughts are appended
- chain import from `MEMORY.md`
- cross-chain agent-memory copy with agent metadata preserved on the target chain
- skill browsing, diffing, deprecation, and revocation

Protect the dashboard with `MENTISDB_DASHBOARD_PIN` whenever the daemon is reachable
outside localhost.

---

## Backup and Restore

MentisDB includes built-in backup and restore commands in the `mentisdbd` CLI.

### Create a backup

```bash
# Default: platform MENTISDB_DIR → ./mentisdb-YYYY-MM-DD-HH-MM-SS.mbak
mentisdbd backup

# Custom output path
mentisdbd backup --output /backups/mentisdb-2026-04-14.mbak

# Include TLS certificates
mentisdbd backup --include-tls

# Full example
mentisdbd backup --dir ~/.cloudllm/mentisdb --output /backups/mentisdb-2026-04-14.mbak --include-tls
```

When the daemon is running, `backup` calls `POST /v1/admin/flush` before reading files to ensure a consistent snapshot even with `MENTISDB_AUTO_FLUSH=false`. If the daemon is not running, files are captured as-is.

### Restore a backup

```bash
# Idempotent restore (preserves existing files)
mentisdbd restore /backups/mentisdb-2026-04-14.mbak

# Restore to a specific directory
mentisdbd restore /backups/mentisdb-2026-04-14.mbak --dir ~/.cloudllm/mentisdb

# Force overwrite of all files
mentisdbd restore /backups/mentisdb-2026-04-14.mbak --overwrite

# Skip interactive confirmation prompts
mentisdbd restore /backups/mentisdb-2026-04-14.mbak --yes
```

Restore is **idempotent by default** — existing files are preserved. If files in the archive already exist in the target directory and `--overwrite` is not passed, an interactive prompt asks for confirmation. Pass `--yes` to skip all prompts.

### Archive format

A backup is a `.mbak` ZIP archive containing:
- `mentisdb.manifest.json` — SHA-256 checksums and metadata
- `*.tcbin` — binary chain storage
- `*.agents.json` — per-chain agent registries
- `*.entity-types.json` — per-chain entity type definitions
- `*.vectors.*.json` — vector search sidecars
- `mentisdb-registry.json` — global chain registry
- `mentisdb-skills.bin` — skill registry (if present)
- `mentisdb-webhooks.json` — webhook registrations (if present)
- `tls/` — TLS certificates and keys (opt-in via `--include-tls`)

Every file is verified by SHA-256 before restore writes anything to disk. A corrupted archive aborts with an error and leaves the target directory untouched.

### REST flush endpoint

The flush operation is also available as a standalone REST endpoint:

```bash
POST http://127.0.0.1:9472/v1/admin/flush
```

Returns `{"status": "flushed"}` on success. Iterates all open chains and calls `flush()` on each adapter, blocking until all writes are durable.

---

## MCP Tool Catalog

The daemon currently exposes 37 MCP tools:

- `mentisdb_bootstrap`
  Create a chain if needed and write one bootstrap checkpoint when it is empty.
- `mentisdb_append`
  Append a durable semantic thought with optional tags, concepts, refs, scope, and signature metadata.
- `mentisdb_append_retrospective`
  Append a retrospective memory intended to prevent future agents from repeating a hard failure.
- `mentisdb_search`
  Search thoughts by semantic filters, identity filters, time bounds, and scoring thresholds.
- `mentisdb_lexical_search`
  Return flat ranked lexical matches with explainable term and field provenance.
- `mentisdb_ranked_search`
  Return flat ranked lexical, graph-aware, or heuristic results with additive score breakdowns. Supports `as_of` for point-in-time queries and `scope` for memory scope filtering.
- `mentisdb_context_bundles`
  Return seed-anchored grouped support context beneath the best lexical seeds.
- `mentisdb_list_chains`
  List known chains with version, storage adapter, counts, and storage location.
- `mentisdb_merge_chains`
  Merge all thoughts from a source chain into a target chain, then permanently delete the source.
- `mentisdb_branch_from`
  Create a new chain that diverges from a thought on an existing chain. The branch receives a genesis thought with a BranchesFrom relation pointing back to the fork point. Searches on the branch chain transparently include ancestor chain results.
- `mentisdb_list_agents`
  List the distinct agent identities participating in one chain.
- `mentisdb_get_agent`
  Return one full agent registry record, including status, aliases, description, keys, and per-chain activity metadata.
- `mentisdb_list_agent_registry`
  Return the full per-chain agent registry.
- `mentisdb_upsert_agent`
  Create or update a registry record before or after an agent writes thoughts.
- `mentisdb_set_agent_description`
  Set or clear the description stored for one registered agent.
- `mentisdb_add_agent_alias`
  Add a historical or alternate alias to a registered agent.
- `mentisdb_add_agent_key`
  Add or replace one public verification key on a registered agent.
- `mentisdb_revoke_agent_key`
  Revoke one previously registered public key.
- `mentisdb_disable_agent`
  Disable one agent by marking its registry status as revoked.
- `mentisdb_recent_context`
  Render recent thoughts into a prompt snippet for session resumption.
- `mentisdb_memory_markdown`
  Export a `MEMORY.md`-style Markdown view of the full chain or a filtered subset.
- `mentisdb_import_memory_markdown`
  Import a `MEMORY.md`-formatted Markdown document into a target chain.
- `mentisdb_get_thought`
  Return one stored thought by stable id, chain index, or content hash.
- `mentisdb_get_genesis_thought`
  Return the first thought ever recorded in the chain, if any.
- `mentisdb_traverse_thoughts`
  Traverse the chain forward or backward in append order from a chosen anchor, in chunks, with optional filters.
- `mentisdb_skill_md`
  Return the official embedded `MENTISDB_SKILL.md` Markdown file.
- `mentisdb_list_skills`
  List versioned skill summaries from the skill registry.
- `mentisdb_skill_manifest`
  Return the versioned skill-registry manifest, including searchable fields and supported formats.
- `mentisdb_upload_skill`
  Upload a new immutable skill version from Markdown or JSON.
- `mentisdb_search_skill`
  Search skills by indexed metadata such as ids, names, tags, triggers, uploader identity, status, format, schema version, and time window.
- `mentisdb_read_skill`
  Read one stored skill as Markdown or JSON. Responses include trust warnings for untrusted or malicious skill content.
- `mentisdb_skill_versions`
  List immutable uploaded versions for one skill.
- `mentisdb_deprecate_skill`
  Mark a skill as deprecated while preserving all prior versions.
- `mentisdb_revoke_skill`
  Mark a skill as revoked while preserving audit history.
- `mentisdb_head`
  Return head metadata, the latest thought at the current chain tip, and integrity state.
- `mentisdb_register_webhook`
  Register a webhook to receive HTTP POST notifications when thoughts are appended. Delivery is fire-and-forget with exponential backoff retries (up to 5 attempts).
- `mentisdb_list_webhooks`
  List all registered webhooks.
- `mentisdb_delete_webhook`
  Remove a webhook registration by its UUID.

The detailed request and response shapes for the MCP surface live in
[`MENTISDB_MCP.md`](../MENTISDB_MCP.md). The REST equivalents live in
[`MENTISDB_REST.md`](../MENTISDB_REST.md).

---

## Thought Lookup And Traversal

MentisDB distinguishes three different read patterns:

- `head` means the newest thought at the current tip of the append-only chain
- `genesis` means the very first thought in the chain
- traversal means sequential browsing by append order, forward or backward, in chunks

That traversal model is deliberately different from graph/context traversal through `refs` and typed relations. Graph traversal answers "what is connected to this thought?" Sequential traversal answers "what came before or after this thought in the ledger?"

Lookup and traversal support:

- direct thought lookup by `id`, `hash`, or `index`
- logical `genesis` and `head` anchors
- `forward` and `backward` traversal directions
- `include_anchor` control for inclusive vs exclusive paging
- chunked pagination, including `chunk_size = 1` for next/previous behavior
- optional filters reused from thought search, such as agent identity, thought type, role, tags, concepts, text, importance, confidence, and time windows
- numeric time windows expressed as `start + delta` with `seconds` or `milliseconds` units for MCP/REST callers

---

## Skill Registry

MentisDB includes a versioned skill registry stored alongside chain data in a binary file. Skills are ingested through adapters:

- Markdown -> `SkillDocument`
- JSON -> `SkillDocument`
- `SkillDocument` -> Markdown
- `SkillDocument` -> JSON

Each uploaded skill version records:

- registry file version
- skill schema version
- upload timestamp
- responsible `agent_id`
- optional agent display name and owner from the MentisDB agent registry
- source format
- integrity hash

Uploaders must already exist in the agent registry for the referenced chain. Reusing an existing `skill_id` creates a new immutable version; it does not overwrite history.

`read_skill` responses include explicit safety warnings because `SKILL.md` content can be malicious. Treat every skill as advisory until provenance, trust, and requested capabilities are validated.

### Skill Versioning

Each upload to an existing `skill_id` creates a new immutable version rather than overwriting history:

- The first upload stores the full content (`SkillVersionContent::Full`).
- Subsequent uploads store a unified diff patch against the previous version
  (`SkillVersionContent::Delta`), keeping storage efficient for iteratively improved skills.
- Each version receives a monotone `version_number` (0-based, assigned in append order).
- Pass a `version_id` to `read_skill` / `mentisdb_read_skill` to retrieve any historical version.
  The system reconstructs it by replaying patches forward from version 0.
- `skill_versions` / `mentisdb_skill_versions` lists all versions with their ids, numbers, and timestamps.

### Signed Skill Uploads

Agents that have registered Ed25519 public keys in the agent registry must sign their uploads.

Required fields when the uploading agent has active keys:

- `signing_key_id` — the `key_id` registered via `POST /v1/agents/keys` or `mentisdb_add_agent_key`
- `skill_signature` — 64-byte Ed25519 signature over the raw skill content bytes

Agents without registered public keys may upload without signatures.

Upload flow for signing agents:

1. Register a public key:
   ```bash
   POST /v1/agents/keys   { agent_id, key_id, algorithm: "ed25519", public_key_bytes }
   ```
   or via MCP: `mentisdb_add_agent_key`
2. Sign the raw content bytes with the corresponding private key (Ed25519).
3. Include `signing_key_id` and `skill_signature` in the upload request:
   ```bash
   POST /v1/skills/upload   { agent_id, skill_id, content, signing_key_id, skill_signature }
   ```
   or via MCP: `mentisdb_upload_skill` with the same fields.

---

## Setup Scenarios

### Claude Desktop

1. Run `mentisdbd setup claude-desktop` or install prerequisites manually:
   - Node.js >= 20
   - `npm install -g mcp-remote`
2. Edit `claude_desktop_config.json` (macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`)
3. Add the `mentisdb` MCP server entry using `node` as command with `mcp-remote` script path as args
4. Set `NODE_TLS_REJECT_UNAUTHORIZED=0` for self-signed certificate support
5. Restart Claude for Desktop

### Claude Code

1. Start the server: `mentisdbd run`
2. Add the MCP server:
   ```bash
   claude mcp add --transport http mentisdb http://127.0.0.1:9471
   ```
3. Or run `mentisdbd setup claude-code` to auto-configure

### OpenCode

1. Start the server: `mentisdbd run`
2. Add the MCP server:
   ```bash
   opencode mcp add mentisdb http://127.0.0.1:9471
   ```

### Codex

```bash
codex mcp add mentisdb --url http://127.0.0.1:9471
```

### Qwen Code

```bash
qwen mcp add --transport http mentisdb http://127.0.0.1:9471
```

---

## Using With MCP Clients

`mentisdbd` exposes both:

- a standard streamable HTTP MCP endpoint at `POST /`
- the legacy CloudLLM-compatible MCP endpoints at `POST /tools/list` and
  `POST /tools/execute`

That means you can:

- use native MCP clients such as Codex and Claude Code against `http://127.0.0.1:9471`
- keep using direct HTTP calls or `cloudllm`'s MCP compatibility layer when needed

### Codex

Codex CLI expects a streamable HTTP MCP server when you use `--url`:

```bash
codex mcp add mentisdb --url http://127.0.0.1:9471
```

Useful follow-up commands:

```bash
codex mcp list
codex mcp get mentisdb
```

This connects Codex to the daemon's standard MCP root endpoint.

### Qwen Code

Qwen Code uses the same HTTP MCP transport model:

```bash
qwen mcp add --transport http mentisdb http://127.0.0.1:9471
```

Useful follow-up commands:

```bash
qwen mcp list
```

For user-scoped configuration:

```bash
qwen mcp add --scope user --transport http mentisdb http://127.0.0.1:9471
```

### Claude for Desktop

Claude for Desktop connects to MCP servers through `claude_desktop_config.json`.
It requires [Node.js >= 20](https://nodejs.org/) and the
[`mcp-remote`](https://www.npmjs.com/package/mcp-remote) npm package as a
bridge between the desktop app and the MentisDB HTTPS endpoint.

The recommended setup path is automatic:

```bash
mentisdbd setup claude-desktop
```

This command:

- checks for Node.js >= 20 and `mcp-remote` on PATH
- installs `mcp-remote` via `npm` if missing
- writes `claude_desktop_config.json` with the correct absolute paths to `node`
  and `mcp-remote` so the desktop app always uses the right Node version
- sets `NODE_TLS_REJECT_UNAUTHORIZED=0` for self-signed certificate support

To set it up manually instead:

**Step 1 — Install prerequisites** (Node.js >= 20 required):

```bash
npm install -g mcp-remote
```

**Step 2 — Edit the config file** (location by OS):

| OS      | Path |
|---------|------|
| macOS   | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux   | `~/.config/Claude/claude_desktop_config.json` |

The config should use the `node` binary as the command and pass the
`mcp-remote` script path as the first argument. This avoids the shebang
resolution issue where `mcp-remote`'s `#!/usr/bin/env node` may pick an older
Node.js version on systems with multiple Node installs (e.g. nvm).

**macOS (with nvm):**

```json
{
  "mcpServers": {
    "mentisdb": {
      "command": "/Users/you/.nvm/versions/node/v22.18.0/bin/node",
      "args": ["/Users/you/.nvm/versions/node/v22.18.0/bin/mcp-remote", "https://my.mentisdb.com:9473"],
      "env": { "NODE_TLS_REJECT_UNAUTHORIZED": "0" }
    }
  }
}
```

**macOS (with Homebrew Node):**

```json
{
  "mcpServers": {
    "mentisdb": {
      "command": "/opt/homebrew/bin/node",
      "args": ["/opt/homebrew/bin/mcp-remote", "https://my.mentisdb.com:9473"],
      "env": { "NODE_TLS_REJECT_UNAUTHORIZED": "0" }
    }
  }
}
```

**Windows:**

```json
{
  "mcpServers": {
    "mentisdb": {
      "command": "node",
      "args": ["mcp-remote", "https://my.mentisdb.com:9473"],
      "env": { "NODE_TLS_REJECT_UNAUTHORIZED": "0" }
    }
  }
}
```

Use `which mcp-remote` and `which node` to confirm the binary paths on your
machine. **Both must point to the same Node.js installation (>= 20).**

**Linux:**

```json
{
  "mcpServers": {
    "mentisdb": {
      "command": "/usr/local/bin/node",
      "args": ["/usr/local/bin/mcp-remote", "https://my.mentisdb.com:9473"],
      "env": { "NODE_TLS_REJECT_UNAUTHORIZED": "0" }
    }
  }
}
```

> **Why `NODE_TLS_REJECT_UNAUTHORIZED: "0"`?**  
> MentisDB ships with a self-signed TLS certificate. Node.js rejects self-signed
> certs by default, which causes `mcp-remote` to disconnect immediately after the
> MCP `initialize` handshake. This env var disables that check for the
> `mcp-remote` process only. As an alternative, trust the certificate at the OS
> level (`sudo security add-trusted-cert` on macOS) and remove the `env` block.
>
> **Why use `node` as the command instead of `mcp-remote` directly?**  
> The `mcp-remote` shell script uses `#!/usr/bin/env node` as its shebang. On
> systems with multiple Node.js versions (e.g. managed by nvm), the shebang may
> resolve to an older Node that doesn't support `mcp-remote`'s dependencies
> (which require Node >= 20). Using the explicit `node` path as the command
> bypasses the shebang entirely and guarantees the correct Node version is used.

Restart Claude for Desktop after saving the config file.

### Claude Code

Claude Code supports MCP servers through its `claude mcp` commands and
project/user MCP config. For a remote HTTP MCP server, the configuration shape
is transport-based:

```bash
claude mcp add --transport http mentisdb http://127.0.0.1:9471
```

Useful follow-up commands:

```bash
claude mcp list
claude mcp get mentisdb
```

`mentisdbd setup claude-code` merges the MCP server entry into
`~/.claude.json` (or `%USERPROFILE%\.claude.json` on Windows), preserving your
existing Claude Code settings. The older `~/.claude/mcp/mentisdb.json` path is
treated as a legacy companion file, not the canonical config target. The
MentisDB HTTP MCP block it writes looks like this:

```json
{
  "mcpServers": {
    "mentisdb": {
      "type": "http",
      "url": "http://127.0.0.1:9471"
    }
  }
}
```

Important:

- `/mcp` inside Claude Code is mainly for managing or authenticating MCP
  servers that are already configured
- the server itself must already be running at the configured URL

### GitHub Copilot CLI

GitHub Copilot CLI can also connect to `mentisdbd` as a remote HTTP MCP
server.

From interactive mode:

1. Run `/mcp add`
2. Set `Server Name` to `mentisdb`
3. Set `Server Type` to `HTTP`
4. Set `URL` to `http://127.0.0.1:9471`
5. Leave headers empty unless you add auth later
6. Save the config

You can also configure it manually in `~/.copilot/mcp-config.json` (or
`$XDG_CONFIG_HOME/copilot/mcp-config.json` when `XDG_CONFIG_HOME` is set):

```json
{
  "mcpServers": {
    "mentisdb": {
      "type": "http",
      "url": "http://127.0.0.1:9471",
      "headers": {},
      "tools": ["*"]
    }
  }
}
```

---

## Python Client

`pymentisdb` is the official Python client for MentisDB.

### Install

```bash
pip install pymentisdb
pip install pymentisdb[langchain]  # with LangChain integration
```

### Basic Example

```python
from pymentisdb import MentisDbClient, ThoughtType

client = MentisDbClient()  # defaults to http://127.0.0.1:9472

# Append a thought
thought = client.append_thought(
    thought_type=ThoughtType.INSIGHT,
    content="Rate limiting is the real bottleneck."
)

# Ranked search
results = client.ranked_search(text="performance optimization")
for hit in results.results:
    print(f"[{hit.score.total:.3f}] {hit.thought.content}")

# Context bundles
bundles = client.context_bundles(text="cache invalidation", limit=5)
```

### LangChain Integration

```python
from pymentisdb import MentisDbMemory

memory = MentisDbMemory(chain_key="my-project", agent_id="assistant")

llm = ChatOpenAI(model="gpt-4")
chain = llm.with_memory(memory)  # LangChain 0.3+
```

See the [full pymentisdb guide](docs/pymentisdb-python-client.html) for complete API reference.

---

## LLM-Extracted Memories

MentisDB can convert raw agent text, conversation logs, or transcripts into structured
`ThoughtInput` records using GPT-4o (or any OpenAI-compatible endpoint). Review the
extracted records and append them to your chain.

This feature is enabled by default. Configure with environment variables:

```bash
export OPENAI_API_KEY="sk-..."           # required
export LLM_BASE_URL="https://api.openai.com/v1"  # defaults to OpenAI
export LLM_MODEL="gpt-4o"                # defaults to gpt-4o
```

```rust,ignore
use mentisdb::{LlmExtractionConfig, extract_memories_from_text};

let config = LlmExtractionConfig::from_env()?;
let result = extract_memories_from_text(raw_text, &config, None).await?;

// Review before appending
for thought in &result.thoughts {
    println!("{:?}", thought);
}
```

The extraction uses `openai-rust2` with connection pooling, 3× retry on 429/5xx,
60s timeout, and JSON object response format. LLM output is **untrusted** — always
review and optionally sign extracted thoughts before appending.

See [docs/llm-extracted-memories-design.md](docs/llm-extracted-memories-design.md) for the full design.

---

## Retrospective Memory

MentisDB supports a dedicated retrospective workflow for lessons learned.

- Use `mentisdb_append` for ordinary durable facts, constraints, decisions,
  plans, and summaries.
- Use `mentisdb_append_retrospective` after a repeated failure, a long snag,
  or a non-obvious fix when future agents should avoid repeating the same
  struggle.

The retrospective helper:

- defaults `thought_type` to `LessonLearned`
- always stores the thought with `role = Retrospective`
- still supports tags, concepts, confidence, importance, and `refs` to earlier
  thoughts such as the original mistake or correction

---

## Thought Types And Roles

MentisDB currently defines 30 semantic `ThoughtType` values and 8 operational
`ThoughtRole` values.

Thought types:

- `PreferenceUpdate`, `UserTrait`, `RelationshipUpdate`
- `Finding`, `Insight`, `FactLearned`, `PatternDetected`, `Hypothesis`, `Surprise`
- `Mistake`, `Correction`, `LessonLearned`, `AssumptionInvalidated`, `Reframe`
- `Constraint`, `Plan`, `Subgoal`, `Goal`, `Decision`, `StrategyShift`
- `Wonder`, `Question`, `Idea`, `Experiment`
- `ActionTaken`, `TaskComplete`
- `Checkpoint`, `StateSnapshot`, `Handoff`, `Summary`

Thought roles:

- `Memory`
- `WorkingMemory`
- `Summary`
- `Compression`
- `Checkpoint`
- `Handoff`
- `Audit`
- `Retrospective`

Use `ThoughtType` to say what the memory means semantically, and `ThoughtRole`
to say how the system should treat it operationally. The crate rustdoc is the
authoritative source for per-variant semantics, and the Agent Guide on the docs
site contains a human-oriented explanation of when to use each one.

### Memory Scopes

MentisDB defines a `MemoryScope` enum that controls how far a thought should propagate:

- `User` (default) — visible to all agents and sessions sharing the chain
- `Session` — visible only within the current agent session
- `Agent` — visible only to the agent that created it

Scopes are stored as tags on the thought:

- `scope:user`
- `scope:session`
- `scope:agent`

Crate API:

- `ThoughtInput::with_scope(MemoryScope)` sets the scope on append
- `RankedSearchQuery::with_scope(MemoryScope)` filters search results by scope

REST API:

- `POST /v1/thoughts` accepts an optional `scope` field (`"user"`, `"session"`, or `"agent"`)
- `POST /v1/ranked-search` and `POST /v1/context-bundles` accept an optional `scope` field

---

## Shared-Chain Multi-Agent Use

Multiple agents can write to the same `chain_key`.

Each stored thought carries a stable:

- `agent_id`

Agent profile metadata now lives in the per-chain agent registry instead of
being duplicated into every thought record. Registry records can store:

- `display_name`
- `agent_owner`
- `description`
- `aliases`
- `status`
- `public_keys`
- per-chain activity counters such as `thought_count`, `first_seen_index`, and `last_seen_index`

That allows a shared chain to represent memory from:

- multiple agents in one workflow
- multiple named roles in one orchestration system
- multiple tenants or owners writing to the same chain namespace

Queries can filter by:

- `agent_id`
- `agent_name`
- `agent_owner`

Administrative tools can also inspect and mutate the agent registry directly,
so agents can be documented, disabled, aliased, or provisioned with public keys
before they start writing thoughts.

---

## Related Docs

At the repository root:

- `MENTISDB_MCP.md`
- `MENTISDB_REST.md`
- `mentisdb/WHITEPAPER.md`
- `mentisdb/changelog.txt`
