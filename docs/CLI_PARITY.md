# CLI parity map

The `mentisdb` binary is a complete command-line interface to everything the
MCP tools, the REST API and the web dashboard can do. This file is the
checklist: every MCP tool, REST route and dashboard action, and the CLI command
that covers it. When a tool, route or dashboard action is added, add its row
here and its command to `src/cli/catalog.rs` in the same change. The test
`tests/cli_remote_parse_tests.rs` fails if a catalogued command is missing from this
document.

## How the CLI reaches MentisDB

| Kind of command | Transport | Why |
|---|---|---|
| Memory and registry operations (`thought`, `search`, `chain`, `agent`, `entity-type`, `memory`, `skill`, `webhook`, `daemon health/flush`) | The running daemon's REST API (`--url`, default `http://127.0.0.1:9472`, bearer token from `--token` or `MENTISDB_BEARER_TOKEN`) | The daemon owns the chain cache and the file locks. A second process appending to the same chain files would race it. Going through REST also means the CLI uses the exact handlers (`MentisDbService::*`) that MCP and REST share, so there is no second implementation to drift. |
| Dashboard-only operations (`chain info/delete`, `vectors enable/disable/sync`, `skill diff`, `agent copy`, `daemon version/restart/settings`) | The dashboard API (`--dashboard-url`, default `https://127.0.0.1:9475`, PIN from `--pin` or `MENTISDB_DASHBOARD_PIN`) | These actions only exist behind the PIN-gated dashboard on purpose (a bearer token must not be able to delete a chain). The CLI sends the PIN as `Authorization: Bearer` and the documented `X-MentisDB-Dashboard: 1` header. The dashboard certificate is self-signed, so verification is skipped for loopback hosts and requires `--insecure` for anything else. |
| Local administration (`bearertoken`, `backup`, `restore`, `cert`, `setup`, `wizard`) | The data directory or config files directly (`--dir`, `MENTISDB_DIR`) | Unchanged: these work with the daemon stopped, or manage files the daemon does not own. |

Every remote command accepts `--json` (compact, one line, for pipes) and
otherwise prints indented JSON; commands whose result is a document (memory
Markdown, skill content, the recent-context prompt) print the raw text, and
`--json` prints the full response envelope instead.

## MCP tools

| MCP tool | REST route | CLI command |
|---|---|---|
| `mentisdb_bootstrap` | `POST /v1/bootstrap` | `chain bootstrap` |
| `mentisdb_append` | `POST /v1/thoughts` | `thought append` (and `add`) |
| `mentisdb_append_retrospective` | `POST /v1/retrospectives` | `thought retro` |
| `mentisdb_search` | `POST /v1/search` | `search filter` |
| `mentisdb_lexical_search` | `POST /v1/lexical-search` | `search lexical` |
| `mentisdb_ranked_search` | `POST /v1/ranked-search` | `search` (default mode) |
| `mentisdb_federated_search` | `POST /v1/federated-search` | `search federated` |
| `mentisdb_context_bundles` | `POST /v1/context-bundles` | `search bundles` |
| `mentisdb_summary_candidates` | `POST /v1/summary-candidates` | `thought candidates` |
| `mentisdb_list_chains` | `GET /v1/chains` | `chain list` |
| `mentisdb_list_agents` | `POST /v1/agents` | `agent list` (and `agents`) |
| `mentisdb_get_agent` | `POST /v1/agent` | `agent get` |
| `mentisdb_list_agent_registry` | `POST /v1/agent-registry` | `agent registry` |
| `mentisdb_upsert_agent` | `POST /v1/agents/upsert` | `agent upsert` |
| `mentisdb_set_agent_description` | `POST /v1/agents/description` | `agent describe` |
| `mentisdb_add_agent_alias` | `POST /v1/agents/aliases` | `agent alias` |
| `mentisdb_add_agent_key` | `POST /v1/agents/keys` | `agent key add` |
| `mentisdb_revoke_agent_key` | `POST /v1/agents/keys/revoke` | `agent key revoke` |
| `mentisdb_disable_agent` | `POST /v1/agents/disable` | `agent disable` |
| `mentisdb_list_entity_types` | `POST /v1/entity-types` | `entity-type list` |
| `mentisdb_upsert_entity_type` | `POST /v1/entity-types/upsert` | `entity-type upsert` |
| `mentisdb_recent_context` | `POST /v1/recent-context` | `thought recent` |
| `mentisdb_memory_markdown` | `POST /v1/memory-markdown` | `memory export` |
| `mentisdb_import_memory_markdown` | `POST /v1/import-markdown` | `memory import` |
| `mentisdb_get_thought` | `POST /v1/thought` | `thought get` |
| `mentisdb_get_genesis_thought` | `POST /v1/thoughts/genesis` | `thought genesis` |
| `mentisdb_traverse_thoughts` | `POST /v1/thoughts/traverse` | `thought traverse` |
| `mentisdb_skill_md` | `GET /mentisdb_skill_md` | `skill guide` |
| `mentisdb_list_skills` | `GET /v1/skills` | `skill list` |
| `mentisdb_skill_manifest` | `GET /v1/skills/manifest` | `skill manifest` |
| `mentisdb_upload_skill` | `POST /v1/skills/upload` | `skill upload` |
| `mentisdb_search_skill` | `POST /v1/skills/search` | `skill search` |
| `mentisdb_read_skill` | `POST /v1/skills/read` | `skill read` |
| `mentisdb_skill_versions` | `POST /v1/skills/versions` | `skill versions` |
| `mentisdb_deprecate_skill` | `POST /v1/skills/deprecate` | `skill deprecate` |
| `mentisdb_revoke_skill` | `POST /v1/skills/revoke` | `skill revoke` |
| `mentisdb_delete_skill` | `POST /v1/skills/delete` | `skill delete` |
| `mentisdb_head` | `POST /v1/head` | `chain head` |
| `mentisdb_merge_chains` | `POST /v1/chains/merge` | `chain merge` |
| `mentisdb_branch_from` | `POST /v1/chains/branch` | `chain branch` |
| `mentisdb_list_webhooks` | `GET /v1/webhooks` | `webhook list` |
| `mentisdb_register_webhook` | `POST /v1/webhooks` | `webhook register` |
| `mentisdb_delete_webhook` | `DELETE /v1/webhooks/{id}` | `webhook delete` |
| `mentisdb_extract_memories` | `POST /v1/extract-memories` | `memory extract` |

