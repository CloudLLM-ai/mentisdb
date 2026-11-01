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
| Local administration (`bearertoken`, `backup`, `restore`, `cert`, `setup`, `wizard`, `migrate`) | The data directory or config files directly (`--dir`, `MENTISDB_DIR`) | Unchanged: these work with the daemon stopped, or manage files the daemon does not own. |

Every remote command accepts `--json` (compact, one line, for pipes) and
otherwise prints indented JSON; commands whose result is a document (memory
Markdown, skill content, the recent-context prompt) print the raw text, and
`--json` prints the full response envelope instead.

## Ranking knobs

`search`, `search federated` and `search bundles` (REST `ranked-search`,
`federated-search`, `context-bundles`; MCP `mentisdb_ranked_search`,
`mentisdb_federated_search`, `mentisdb_context_bundles`) accept the same ranking
knobs. Each CLI flag is the REST/MCP request field of the same name with `-`
for `_`; omitted knobs keep the library default.

| REST / MCP field | CLI flag | Library | Default |
|---|---|---|---|
| `resolve_successors` | `--resolve-successors BOOL` | `RankedSearchQuery::with_resolve_successors` | `true` |
| `collapse_duplicates` | `--collapse-duplicates THRESHOLD` (0 disables) | `with_collapse_duplicates` | `0.9` |
| `vector_fusion` | `--vector-fusion relative\|absolute` | `with_vector_fusion` | `relative` |
| `synonym_weight` | `--synonym-weight WEIGHT` (0 disables) | `with_synonym_weight`, `with_thesaurus` | `0.3` |
| `df_scope` | `--df-scope global\|field` | `with_df_scope` | `global` |
| `df_cutoffs.{content,tags,concepts,agent_id,agent_registry}` | `--df-cutoff-content`, `-tags`, `-concepts`, `-agent-id`, `-agent-registry` | `with_df_cutoffs` | 0.30 / 0.30 / 0.30 / 0.70 / 0.60 |

Unknown `df_scope` or `vector_fusion` names and cutoffs outside `0..=1` are
rejected with an error instead of silently falling back.

## Compact results

`search` and `search federated` (REST `ranked-search` and `federated-search`; MCP
`mentisdb_ranked_search` and `mentisdb_federated_search`) accept `compact`. The
field is not part of `search bundles` (`context-bundles`), whose request
rejects it.

| REST / MCP field | CLI flag | Library | Default |
|---|---|---|---|
| `compact` | `--compact` (switch) | none: a response shape of the server, not a query option | `false` |

A compact hit is `chain_key`, the total `score` and the thought without `hash`,
`prev_hash`, `thought_signature`, `signing_key_id`, `schema_version`,
`agent_name`, `agent_owner` and top-level empty values. The `provenance` object,
the version fields and graph provenance are kept. See
[Compact search results](../README.md#compact-search-results).

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
| `mentisdb_migration_status` | `GET /v1/migrations` (and `POST /v1/migrations/retry`, `/skip`) | `migrate status` (offline, or `--url` for a running daemon), `migrate retry`, `migrate skip` |
| `mentisdb_list_agents` | `POST /v1/agents` | `agent list` (and `agents`) |
| `mentisdb_get_agent` | `POST /v1/agent` | `agent get` |
| `mentisdb_list_agent_registry` | `POST /v1/agent-registry` | `agent registry` |
| `mentisdb_upsert_agent` | `POST /v1/agents/upsert` | `agent upsert` |
| `mentisdb_set_agent_description` | `POST /v1/agents/description` | `agent describe` |
| `mentisdb_add_agent_alias` | `POST /v1/agents/aliases` | `agent alias` |
| `mentisdb_list_entity_types` | `POST /v1/entity-types` | `entity-type list` |
| `mentisdb_upsert_entity_type` | `POST /v1/entity-types/upsert` | `entity-type upsert` |
| `mentisdb_recent_context` | `POST /v1/recent-context` | `thought recent` |
| `mentisdb_standing_context` | `POST /v1/standing-context` | `thought standing` |
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
| `mentisdb_get_access_policy` | `POST /v1/access/policy` | `chain access show` |
| `mentisdb_verify_signatures` | `POST /v1/access/verify` | `chain access verify` |

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
`bearertoken`, `key generate` and `key show` (Ed25519 seed files for signing), `add` (quick thought append), `agents` (alias of `agent list`).

## Operator commands (no MCP tool, REST answers 403)

Administering who may write to and read a chain, the registry of agent keys and
an agent's status is the operator's alone (design:
[CHAIN_ACCESS.md](CHAIN_ACCESS.md)). The matching REST routes
(`/v1/agents/keys`, `/v1/agents/keys/revoke`, `/v1/agents/disable`,
`/v1/access/policy/set`) answer 403 with any bearer token and the MCP tools
(`mentisdb_add_agent_key`, `mentisdb_revoke_agent_key`, `mentisdb_disable_agent`,
`mentisdb_set_access_policy`) are not offered. The operator uses:

| Operator action | CLI command | Dashboard route |
|---|---|---|
| Show the operator's view of a chain | `chain access admin` | `GET /dashboard/api/chains/{chain}/access` |
| Make a chain public or private | `chain access set public\|private\|signed` | `POST .../access` `{"op":"set_mode"}` |
| Allow / remove a key | `chain access allow`, `chain access deny` | `{"op":"allow_key"}`, `{"op":"deny_key"}` |
| Register / revoke an agent key | `agent key add`, `agent key revoke` | `{"op":"register_key"}`, `{"op":"revoke_key"}` |
| Disable / activate an agent | `agent disable`, `agent activate` | `{"op":"disable_agent"}`, `{"op":"enable_agent"}` |

They go through the dashboard behind its PIN (`--pin`, or
`$MENTISDB_DASHBOARD_PIN`), or run directly against the data directory on the
host with `--offline [--data-dir DIR]`, which refuses while a daemon holds the
directory. The operator signs policy records with the key in
`<data dir>/operator/operator.key`.

## Reading private chains

`chain access show` and `chain access verify` (REST) show what the caller may
see: a public reader gets the mode and segment boundaries, a reader who proves a
key gets the key list and every segment. Every REST command accepts
`--read-key-file FILE --read-agent ID --read-key-id ID` (or
`MENTISDB_READ_KEY_FILE`, `MENTISDB_READ_AGENT_ID`, `MENTISDB_READ_KEY_ID`) and
signs each request locally with it; the key is never sent. `thought append` and
`thought retro` sign a thought with `--private-key-file` for a chain that
requires signatures, and use that key for the head lookup too. The stdio MCP
proxy reads the same environment variables.
