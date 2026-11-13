# MentisDB 0.12.2.59 (Steady MCP stream, safe appends, source episodes)

**Date:** November 12, 2026

A maintenance release. Its headline fix is for anyone connecting an MCP client over HTTP,
Claude Code included: the MCP event stream no longer goes silent, so clients stop dropping
and reconnecting to it. It also closes a way one oversized thought could make a whole chain
unreadable, makes `source_episode` actually stored and searchable, and brings the
dashboard's Settings screen and Markdown skill import up to what the daemon supports.

There is no data migration and no library break.

## Highlights

- **The MCP event stream stays alive** ([cloudllm#57](https://github.com/CloudLLM-ai/cloudllm/issues/57)).
  `GET /` on the MCP port wrote no bytes until a tool call was broadcast, so clients
  with an idle-body timeout dropped it and reconnected over and over. Claude Code's HTTP
  client logged hundreds of `SSE stream disconnected: TimeoutError` lines per session, and a
  reconnect that met a brief network stall could leave MentisDB disconnected. MentisDB now
  builds on `cloudllm_mcp` 0.4.1, which sends an SSE comment (`: keepalive`) after 15 s
  with nothing else to send. Clients ignore comment lines; a busy stream carries none.
- **One oversized thought can no longer make a chain unreadable.** The 10 MiB cap on a
  thought record was enforced only when a chain was opened, so an append over it was
  written, and the next open failed. Ordinary REST and MCP bodies are capped at 2 MiB, so
  the way in was the library and the large-body import routes (for a token holder). The
  append now fails with `InvalidInput` (400) before anything is written.
- **`source_episode` is stored, returned and filterable** (#27). `POST /v1/thoughts` and
  `mentisdb_append` accepted the field and silently dropped it. It is now stored, and is an
  exact-match filter on search, ranked, federated and context-bundle search over REST and
  MCP (thanks to cgcardona). The CLI has the same: `--source-episode` on `thought append` and the search
  commands, and commands whose route cannot apply it refuse the flag instead of sending a
  field the server would drop.
- **The Settings screen lists every daemon setting.** 18 variables were missing, among them
  `MENTISDB_DEDUP_THRESHOLD` / `_SCAN_WINDOW`, `MENTISDB_AUTO_EDGE_*`, the request-body
  limits, `MENTISDB_FSYNC` and the search defaults. The HTTPS ports were shown but refused
  on save, and five startup-only settings were labelled hot-reload. One catalog now drives
  the screen, and a test fails when the daemon reads a `MENTISDB_*` variable it does not
  list.
- **Markdown skill import reads standard YAML frontmatter** (#26). Multi-line `description:
  >` / `|` values and `- item` block lists for `triggers`, `tags` and `warnings` were
  dropped, so skills written for agent harnesses uploaded with no description and nothing
  to search on (thanks to Austin Faulkner-Pearce).

## Upgrading

```bash
cargo install mentisdb --locked --force
```

Or download a release binary, or `docker pull ghcr.io/cloudllm-ai/mentisdb:0.12.2.59`.
There is no data migration, and the library API only gained items.

- **Restart the daemon** to get the MCP keepalive. Nothing to configure.
- **Library embedders on `cloudllm_mcp`:** the keepalive is on by default.
  `StreamableHttpConfig::with_sse_keepalive(None)` turns it off.
- **Appends over 10 MiB** are now refused with `InvalidInput` instead of being written.
- **Startup-only settings** (`MENTISDB_VERBOSE`, `MENTISDB_UPDATE_CHECK`,
  `MENTISDB_UPDATE_REPO`, `MENTISDB_STARTUP_SOUND`, `MENTISDB_THOUGHT_SOUNDS`) now say
  "restart required" on the Settings screen, as they always behaved.

### Remaining limitations

- `/v1/lexical-search`, thought traversal, summary candidates and the MEMORY.md export do
  not take the `source_episode` filter yet.
- Relations' `valid_at` / `invalid_at` on append are accepted but not yet applied.

## Benchmarks

Full LoCoMo-10P and LongMemEval protocol runs on a fresh daemon and fresh chains, built from the tag. Ranking is
unchanged: 0.12.1.58, run against the same ingested chains, returned the same top-5 lists for all 500
LongMemEval questions and the same LoCoMo results. LongMemEval sits one to three questions under the stored
0.12.1.58 numbers because the harness ingests concurrently; two fresh ingests on 0.12.2.59 alone differ by
one question. Details: [`results/benchmarks/2026-11-13-0.12.2.59`](https://github.com/CloudLLM-ai/mentisdb/tree/master/results/benchmarks/2026-11-13-0.12.2.59).

| Benchmark | Metric | 0.12.1.58 | 0.12.2.59 |
|---|---|---|---|
| LoCoMo-10P | default R@10 | 76.7 | 76.7 |
| LoCoMo-10P | rerank R@10 | 76.9 | 76.9 |
| LoCoMo-10P | default R@5 | 67.9 | 67.9 |
| LongMemEval | default R@5 | 70.0 | 69.6-69.8 |
| LongMemEval | rerank R@5 | 70.6 | 70.0-70.2 |

Full changelog: [`changelog.txt`](https://github.com/CloudLLM-ai/mentisdb/blob/0.12.2.59/changelog.txt)