## REST routes with no MCP tool

| REST route | CLI command |
|---|---|
| `GET /health` | `daemon health` |
| `POST /v1/vectors/rebuild` | `vectors rebuild` |
| `POST /v1/admin/flush` | `daemon flush` |

## Dashboard actions (`/dashboard/api/...`)

| Dashboard action | CLI command |
|---|---|
| List chains with sizes and sidecar status (`GET /chains`) | `chain list` (REST list) and `chain info <chain>` (dashboard detail) |
| Chain detail (`GET /chains/{k}`) | `chain info` |
| Create chain (`POST /chains`) | `chain bootstrap` |
| Delete chain (`DELETE /chains/{k}`) | `chain delete` |
| Enable / disable / sync a vector sidecar | `vectors enable`, `vectors disable`, `vectors sync` |
| Rebuild a vector sidecar | `vectors rebuild` |
| Paged thoughts of a chain, one thought, thoughts of an agent | `thought traverse`, `thought get`, `search filter --agent` |
| Chain search, bundles, agent search | `search`, `search bundles`, `search filter` |
| List agents (all chains / one chain) | `agent list` |
| Agent detail | `agent get` |
| Create or patch an agent | `agent upsert` |
| Revoke / activate an agent | `agent disable`, `agent activate` |
| Add / delete an agent key | `agent key add`, `agent key revoke` |
| Agent memory Markdown export | `memory export --agent` |
| Import MEMORY.md | `memory import` |
| Copy an agent's memories to another chain | `agent copy` |
| Merge / branch chains | `chain merge`, `chain branch` |
| List / read skills, versions | `skill list`, `skill read`, `skill versions` |
| Skill diff | `skill diff` |
| Upload / revoke / deprecate / delete a skill | `skill upload`, `skill revoke`, `skill deprecate`, `skill delete` |
| Version | `daemon version` |
| Settings read / update | `daemon settings list`, `daemon settings set` |
| Restart the daemon | `daemon restart` |
| Bearer tokens list / create / revoke / delete | `bearertoken list/create/revoke/remove` (local, unchanged) |

## CLI-only commands

`setup`, `wizard` (agent integrations), `backup`, `restore`, `cert`,
`bearertoken`, `add` (quick thought append), `agents` (alias of `agent list`).

## Reserved for the access-policy track

The command grammar is `<group> <subcommand...>` with the subcommand path
resolved by longest match, so `chain access show`, `chain access set` and
`chain access key add|revoke` slot into the `chain` group without touching the
parser.
