# MentisDB 0.9.2.38 — Smart Stdio MCP, Shared Service State, and Reproducible Benchmarks

Released April 17, 2026. Cargo version `0.9.2`, git tag `0.9.2.38`.

This is a correctness release. Two fixes close long-standing integration gaps that were
easy to miss and expensive to debug: stdio MCP clients no longer need a pre-launched
daemon, and the multi-surface server no longer splits brain across HTTP, HTTPS, MCP, REST,
and the dashboard. Benchmarks are now reproducible bit-identical across full-scale runs.

## Highlights

- **Smart stdio MCP mode.** Zero pre-flight for Claude Desktop and other stdio MCP clients.
- **Cross-surface state coherency.** One `MentisDbService` shared across every surface booted by `start_servers`.
- **Reproducible benchmarks.** LoCoMo-10P R@10 = 71.9%, LongMemEval R@5 = 66.8%, R@10 = 72.2%, R@20 = 78.0%. Bit-identical across three full-scale runs.
- **Backup and restore.** `mentisdbd backup` / `mentisdbd restore` with SHA-256 manifest, path-traversal rejection, and interactive conflict prompt.

## Smart Stdio MCP Mode

When an MCP client (Claude Desktop, Cursor, or anything that spawns an stdio subprocess)
starts `mentisdbd` in stdio mode, the process now:

1. Probes the local daemon's health endpoint.
2. If a daemon is already running, proxies stdio requests to the HTTP MCP surface so every client sees the same live chain cache.
3. If not, launches a background daemon (`nohup` on Unix, `start /B` on Windows), waits for health, and proxies to it.
4. If launch fails, falls back to in-process stdio so the client still works.

Claude Desktop users stop fighting MCP bootstrap: one entry in
`claude_desktop_config.json` pointing at `mentisdbd` is the whole setup. Multiple stdio
clients observe each other's appends in real time because they share the daemon.

