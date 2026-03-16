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

Install and run the daemon:

```bash
cargo install mentisdb
mentisdbd
```

Run persistently after closing your SSH session:

```bash
nohup mentisdbd &
```

Connect your AI coding tool to the running daemon:

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
make install        # cargo install --path . --bin mentisdbd
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

The standalone daemon binary is `mentisdbd`.

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

When it starts, it serves both:

- an MCP server
- a REST server

Before serving traffic, it:

- migrates or reconciles discovered chains to the current schema and default storage adapter
- verifies chain integrity and attempts repair from valid local sources when possible
- migrates the skill registry from V1 to V2 format if needed (idempotent; safe to run repeatedly)

Once startup completes, it prints:

- the active chain directory, default chain key, and bound MCP/REST addresses
- a catalog of all exposed HTTP endpoints with one-line descriptions
- a per-chain summary with version, adapter, thought count, and per-agent counts

---

## Daemon Configuration

`mentisdbd` is configured with environment variables:

- `MENTISDB_DIR`
  Directory where MentisDB storage adapters store chain files.
- `MENTISDB_DEFAULT_KEY`
  Default `chain_key` used when requests omit one. Default: `borganism-brain`
- `MENTISDB_DEFAULT_STORAGE_ADAPTER`
  Default storage backend for newly created chains. Supported values: `binary`, `jsonl`.
  Default: `binary`
- `MENTISDB_STORAGE_ADAPTER`
  Optional short alias for `MENTISDB_DEFAULT_STORAGE_ADAPTER`.
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
- `MENTISDB_AUTO_FLUSH`
  Controls per-write durability of the `binary` storage adapter.
  - `true` (default): every `append_thought` flushes to disk immediately. Full durability.
  - `false`: writes are batched and flushed every 16 appends (`FLUSH_THRESHOLD`). Up to 15
    thoughts may be lost on a hard crash or power failure, but write throughput increases
    significantly for multi-agent hubs with many concurrent writers.
  Supported values: `1`, `0`, `true`, `false`. Has no effect on the `jsonl` adapter.

Example — full durability (production default):

```bash
MENTISDB_DIR=/tmp/mentisdb \
MENTISDB_DEFAULT_KEY=borganism-brain \
MENTISDB_DEFAULT_STORAGE_ADAPTER=binary \
MENTISDB_VERBOSE=true \
MENTISDB_LOG_FILE=/tmp/mentisdb/mentisdbd.log \
MENTISDB_BIND_HOST=127.0.0.1 \
MENTISDB_MCP_PORT=9471 \
MENTISDB_REST_PORT=9472 \
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
- `POST /v1/thoughts/genesis`
- `POST /v1/thoughts`
- `POST /v1/thoughts/traverse`
- `POST /v1/retrospectives`
- `POST /v1/search`
- `POST /v1/recent-context`
- `POST /v1/memory-markdown`
- `POST /v1/skills/upload`
- `POST /v1/skills/search`
- `POST /v1/skills/read`
- `POST /v1/skills/versions`
- `POST /v1/skills/deprecate`
- `POST /v1/skills/revoke`
- `POST /v1/head`

---

## MCP Tool Catalog

The daemon currently exposes 29 MCP tools:

- `mentisdb_bootstrap`
  Create a chain if needed and write one bootstrap checkpoint when it is empty.
- `mentisdb_append`
  Append a durable semantic thought with optional tags, concepts, refs, and signature metadata.
- `mentisdb_append_retrospective`
  Append a retrospective memory intended to prevent future agents from repeating a hard failure.
- `mentisdb_search`
  Search thoughts by semantic filters, identity filters, time bounds, and scoring thresholds.
- `mentisdb_list_chains`
  List known chains with version, storage adapter, counts, and storage location.
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

Claude Code also supports JSON config files such as `.mcp.json`. A MentisDB
HTTP MCP config looks like this:

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

You can also configure it manually in `~/.copilot/mcp-config.json`:

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