Background: [Stdio MCP Mode with Smart Daemon Detection](https://mentisdb.com/blog/mentisdb-stdio-mcp-mode.html).

## Cross-Surface State Coherency in `start_servers`

Before 0.9.2.38, `start_servers` constructed each surface — HTTP MCP, HTTP REST, HTTPS
MCP, HTTPS REST, and the dashboard — with its own `MentisDbService::new(...)`. Each
service owned a private `DashMap<chain_key, Arc<RwLock<MentisDb>>>`, so an append via
REST was invisible to MCP (and vice versa) until the daemon restarted and both services
happened to reload the chain from disk.

The fix is small and surgical: `start_servers` now constructs **one** `MentisDbService`
and shares it across every surface it boots. Single-surface entry points
(`start_mcp_server`, `start_rest_server`, `start_https_*_server`) are unchanged.

Regression test `start_servers_shares_state_across_mcp_and_rest` pre-warms the MCP
service, appends via REST, and asserts the MCP side immediately sees the new
`head_hash`, `thought_count`, and `latest_thought.index`.

## Backup and Restore

Two new subcommands, `mentisdbd backup` and `mentisdbd restore`, create and restore
`.mentis` archives of the full `MENTISDB_DIR`.

- SHA-256 manifest for every file; verification before any file is written on restore.
- Flags: `--flush`, `--include-tls`, `--overwrite`.
- CLI auto-detects a running daemon and calls `POST /v1/admin/flush` before reading files.
- Interactive conflict prompt lists conflicting files when `--overwrite` is absent; declining exits cleanly with no changes.
- Restore rejects path-traversal entries (`../escape.txt`, absolute paths) with `InvalidData` before extraction.
- 33 integration tests covering roundtrip, TLS inclusion, idempotent restore, overwrite, corruption detection, manifest round-trip, subdirectory creation.

## `POST /v1/admin/flush`

New REST endpoint iterates every open chain and calls `flush()` on its
`BinaryStorageAdapter`. Backup uses it automatically; operators can call it directly
before snapshotting the data directory with an external tool.

## Search and Relation Correctness

- **Federated search dedup** keeps the higher-scoring duplicate occurrence instead of whichever chain surfaced the UUID first.
- **Relation dedup** no longer collapses edges that differ by `chain_key`, `valid_at`, or `invalid_at`; only exact duplicates are removed.
- **Branch ancestor discovery is transitive** — branch-aware search walks child to parent to grandparent.
- **Webhook registry persistence** uses temp-file-plus-rename; delivery fan-out is bounded by a queue and semaphore so bursty appends no longer spawn unbounded tasks.
- **LoCoMo bench fix**: `--limit` filter now rebuilds `session_map` after filtering, so evidence from filtered-out personas can't leak back into R@K scoring via neighbor lookups (dev runs only; full-scale was unaffected).

## Wizard and Client Fixes

- Wizard prompts before `brew install mcp-remote` on macOS (previously silent). brew is tried first, npm is the fallback. Dead `check_node_version` and `detect_brew_mcp_remote` helpers removed.
- When `mcp-remote` is installed as an absolute executable, the wizard uses it directly without a node wrapper; shebang-based fallback retained for npm scripts.
- Restore's interactive confirmation actually enables overwrite when the user answers yes (previously silently kept `overwrite=false`).
- `pymentisdb.context_bundles()` decodes typed `ContextBundleSeed` and `ContextBundleHit` instead of nesting `ContextBundle` inside itself. `append_thought()` uses one canonical `ThoughtInput` builder. `ranked_search()` and `context_bundles()` accept `MemoryScope` enums directly.
- LLM memory extraction no longer sends an OpenAI `response_format` hint — some OpenAI-compatible endpoints rejected the schema. Prompt plus strict JSON validation now preserves provider portability.

## Startup: Terminal-Close Warning

`mentisdbd` prints a yellow warning at startup that closing the terminal stops the
process, followed by OS-specific background-launch guidance:

- **macOS:** `nohup mentisdbd > ~/.cloudllm/mentisdb/mentisdbd.log 2>&1 &`
- **Linux:** systemd user service unit snippet or a `nohup` one-liner.
- **Windows:** `schtasks /create` one-liner or `Start-Process -WindowStyle Hidden`.

## Benchmarks

Deterministic on 0.9.2.38. Three independent full-scale runs (2026-04-14 and two on
2026-04-17) produced bit-identical scores.

| Benchmark          | Metric | Result    |
|--------------------|--------|-----------|
| LoCoMo 10-persona  | R@10   | **71.9%** |
| LongMemEval        | R@5    | **66.8%** |
| LongMemEval        | R@10   | **72.2%** |
| LongMemEval        | R@20   | **78.0%** |

The retrieval pipeline that produces these numbers is described end-to-end in
[Inside MentisDB Ranked Search — One Call, One Pipeline](https://mentisdb.com/blog/mentisdb-ranked-search-pipeline.html).

## WHITEPAPER Refresh

[`WHITEPAPER.md`](https://github.com/CloudLLM-ai/mentisdb/blob/master/WHITEPAPER.md) was
rewritten in academic register with formal definitions (Thought, Chain, BM25, RRF,
Jaccard dedup, temporal validity), a tamper-evidence proof sketch, and a References
section. A LaTeX port (`WHITEPAPER.tex`) uses `amsmath`/`amsthm`/`booktabs`, and
`build-whitepaper.sh` ships with macOS and Ubuntu install instructions plus
`--open`/`--clean` flags. Benchmark numbers now match the reproducible results; the
previously empty LoCoMo single-hop (75.8%), multi-hop (57.4%), and R@20 (79.1%) cells
are filled in.

## Other Fixes and Chore

- `invalid_input_error()` / `not_found_error()` helpers in the server replace four `Box::new(io::Error::new(...))` callsites (net -8 lines, identical behavior).
- Strengthened server-side signature-verification coverage for skill uploads: explicit checks for missing signature fields, unknown signing-key ids, and tampered signatures.
- README and Rust docs coverage gaps filled for `source_episode`, `entity_type`, `BranchesFrom`, `mentisdb_federated_search`, `mentisdb_extract_memories`, `mentisdb_list_entity_types`, `mentisdb_upsert_entity_type` (MCP catalog count 37 → 42).
- `docs.mentisdb.com`: added Advanced Retrieval, Entity Types & Provenance, Webhook Callbacks, LLM-Extracted Memories, and pymentisdb Python Client sections to `user_docs.rs`; added LLM-based reranking and Operations & Admin sections to `developer_docs.rs`.
- Daemon endpoint catalog and REST rustdocs now include the live router surface (`/v1/federated-search`, `/v1/import-markdown`, entity types, chain merge, webhooks, extract-memories, admin flush).
- `mentisdbd --help` documents `MENTISDB_DASHBOARD_PIN`.

## Upgrade

```bash
cargo install mentisdb --locked --force
```

**Claude Desktop users:** you no longer need to pre-launch `mentisdbd`. Point your MCP
config at the `mentisdbd` binary in stdio mode and the process will find or start the
daemon itself.

No schema migration. Webhook registrations, backups, and existing chains from 0.9.1.x
carry forward unchanged.

## Links

- [Inside MentisDB Ranked Search — One Call, One Pipeline](https://mentisdb.com/blog/mentisdb-ranked-search-pipeline.html)
- [Stdio MCP Mode with Smart Daemon Detection](https://mentisdb.com/blog/mentisdb-stdio-mcp-mode.html)
- [WHITEPAPER.md](https://github.com/CloudLLM-ai/mentisdb/blob/master/WHITEPAPER.md)
- [GitHub Releases](https://github.com/CloudLLM-ai/mentisdb/releases)
